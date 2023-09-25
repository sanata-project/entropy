use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::{env, thread::available_parallelism, time::Duration};

use awc::Client;
use std::thread::sleep;
use tokio_util::task::LocalPoolHandle;

fn main() {
    let pool = LocalPoolHandle::new(available_parallelism().unwrap().into());
    // let pool = LocalPoolHandle::new(1);
    let count = Arc::new(AtomicU32::new(0));
    let uri = env::args().nth(1).unwrap();
    for _ in 0..100 {
        let count = count.clone();
        let uri = uri.clone();
        pool.spawn_pinned(|| async move {
            loop {
                Client::new().get(&uri).send().await.unwrap();
                count.fetch_add(1, SeqCst);
            }
        });
    }
    sleep(Duration::from_secs(10));
    println!("{}", count.load(SeqCst));
}
