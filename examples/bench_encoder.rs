use std::time::Instant;

use rand::RngCore;
use wirehair::WirehairEncoder;

fn main() {
    let fragment_size = 4 << 20;
    let inner_k = 32;
    let mut chunk = vec![0; fragment_size * inner_k];
    rand::thread_rng().fill_bytes(&mut chunk);
    let start = Instant::now();
    let _encoder = WirehairEncoder::new(chunk, fragment_size as _);
    println!("{:?}", Instant::now() - start);
}
