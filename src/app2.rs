use std::{
    collections::{HashMap, HashSet},
    iter::repeat,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use actix_web::{
    get,
    http::StatusCode,
    post,
    web::{Bytes, Data, Json, Path},
    HttpResponse, Responder,
};
use awc::Client;
use bincode::Options;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::{sync::CancellationToken, task::LocalPoolHandle};
use wirehair::WirehairEncoder;

use crate::{chunk::ChunkKey, common::hex_string, peer, Peer};

fn fragment_id(key: &ChunkKey, index: u32) -> [u8; 32] {
    Sha256::new()
        .chain_update(key)
        .chain_update(index.to_le_bytes())
        .finalize()
        .into()
}

fn parse_key(s: &str) -> ChunkKey {
    let mut key = Vec::new();
    for i in (0..s.len()).step_by(2) {
        key.push(u8::from_str_radix(&s[i..i + 2], 16).unwrap())
    }
    key.try_into().unwrap()
}

pub struct State {
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
    benchmarks: Mutex<Vec<Arc<Mutex<StateBenchmark>>>>,
    upload_chunk_states: Arc<Mutex<HashMap<ChunkKey, UploadChunkState>>>,
    chunk_states: Arc<Mutex<HashMap<ChunkKey, ChunkState>>>,
    peer_store: Arc<Mutex<peer::Store>>,
}

#[derive(Debug, Clone)]
pub struct StateConfig {
    pub fragment_size: u32,
    pub inner_k: u32,
    pub inner_n: u32,
    pub outer_k: u32,
    pub outer_n: u32,

    pub chunk_path: PathBuf,

    pub peer: Peer,
    pub peer_secret: ed25519_dalek::SigningKey,
}

#[derive(Debug, Clone, Default, Serialize)]
struct StateBenchmark {
    object_hash: [u8; 32],
    put_start: Option<SystemTime>,
    put_end: Option<SystemTime>,
    get_start: Option<SystemTime>,
    get_end: Option<SystemTime>,
}

#[derive(Debug)]
struct UploadChunkState {
    members: HashMap<u32, ChunkMember>,
    query: mpsc::UnboundedSender<ChunkMember>,
    up: mpsc::UnboundedSender<(u32, Bytes)>,
}

#[derive(Debug)]
struct ChunkState {
    index: u32,
    members: HashMap<u32, ChunkMember>,
    has_fragment: CancellationToken,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkMember {
    index: u32,
    peer: Peer,
    proof: (),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkMembersMessage {
    members: HashMap<u32, ChunkMember>,
    uploader: Peer,
}

#[post("/benchmark/entropy")]
async fn benchmark_entropy(data: Data<State>) -> impl Responder {
    let mut benchmarks = data.benchmarks.lock().unwrap();
    let id = benchmarks.len();
    let benchmark = Arc::new(Mutex::new(StateBenchmark::default()));
    benchmarks.push(benchmark.clone());

    let upload_chunk_states = data.upload_chunk_states.clone();
    let peer_store = data.peer_store.clone();
    let config = data.config.clone();
    let pool = data.pool.clone();
    data.pool.spawn_pinned(move || {
        entropy_put(benchmark, upload_chunk_states, peer_store, config, pool)
    });
    Json(id)
}

async fn entropy_put(
    benchmark: Arc<Mutex<StateBenchmark>>,
    upload_chunk_states: Arc<Mutex<HashMap<ChunkKey, UploadChunkState>>>,
    peer_store: Arc<Mutex<peer::Store>>,
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
) {
    let object_len =
        config.fragment_size as usize * config.inner_k as usize * config.outer_k as usize;
    let mut object = vec![0; object_len];
    rand::thread_rng().fill_bytes(&mut object);
    let object_hash = Sha256::digest(&object);
    {
        let mut benchmark = benchmark.lock().unwrap();
        benchmark.object_hash = object_hash.into();
        benchmark.put_start = Some(SystemTime::now());
    }

    let chunk_encoder = Arc::new(WirehairEncoder::new(object, object_len as _));
    let upload_chunks = Vec::from_iter(
        (0..config.outer_n)
            .zip(repeat((
                upload_chunk_states,
                chunk_encoder,
                peer_store,
                config,
                pool.clone(),
            )))
            .map(
                |(
                    chunk_index,
                    (upload_chunk_states, chunk_encoder, peer_store, config, cloned_pool),
                )| {
                    pool.spawn_pinned(move || {
                        entropy_upload_chunk(
                            upload_chunk_states,
                            chunk_encoder,
                            chunk_index,
                            peer_store,
                            config,
                            cloned_pool,
                        )
                    })
                },
            ),
    );
    for upload_chunk in upload_chunks {
        upload_chunk.await.unwrap();
    }
    let mut benchmark = benchmark.lock().unwrap();
    benchmark.put_end = Some(SystemTime::now());
}

async fn entropy_upload_chunk(
    upload_chunk_states: Arc<Mutex<HashMap<ChunkKey, UploadChunkState>>>,
    chunk_encoder: Arc<WirehairEncoder>,
    chunk_index: u32,
    peer_store: Arc<Mutex<peer::Store>>,
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
) {
    let mut chunk = vec![0; config.fragment_size as usize * config.inner_k as usize];
    chunk_encoder.encode(chunk_index, &mut chunk).unwrap();
    let chunk_key = Sha256::digest(&chunk).into();
    let mut peers = HashMap::new();
    let mut peer_id_index = HashMap::new();
    for index in 0..config.inner_n * 2 {
        let peer = peer_store
            .lock()
            .unwrap()
            .closest_peers(&fragment_id(&chunk_key, index), 1)[0]
            .clone();
        if let Some(duplicated_index) = peer_id_index.insert(peer.id, index) {
            peers.remove(&duplicated_index).unwrap();
        }
        peers.insert(index, peer);
        if peers.len() == config.inner_n as usize {
            break;
        }
    }
    assert_eq!(peers.len(), config.inner_n as usize);

    let mut query = mpsc::unbounded_channel();
    let mut up = mpsc::unbounded_channel();
    let chunk_state = UploadChunkState {
        members: Default::default(),
        query: query.0,
        up: up.0,
    };
    upload_chunk_states
        .lock()
        .unwrap()
        .insert(chunk_key, chunk_state);

    let hex_key = hex_string(&chunk_key);
    let message = Bytes::from(bincode::options().serialize(&config.peer).unwrap());
    for ((index, peer), (hex_key, message)) in
        peers.into_iter().zip(repeat((hex_key.clone(), message)))
    {
        pool.spawn_pinned(move || async move {
            let mut response = Client::new()
                .post(format!(
                    "{}/entropy/upload/invite/{hex_key}/{index}",
                    peer.uri
                ))
                .send_body(message)
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{:?}",
                response.body().await
            );
        });
    }

    let encoder = Arc::new(WirehairEncoder::new(chunk, config.fragment_size));

    let members = loop {
        let member = query
            .1
            .recv()
            .await
            .expect("upload chunk quit while receiving query");
        {
            let index = member.index;
            let encoder = encoder.clone();
            let peer = member.peer.clone();
            let config = config.clone();
            pool.spawn_pinned(move || {
                entropy_push_fragment(chunk_key, index, encoder, peer, config)
            });
        }

        let mut upload_chunk_states = upload_chunk_states.lock().unwrap();
        let members = &mut upload_chunk_states.get_mut(&chunk_key).unwrap().members;
        members.insert(member.index, member);
        if members.len() == config.inner_n as usize {
            break members.clone();
        }
    };

    let message = Bytes::from(
        bincode::options()
            .serialize(&ChunkMembersMessage {
                members: members.clone(),
                uploader: config.peer.clone(),
            })
            .unwrap(),
    );
    for (member, (hex_key, message)) in members.values().zip(repeat((hex_key, message))) {
        let uri = member.peer.uri.clone();
        pool.spawn_pinned(move || async move {
            let mut response = Client::new()
                .post(format!("{uri}/entropy/upload/members/{hex_key}"))
                .send_body(message)
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{:?}",
                response.body().await
            );
        });
    }

    let mut waiting_up = HashSet::new();
    waiting_up.extend(members.keys().copied());
    while !waiting_up.is_empty() {
        let (index, _signature) =
            up.1.recv()
                .await
                .expect("upload chunk quit while receiving up");
        // TODO verify signature
        waiting_up.remove(&index);
    }
}

async fn entropy_push_fragment(
    chunk_key: [u8; 32],
    index: u32,
    encoder: Arc<WirehairEncoder>,
    peer: Peer,
    config: Arc<StateConfig>,
) {
    let mut fragment = vec![0; config.fragment_size as usize];
    encoder.encode(index, &mut fragment).unwrap();
    let hex_key = hex_string(&chunk_key);
    let mut response = Client::new()
        .post(format!("{}/entropy/upload/push/{hex_key}", peer.uri))
        .send_body(fragment)
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{:?}",
        response.body().await
    );
}

#[get("/benchmark/{index}")]
async fn poll_benchmark(data: Data<State>, index: Path<usize>) -> impl Responder {
    let benchmarks = data.benchmarks.lock().unwrap();
    let benchmark = benchmarks[index.into_inner()].lock().unwrap();
    Json(benchmark.clone())
}

#[post("/entropy/upload/query/{chunk_key}")]
async fn entropy_upload_query(
    data: Data<State>,
    chunk_key: Path<String>,
    chunk_member: Bytes,
) -> impl Responder {
    data.upload_chunk_states.lock().unwrap()[&parse_key(&chunk_key)]
        .query
        .send(bincode::options().deserialize(&chunk_member).unwrap())
        .unwrap();
    HttpResponse::Ok()
}

#[post("/entropy/upload/up/{chunk_key}/{index}")]
async fn entropy_upload_up(
    data: Data<State>,
    path: Path<(String, u32)>,
    signature: Bytes,
) -> impl Responder {
    let (chunk_key, index) = path.into_inner();
    data.upload_chunk_states.lock().unwrap()[&parse_key(&chunk_key)]
        .up
        .send((index, signature))
        .unwrap();
    HttpResponse::Ok()
}

#[post("/entropy/upload/invite/{chunk_key}/{index}")]
async fn entropy_upload_invite(
    data: Data<State>,
    path: Path<(String, u32)>,
    uploader: Bytes,
) -> impl Responder {
    let (chunk_key, index) = path.into_inner();
    let uploader = bincode::options().deserialize::<Peer>(&uploader).unwrap();
    // TODO generate proof
    let chunk_state = ChunkState {
        index,
        members: Default::default(),
        has_fragment: CancellationToken::new(),
    };
    let duplicated = data
        .chunk_states
        .lock()
        .unwrap()
        .insert(parse_key(&chunk_key), chunk_state);
    assert!(duplicated.is_none());

    let message = bincode::options()
        .serialize(&ChunkMember {
            peer: data.config.peer.clone(),
            index,
            proof: (),
        })
        .unwrap();
    data.pool.spawn_pinned(move || async move {
        let mut response = Client::new()
            .post(format!("{}/entropy/upload/query/{chunk_key}", uploader.uri))
            .send_body(message)
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{:?}",
            response.body().await
        );
    });

    HttpResponse::Ok()
}

#[post("/entropy/upload/push/{chunk_key}/{index}")]
async fn entropy_upload_push(
    data: Data<State>,
    path: Path<(String, u32)>,
    fragment: Bytes,
) -> impl Responder {
    let (chunk_key, index) = path.into_inner();
    let has_fragment = data.chunk_states.lock().unwrap()[&parse_key(&chunk_key)]
        .has_fragment
        .clone();
    let config = data.config.clone();
    data.pool.spawn_pinned(move || async move {
        tokio::fs::create_dir(config.chunk_path.join(&chunk_key))
            .await
            .unwrap();
        tokio::fs::write(
            config.chunk_path.join(&chunk_key).join(index.to_string()),
            fragment,
        )
        .await
        .unwrap();
        has_fragment.cancel();
    });
    HttpResponse::Ok()
}

#[post("/entropy/upload/members/{chunk_key}")]
async fn entropy_upload_members(
    data: Data<State>,
    chunk_key: Path<String>,
    message: Bytes,
) -> impl Responder {
    let message = bincode::options()
        .deserialize::<ChunkMembersMessage>(&message)
        .unwrap();
    let mut chunk_states = data.chunk_states.lock().unwrap();
    let chunk_state = chunk_states.get_mut(&parse_key(&chunk_key)).unwrap();
    chunk_state.members = message.members;

    let has_fragment = chunk_state.has_fragment.clone();
    let index = chunk_state.index;
    data.pool.spawn_pinned(move || async move {
        has_fragment.cancelled().await;
        // TODO spawn check repair task
        let mut response = Client::new()
            .post(format!(
                "{}/entropy/upload/up/{chunk_key}/{index}",
                message.uploader.uri,
            ))
            .send_body(Bytes::default()) // TODO signature
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{:?}",
            response.body().await
        );
    });
    HttpResponse::Ok()
}
