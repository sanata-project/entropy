use std::{future::Future, time::Duration, panic};

use actix_web::http::StatusCode;
use actix_web_opentelemetry::ClientExt;
use awc::Client;
use clap::Parser;
use ed25519_dalek::SigningKey;
use opentelemetry::{
    trace::{FutureExt, TraceContextExt, Tracer, get_active_span, Status},
    Context, KeyValue, global::shutdown_tracer_provider,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    task::{spawn_blocking, spawn_local},
    time::sleep,
};
use tokio_util::task::LocalPoolHandle;

use crate::{
    common::hex_string,
    peer::Peer,
    plaza::{News, Run},
};

#[path = "../common.rs"]
mod common;
#[path = "../peer.rs"]
mod peer;
#[path = "../plaza.rs"]
mod plaza;

#[derive(Debug, Serialize, Deserialize)]
enum Participant {
    Peer(Peer),
    BenchmarkPeer(Peer),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Shared {
    fragment_size: u32,
    inner_k: u32,
    inner_n: u32,
    outer_k: u32,
    outer_n: u32,
}

struct ReadyRun {
    participants: Vec<Participant>,
    shared: Shared,
    join_id: u32,
}

#[derive(clap::Parser, Debug, Clone)]
struct Cli {
    host: String,
    #[clap(long)]
    plaza_service: Option<usize>,
    #[clap(long)]
    plaza: Option<String>,
    #[clap(long)]
    benchmark: bool,
}

#[actix_web::main]
async fn main() {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        get_active_span(|span| {
            // span.add_event("panic", vec![KeyValue::new("display", info.to_string())]);
            span.set_status(Status::error(info.to_string()));
            span.end();
        });
        println!("shutdown tracer provider");
        shutdown_tracer_provider();
        hook(info)
    }));

    let cli = Cli::parse();
    common::setup_tracing([KeyValue::new("service.name", "entropy.benchmark-plaza")]);

    let local = LocalPoolHandle::new(32);
    let tasks = Vec::from_iter((0..300).map(|_| {
        local.spawn_pinned({
            let cli = cli.clone();
            move || async move {
                let signing_key = SigningKey::generate(&mut OsRng);
                let verifying_key = signing_key.verifying_key();
                let peer_id = Sha256::digest(verifying_key);
                println!("{}", hex_string(&peer_id));

                let listener = TcpListener::bind((&*cli.host, 0)).await.unwrap();
                let peer = Peer {
                    uri: format!(
                        "http://{}:{}",
                        cli.host,
                        listener.local_addr().unwrap().port()
                    ),
                    id: peer_id.into(),
                    key: verifying_key,
                };
                println!("{}", peer.uri);

                let run = join_network(&peer, &cli).await;
                let Some(run) = run else {
                    // spawn_blocking(common::shutdown_tracing);
                    return;
                };

                // spawn_local(poll_network(&cli));

                let peer_store =
                    peer::Store::new(Vec::from_iter(run.participants.into_iter().map(
                        |participant| match participant {
                            Participant::Peer(peer) => peer,
                            Participant::BenchmarkPeer(peer) => peer,
                        },
                    )));
                assert_eq!(peer_store.closest_peers(&peer.id, 1)[0].id, peer.id);
            }
        })
    }));

    for task in tasks {
        task.await.unwrap();
    }
    println!("READY");
    spawn_blocking(common::shutdown_tracing).await.unwrap();
}

// #[instrument(skip_all, fields(peer = common::hex_string(&peer.id)))]
async fn join_network(peer: &Peer, cli: &Cli) -> Option<ReadyRun> {
    // sleep(Duration::from_millis(
    //     rand::thread_rng().gen_range(0..10 * 1000),
    // ))
    // .await;
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
    assert_eq!(response.status(), StatusCode::OK);
    let join = response.json::<plaza::Join>().await.unwrap();

    let mut retry_interval = join.wait;
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
                    join_id: join.id,
                })
            }
        }
    }
    // println!("{response:?}");
    // run
}

// async fn leave_network(cli: &Cli, join_id: u32) {
//     let response = Client::new()
//         .post(format!("{}/leave/{join_id}", cli.plaza.as_ref().unwrap()))
//         .trace_request()
//         .send()
//         .await
//         .unwrap();
//     assert_eq!(response.status(), StatusCode::OK);
// }

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
