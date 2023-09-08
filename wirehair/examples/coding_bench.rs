use std::{collections::HashMap, time::Instant};

use rand::{random, thread_rng, RngCore};
use wirehair::{WirehairDecoder, WirehairEncoder};

const OBJECT_SIZE: usize = 1 << 30;
fn main() {
    // let outer_codings = [(4, 5), (8, 10), (12, 15)];
    // let inner_codings = [(32, 80)];
    let outer_codings = [(8, 10)];
    let inner_codings = [(16, 40), (32, 80), (48, 120)];

    println!("chunk_k,chunk_n,fragment_k,fragment_n,op,time");
    for (chunk_k, chunk_n) in outer_codings {
        for (fragment_k, fragment_n) in inner_codings {
            run(chunk_k, chunk_n, fragment_k, fragment_n);
        }
    }
}

fn run(chunk_k: usize, chunk_n: usize, fragment_k: usize, fragment_n: usize) {
    let mut data = vec![0; OBJECT_SIZE];
    thread_rng().fill_bytes(&mut data);

    let start = Instant::now();
    let mut chunks = HashMap::new();

    let outer_encoder = WirehairEncoder::new(data, (OBJECT_SIZE / chunk_k) as _);
    for _ in 0..chunk_n {
        let chunk_id = random();
        let mut chunk = vec![0; OBJECT_SIZE / chunk_k];
        outer_encoder.encode(chunk_id, &mut chunk).unwrap();
        let inner_encoder = WirehairEncoder::new(chunk, (OBJECT_SIZE / chunk_k / fragment_k) as _);
        let mut fragments = HashMap::new();
        for _ in 0..fragment_n {
            let fragment_id = random();
            let mut fragment = vec![0; OBJECT_SIZE / chunk_k / fragment_k];
            inner_encoder.encode(fragment_id, &mut fragment).unwrap();
            fragments.insert(fragment_id, fragment);
        }
        chunks.insert(chunk_id, fragments);
    }

    let encode = Instant::now() - start;
    println!(
        "{chunk_k},{chunk_n},{fragment_k},{fragment_n},encode,{}",
        encode.as_secs_f32()
    );

    let repair_chunk = chunks.values().next().unwrap().clone();

    let start = Instant::now();

    let mut outer_decoder = WirehairDecoder::new(OBJECT_SIZE as _, (OBJECT_SIZE / chunk_k) as _);
    for (chunk_id, fragments) in chunks {
        let mut inner_decoder = WirehairDecoder::new(
            (OBJECT_SIZE / chunk_k) as _,
            (OBJECT_SIZE / chunk_k / fragment_k) as _,
        );
        for (fragment_id, fragment) in fragments {
            if inner_decoder.decode(fragment_id, &fragment).unwrap() {
                break;
            }
        }
        let mut chunk = vec![0; OBJECT_SIZE / chunk_k];
        inner_decoder.recover(&mut chunk).unwrap();

        if outer_decoder.decode(chunk_id, &chunk).unwrap() {
            break;
        }
    }
    let mut object = vec![0; OBJECT_SIZE];
    outer_decoder.recover(&mut object).unwrap();

    let decode = Instant::now() - start;
    println!(
        "{chunk_k},{chunk_n},{fragment_k},{fragment_n},decode,{}",
        decode.as_secs_f32()
    );

    let start = Instant::now();
    let mut repair_decoder = WirehairDecoder::new(
        (OBJECT_SIZE / chunk_k) as _,
        (OBJECT_SIZE / chunk_k / fragment_k) as _,
    );
    for (fragment_id, fragment) in repair_chunk {
        if repair_decoder.decode(fragment_id, &fragment).unwrap() {
            break;
        }
    }
    let mut repaired_fragment = vec![0; OBJECT_SIZE / chunk_k / fragment_k];
    repair_decoder
        .into_encoder()
        .unwrap()
        .encode(random(), &mut repaired_fragment)
        .unwrap();
    let repair = Instant::now() - start;
    println!(
        "{chunk_k},{chunk_n},{fragment_k},{fragment_n},repair,{}",
        repair.as_secs_f32()
    );
}
