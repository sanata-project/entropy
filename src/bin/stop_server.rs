use tokio_util::sync::CancellationToken;

#[actix_web::main]
async fn main() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    cancel.cancelled().await;
}
