use std::{
    env::current_exe,
    time::{Duration, Instant},
};

use actix_web::{
    http::StatusCode,
    web::{Data, PayloadConfig},
    App, HttpServer,
};
use bincode::Options;
use clap::Parser;
use opentelemetry::KeyValue;
use tokio::{spawn, sync::mpsc, time::sleep};
use tokio_util::{sync::CancellationToken, task::LocalPoolHandle};

use crate::{common::hex_string, peer::Peer};

mod app;
mod common;
mod peer;
mod plaza;

#[derive(clap::Parser)]
struct Cli {
    host: String,
    #[clap(long)]
    plaza_service: Option<usize>,
    #[clap(long)]
    plaza: Option<String>,
    #[clap(long)]
    port: Option<u16>,
    #[clap(long)]
    benchmark: bool,
    #[clap(long)]
    repair: bool,
    #[clap(long)]
    num_host_peer: Option<usize>,

    #[clap(long, default_value_t = 4 << 20)]
    fragment_size: u32,
    #[clap(long, default_value_t = 32)]
    inner_k: u32,
    #[clap(long, default_value_t = 80)]
    inner_n: u32,
    #[clap(long, default_value_t = 8)]
    outer_k: u32,
    #[clap(long, default_value_t = 10)]
    outer_n: u32,
}

fn main() {
    let cli = Cli::parse();

    if let Some(num_participant) = cli.plaza_service {
        let shutdown = CancellationToken::new();
        let state = Data::new(plaza::State::new(num_participant, shutdown.clone()));
        let server = HttpServer::new(move || {
            App::new()
                .wrap(actix_web_opentelemetry::RequestTracing::new())
                .app_data(state.clone())
                .service(plaza::join)
                .service(plaza::leave)
                .service(plaza::poll_ready)
                .service(plaza::shutdown)
                .service(plaza::poll_status)
                .service(plaza::repair)
                .service(plaza::repair_finish)
        })
        .bind((cli.host, 8080))
        .unwrap()
        .run();
        let server_handle = server.handle();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                common::setup_tracing([KeyValue::new("service.name", "entropy.plaza")]);
                spawn(async move {
                    shutdown.cancelled().await;
                    server_handle.stop(true).await;
                });
                server.await.unwrap();
                tokio::task::spawn_blocking(common::shutdown_tracing)
                    .await
                    .unwrap();
            });
        return;
    }

    let port = cli.port.unwrap();
    let uri = format!("http://{}:{port}", cli.host);
    let signing_key = Peer::signing_key(&uri);
    let peer = Peer::new(uri);
    // println!("{}", peer.uri);
    // println!("{}", hex_string(&peer.id));
    let work_dir = current_exe().unwrap().parent().unwrap().to_owned();
    let peer_store = peer::Store::new(Vec::from_iter(
        std::fs::read_to_string(work_dir.join("hosts.txt"))
            .unwrap()
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .flat_map(|host| {
                let host = host.to_string();
                (0..cli.num_host_peer.unwrap())
                    .map(move |index| Peer::new(format!("http://{host}:{}", 10000 + index)))
            }),
    ));
    assert_eq!(peer_store.closest_peers(&peer.id, 1)[0].id, peer.id);

    let chunk_path = work_dir
        .join("entropy_chunk")
        .join(common::hex_string(&peer.id));
    std::fs::create_dir_all(&chunk_path).unwrap();

    let pool = LocalPoolHandle::new(if cli.benchmark {
        std::thread::available_parallelism().unwrap().into()
    } else {
        1
    });
    let config = app::StateConfig {
        fragment_size: cli.fragment_size,
        inner_k: cli.inner_k,
        inner_n: cli.inner_n,
        outer_k: cli.outer_k,
        outer_n: cli.outer_n,
        chunk_path: chunk_path.clone(),
        repair: cli.repair,
        peer: peer.clone(),
        peer_secret: signing_key,
    };
    let repair_finish = mpsc::unbounded_channel();
    let state = Data::new(app::State::new(
        config,
        pool.clone(),
        peer_store,
        repair_finish.0,
    ));
    let server = {
        let state = state.clone();
        HttpServer::new(move || {
            let state = state.clone();
            App::new()
                .wrap(actix_web_opentelemetry::RequestTracing::new())
                .configure(|config| app::State::inject(config, state))
                .app_data(PayloadConfig::new(16 << 20))
        })
    };
    let server = if !cli.benchmark {
        server.workers(1)
    } else {
        server
    }
    .bind((cli.host, port))
    .unwrap()
    .run();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            common::setup_tracing([
                KeyValue::new(
                    "service.name",
                    if cli.benchmark {
                        "entropy.benchmark-peer"
                    } else {
                        "entropy.peer"
                    },
                ),
                KeyValue::new("service.instance.id", hex_string(&peer.id)),
            ]);

            let server_handle = server.handle();
            let server = spawn(server);

            let shutdown = CancellationToken::new();
            pool.spawn_pinned({
                let plaza = common::aws_dns_to_ip(cli.plaza.clone().unwrap());
                let shutdown = shutdown.clone();
                let pool = pool.clone();
                move || plaza_session(plaza, shutdown.clone(), state, repair_finish.1, pool)
            });

            spawn(async move {
                shutdown.cancelled().await;
                server_handle.stop(true).await;
            });
            server.await.unwrap().unwrap();

            tokio::task::spawn_blocking(common::shutdown_tracing)
                .await
                .unwrap();
        });

    std::fs::remove_dir_all(&chunk_path).unwrap();
}

async fn plaza_session(
    plaza: String,
    shutdown: CancellationToken,
    state: Data<app::State>,
    repair_finish: mpsc::UnboundedReceiver<[u8; 32]>,
    pool: LocalPoolHandle,
) {
    // use rand::Rng;
    // sleep(Duration::from_millis(rand::thread_rng().gen_range(0..5000))).await;

    let client = awc::Client::new();
    let mut response = client.post(format!("{plaza}/join")).send().await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{:?}",
        response.body().await
    );

    let mut repair_finish = Some(repair_finish);
    loop {
        sleep(Duration::from_secs(1)).await;
        let pool_uri = format!("{plaza}/status");
        let status = async {
            let mut response = client.get(pool_uri).send().await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{:?}",
                response.body().await
            );
            // bincode::options()
            //     .deserialize::<plaza::PollMessage>(&response.body().await.unwrap())
            //     .unwrap()
            response.json::<plaza::PollMessage>().await.unwrap()
        };
        let status = tokio::select! {
            status = status => status,
            () = shutdown.cancelled() => break,
        };

        if status.shutdown {
            shutdown.cancel();
            break;
        }

        if status.repair {
            if let Some(mut repair_finish) = repair_finish.take() {
                let start = Instant::now();
                state.repair();
                let repair_finish_uri = format!("{plaza}/repair/finish");
                pool.spawn_pinned(move || async move {
                    let client = awc::Client::new();
                    while let Some(chunk_key) = repair_finish.recv().await {
                        client
                            .post(&repair_finish_uri)
                            .send_body(
                                bincode::options()
                                    .serialize(&plaza::RepairFinishMessage {
                                        key: chunk_key,
                                        duration: start.elapsed(),
                                    })
                                    .unwrap(),
                            )
                            .await
                            .unwrap();
                    }
                });
            }
        }
    }

    let mut response = client.post(format!("{plaza}/leave")).send().await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{:?}",
        response.body().await
    );
    // println!("{} leaved", peer.uri);
}
