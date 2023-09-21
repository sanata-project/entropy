use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

use actix_web::{get, post, web::Data, HttpResponse};
use tokio_util::sync::CancellationToken;

pub struct State {
    num_participant: usize,
    num_join: AtomicUsize,
    shutdown: AtomicBool,
    shutdown_server: CancellationToken,
}

#[post("/join")]
#[tracing::instrument(skip(data))]
pub async fn join(data: Data<State>) -> HttpResponse {
    let before = data.num_join.fetch_add(1, SeqCst);
    assert!(before < data.num_participant);
    if before + 1 == data.num_participant {
        println!("ready");
    }
    HttpResponse::Ok().finish()
}

#[post("/leave")]
#[tracing::instrument(skip(data))]
pub async fn leave(data: Data<State>) -> HttpResponse {
    let before = data.num_join.fetch_sub(1, SeqCst);
    assert!(before > 0);
    if before == 1 && data.shutdown.load(SeqCst) {
        data.shutdown_server.cancel();
    }
    HttpResponse::Ok().finish()
}

#[get("/ready")]
#[tracing::instrument(skip(data))]
pub async fn poll_ready(data: Data<State>) -> HttpResponse {
    HttpResponse::Ok().json(data.num_join.load(SeqCst) == data.num_participant)
}

#[post("/shutdown")]
#[tracing::instrument(skip(data))]
pub async fn shutdown(data: Data<State>) -> HttpResponse {
    data.shutdown.store(true, SeqCst);
    if data.num_join.load(SeqCst) == 0 {
        data.shutdown_server.cancel();
    }
    HttpResponse::Ok().finish()
}

#[get("/shutdown")]
#[tracing::instrument(skip(data))]
pub async fn poll_shutdown(data: Data<State>) -> HttpResponse {
    HttpResponse::Ok().json(data.shutdown.load(SeqCst))
}

impl State {
    pub fn new(num_participant: usize, shutdown_server: CancellationToken) -> Self {
        Self {
            num_participant,
            num_join: Default::default(),
            shutdown: Default::default(),
            shutdown_server,
        }
    }
}
