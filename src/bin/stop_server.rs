use std::time::Instant;

#[path = "../chunk.rs"]
mod chunk;
#[path = "../common.rs"]
mod common;

#[actix_web::main]
async fn main() {
    let store = chunk::Store::new(".".into(), 4 << 20, 32);
    let fragment = vec![0; 4 << 20];
    let start = Instant::now();
    store.put_fragment(&[0; 32], 0, fragment).await;
    println!("{:?}", Instant::now() - start);
}
