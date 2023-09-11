use std::{future::Future, panic::panic_any, path::PathBuf, time::Duration};

use actix_web::{http::StatusCode, App, HttpServer};
use actix_web_opentelemetry::ClientExt;
use awc::Client;
use clap::Parser;
use common::hex_string;
use ed25519_dalek::SigningKey;
use opentelemetry::{
    trace::{FutureExt, TraceContextExt, Tracer},
    Context, KeyValue,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    spawn,
    sync::mpsc,
    task::{spawn_blocking, spawn_local},
    time::{sleep, timeout},
};

use crate::{
    peer::Peer,
    plaza::{News, Run},
};

mod app;
mod chunk;
mod common;
mod peer;
mod plaza;

#[derive(Debug, Serialize, Deserialize)]
enum Participant {
    Peer(Peer),
    BenchmarkPeer(Peer),
}

#[derive(clap::Parser)]
struct Cli {
    host: String,
    #[clap(long)]
    plaza_service: Option<usize>,
    #[clap(long)]
    plaza: Option<String>,
    #[clap(long)]
    benchmark: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Shared {
    fragment_size: u32,
    inner_k: u32,
    inner_n: u32,
    outer_k: u32,
    outer_n: u32,
    chunk_root: PathBuf,
}

struct ReadyRun {
    participants: Vec<Participant>,
    shared: Shared,
    join_id: u32,
}

#[actix_web::main]
async fn main() {
    let cli = Cli::parse();

    if let Some(expect_number) = cli.plaza_service {
        common::setup_tracing([KeyValue::new("service.name", "entropy.plaza")]);

        let mut shutdown = mpsc::unbounded_channel();
        let (run, configure) = plaza::State::spawn::<Participant>(
            expect_number,
            Shared {
                fragment_size: 4 << 20,
                inner_k: 4,
                inner_n: 4,
                outer_k: 10,
                outer_n: 10,
                chunk_root: "/local/cowsay/artifacts/entropy_chunk".into(),
            },
            shutdown.0,
        );
        let state_handle = spawn(run);
        let server = HttpServer::new(move || {
            App::new()
                .wrap(actix_web_opentelemetry::RequestTracing::new())
                .configure(configure.clone())
        })
        .workers(1)
        .bind((cli.host, 8080))
        .unwrap()
        .run();
        let server_handle = server.handle();
        spawn(async move {
            let _ = shutdown.1.recv().await;
            server_handle.stop(true).await;
        });

        server.await.unwrap();
        state_handle.await.unwrap(); // inspect returned state if necessary
        common::shutdown_tracing().await;
        return;
    }

    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let peer_id = Sha256::digest(verifying_key);
    println!("{}", hex_string(&peer_id));
    common::setup_tracing([
        KeyValue::new("service.name", "entropy.peer"),
        KeyValue::new("service.instance.id", hex_string(&peer_id)),
    ]);

    let listener = TcpListener::bind((&*cli.host, 0)).await.unwrap();
    let peer = Peer {
        uri: format!("http://{}", listener.local_addr().unwrap()),
        id: peer_id.into(),
        key: verifying_key,
    };
    println!("{}", peer.uri);

    let run = join_network(&peer, &cli)
        .with_context(Context::current_with_span(
            opentelemetry::global::tracer("").start("join network"),
        ))
        .await;
    let Some(run) = run else {
        common::shutdown_tracing().await;
        return;
    };

    let poll_handle = spawn_local(poll_network(&cli).with_context(Context::current_with_span(
        opentelemetry::global::tracer("").start("poll network"),
    )));

    let peer_store = peer::Store::new(Vec::from_iter(run.participants.into_iter().map(
        |participant| match participant {
            Participant::Peer(peer) => peer,
            Participant::BenchmarkPeer(peer) => peer,
        },
    )));
    assert_eq!(peer_store.closest_peers(&peer.id, 1)[0].id, peer.id);

    let chunk_path = run.shared.chunk_root.join(common::hex_string(&peer.id));
    tokio::fs::create_dir_all(&chunk_path).await.unwrap();
    let chunk_store = chunk::Store::new(
        chunk_path.clone(),
        run.shared.fragment_size,
        run.shared.inner_k,
    );

    let (run_app, configuration) = app::State::spawn(
        peer,
        signing_key,
        run.shared.fragment_size,
        run.shared.inner_n,
        run.shared.inner_k,
        run.shared.outer_n,
        run.shared.outer_k,
        peer_store,
        chunk_store,
    );
    let app_runtime = if cli.benchmark {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(32)
            .build()
            .unwrap()
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    };
    let app_handle = spawn_blocking(move || app_runtime.block_on(run_app));

    let server = HttpServer::new(move || {
        App::new()
            .wrap(actix_web_opentelemetry::RequestTracing::new())
            .configure(configuration.clone())
    })
    .workers(1)
    .listen(listener.into_std().unwrap())
    .unwrap()
    .run();
    let server_handle = server.handle();
    spawn(async move {
        poll_handle.await.unwrap();
        // this rarely stucks, not sure why
        let _ = timeout(Duration::from_secs(1), server_handle.stop(false)).await;
    });

    println!("READY");
    server.await.unwrap();

    app_handle.await.unwrap();
    tokio::fs::remove_dir_all(&chunk_path).await.unwrap();
    leave_network(&cli, run.join_id).await;
    common::shutdown_tracing().await;
}

// #[instrument(skip_all, fields(peer = common::hex_string(&peer.id)))]
async fn join_network(peer: &Peer, cli: &Cli) -> Option<ReadyRun> {
    let client = Client::new();
    let mut response = client
        .post(format!("{}/join", cli.plaza.as_ref().unwrap()))
        .trace_request()
        .send_json(&if cli.benchmark {
            Participant::BenchmarkPeer(peer.clone())
        } else {
            Participant::Peer(peer.clone())
        })
        .await
        .unwrap();
    if response.status() != StatusCode::OK {
        panic_any(String::from_utf8(response.body().await.unwrap().to_vec()).unwrap());
    }
    let join_id = response.json::<serde_json::Value>().await.unwrap()["id"]
        .to_string()
        .parse()
        .unwrap();

    let mut retry_interval = Duration::ZERO;
    loop {
        sleep(retry_interval).await;
        match client
            .get(format!("{}/run", cli.plaza.as_ref().unwrap()))
            .trace_request()
            .send()
            .await
            .unwrap()
            .json::<plaza::Run<Participant, Shared>>()
            .await
            .unwrap()
        {
            Run::Retry(interval) => retry_interval = interval,
            Run::Shutdown => break None,
            Run::Ready {
                participants,
                assemble_time: _,
                shared,
            } => {
                break Some(ReadyRun {
                    participants,
                    shared,
                    join_id,
                })
            }
        }
    }
    // println!("{response:?}");
    // run
}

async fn leave_network(cli: &Cli, join_id: u32) {
    let client = Client::new();
    let response = client
        .post(format!("{}/leave/{join_id}", cli.plaza.as_ref().unwrap()))
        .trace_request()
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

fn poll_network(cli: &Cli) -> impl Future<Output = ()> {
    let endpoint = format!("{}/news", cli.plaza.as_ref().unwrap());
    async move {
        let client = Client::new();
        let mut wait = Duration::from_secs(1);
        loop {
            sleep(wait).await; //
            let news = client
                .get(&endpoint)
                .trace_request()
                .send()
                .await
                .unwrap()
                .json::<News>()
                .await
                .unwrap();
            tracing::info!(shutdown = news.shutdown, "news");
            if news.shutdown {
                break;
            }
            wait = news.wait;
        }
    }
}
