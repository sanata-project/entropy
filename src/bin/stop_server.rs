use std::time::Duration;

use actix_web::{App, HttpServer};
use tokio::{spawn, time::sleep};

#[actix_web::main]
async fn main() {
    let server = HttpServer::new(App::new)
        .bind(("127.0.0.1", 8080))
        .unwrap()
        .run();
    let server_handle = server.handle();
    spawn(async move { server_handle.stop(true).await });
    sleep(Duration::from_secs(1)).await;
    // server_handle.stop(true).await;
    server.await.unwrap();
}
