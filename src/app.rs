use std::{
    collections::{BTreeMap, HashMap, HashSet},
    iter::repeat,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use actix_web::{
    get,
    http::StatusCode,
    post,
    web::{Bytes, Data, Json, Path, ServiceConfig},
    HttpResponse, Responder,
};
use actix_web_opentelemetry::ClientExt;
use awc::Client;
use bincode::Options;
use opentelemetry::trace::FutureExt;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    sync::mpsc::{self, UnboundedReceiver},
    task::JoinSet,
};
use tokio_util::{sync::CancellationToken, task::LocalPoolHandle};
use wirehair::{WirehairDecoder, WirehairEncoder};

use crate::{common::hex_string, peer, Peer};

type ChunkKey = [u8; 32];

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
    parts: Arc<Mutex<HashSet<[u8; 32]>>>,
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
    pub repair: bool,

    pub peer: Peer,
    pub peer_secret: ed25519_dalek::SigningKey,
}

impl StateConfig {
    fn object_size(&self) -> u64 {
        self.chunk_size() as u64 * self.outer_k as u64
    }

    fn chunk_size(&self) -> u32 {
        self.fragment_size * self.inner_k
    }
}

#[derive(Debug, Clone, Default, Serialize)]
struct StateBenchmark {
    object_hash: [u8; 32],
    put_start: Option<SystemTime>,
    put_end: Option<SystemTime>,
    get_start: Option<SystemTime>,
    get_end: Option<SystemTime>,
    #[serde(skip)]
    chunk_keys: Vec<ChunkKey>,
    #[serde(skip)]
    part_ids: HashMap<u32, [u8; 32]>,
}

#[derive(Debug, Clone)]
struct UploadChunkState {
    chunk_index: u32,
    members: BTreeMap<u32, ChunkMember>,
    query: mpsc::UnboundedSender<ChunkMember>,
    up: mpsc::UnboundedSender<(u32, Bytes)>,
}

#[derive(Debug)]
struct ChunkState {
    index: u32,
    members: BTreeMap<u32, ChunkMember>,
    has_fragment: CancellationToken,
    repair: mpsc::UnboundedSender<(u32, Bytes)>,
    repair_finish: Option<mpsc::UnboundedSender<[u8; 32]>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkMember {
    index: u32,
    peer: Peer,
    proof: (),
}

impl State {
    pub fn new(config: StateConfig, pool: LocalPoolHandle, peer_store: peer::Store) -> Self {
        Self {
            config: Arc::new(config),
            pool,
            benchmarks: Default::default(),
            upload_chunk_states: Default::default(),
            chunk_states: Default::default(),
            parts: Default::default(),
            peer_store: Arc::new(Mutex::new(peer_store)),
        }
    }

    pub fn inject(config: &mut ServiceConfig, state: Data<Self>) {
        config
            .app_data(state)
            .service(poll_benchmark)
            .service(benchmark_put)
            .service(benchmark_get)
            .service(entropy_upload_invite)
            .service(entropy_upload_query)
            .service(entropy_upload_push)
            .service(entropy_upload_members)
            .service(entropy_upload_up)
            .service(entropy_download_pull)
            .service(entropy_repair_invite)
            .service(entropy_repair_query)
            .service(entropy_repair_push)
            .service(entropy_repair_up)
            .service(kademlia_upload_push)
            .service(kademlia_download_pull)
            .service(kademlia_repair_push);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkMembersMessage {
    members: BTreeMap<u32, ChunkMember>,
    uploader: Peer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RepairUpMessage {
    members: BTreeMap<u32, ChunkMember>,
    index: u32,
    time: SystemTime,
    signature: (),
}

fn spawn<F: std::future::Future<Output = T> + 'static, T: Send + 'static>(
    pool: &LocalPoolHandle,
    create: impl FnOnce() -> F + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    let context = opentelemetry::Context::current();
    pool.spawn_pinned(move || create().with_context(context))
}

#[post("/benchmark/{protocol}")]
async fn benchmark_put(data: Data<State>, protocol: Path<String>) -> impl Responder {
    let mut benchmarks = data.benchmarks.lock().unwrap();
    let id = benchmarks.len();
    let benchmark = Arc::new(Mutex::new(StateBenchmark::default()));
    benchmarks.push(benchmark.clone());

    let peer_store = data.peer_store.clone();
    let config = data.config.clone();
    let pool = data.pool.clone();
    match &**protocol {
        "entropy" => {
            let upload_chunk_states = data.upload_chunk_states.clone();
            spawn(&data.pool, move || {
                entropy_put(benchmark, upload_chunk_states, peer_store, config, pool)
            });
        }
        "kademlia" => {
            spawn(&data.pool, move || {
                kademlia_put(benchmark, peer_store, config, pool)
            });
        }
        _ => unimplemented!(),
    }

    Json(id)
}

#[post("/benchmark/{index}/{protocol}/get")]
async fn benchmark_get(data: Data<State>, path: Path<(usize, String)>) -> impl Responder {
    let (index, protocol) = path.into_inner();
    let benchmarks = data.benchmarks.lock().unwrap();
    let benchmark = benchmarks[index].clone();
    let pool = data.pool.clone();
    match &*protocol {
        "entropy" => {
            let upload_chunk_states = data.upload_chunk_states.clone();
            let config = data.config.clone();
            spawn(&data.pool, move || {
                entropy_get(benchmark, upload_chunk_states, config, pool)
            })
        }
        "kademlia" => {
            let peer_store = data.peer_store.clone();
            spawn(&data.pool, move || {
                kademlia_get(benchmark, peer_store, pool)
            })
        }
        _ => unimplemented!(),
    };
    HttpResponse::Ok()
}

#[get("/benchmark/{index}")]
async fn poll_benchmark(data: Data<State>, index: Path<usize>) -> impl Responder {
    let benchmarks = data.benchmarks.lock().unwrap();
    let benchmark = benchmarks[index.into_inner()].lock().unwrap();
    Json(benchmark.clone())
}

async fn entropy_put(
    benchmark: Arc<Mutex<StateBenchmark>>,
    upload_chunk_states: Arc<Mutex<HashMap<ChunkKey, UploadChunkState>>>,
    peer_store: Arc<Mutex<peer::Store>>,
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
) {
    let mut object = vec![0; config.object_size() as usize];
    rand::thread_rng().fill_bytes(&mut object);
    let object_hash = Sha256::digest(&object);
    {
        let mut benchmark = benchmark.lock().unwrap();
        benchmark.object_hash = object_hash.into();
        benchmark.put_start = Some(SystemTime::now());
    }

    let chunk_encoder = Arc::new(WirehairEncoder::new(object, config.chunk_size()));
    let upload_chunks = Vec::from_iter(
        (0..config.outer_n)
            .zip(repeat((
                benchmark.clone(),
                upload_chunk_states,
                chunk_encoder,
                peer_store,
                config,
                pool.clone(),
            )))
            .map(
                |(
                    chunk_index,
                    (
                        benchmark,
                        upload_chunk_states,
                        chunk_encoder,
                        peer_store,
                        config,
                        cloned_pool,
                    ),
                )| {
                    spawn(&pool, move || {
                        entropy_upload_chunk(
                            benchmark,
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
    benchmark: Arc<Mutex<StateBenchmark>>,
    upload_chunk_states: Arc<Mutex<HashMap<ChunkKey, UploadChunkState>>>,
    chunk_encoder: Arc<WirehairEncoder>,
    chunk_index: u32,
    peer_store: Arc<Mutex<peer::Store>>,
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
) {
    let mut chunk = vec![0; config.chunk_size() as usize];
    chunk_encoder.encode(chunk_index, &mut chunk).unwrap();
    let chunk_key = Sha256::digest(&chunk).into();
    benchmark.lock().unwrap().chunk_keys.push(chunk_key);

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
        chunk_index,
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
        spawn(&pool, move || async move {
            let mut response = Client::new()
                .post(format!(
                    "{}/entropy/upload/invite/{hex_key}/{index}",
                    peer.uri
                ))
                .trace_request()
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
            spawn(&pool, move || {
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
        spawn(&pool, move || async move {
            let mut response = Client::new()
                .post(format!("{uri}/entropy/upload/members/{hex_key}"))
                .trace_request()
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
    let mut response = Client::builder()
        .disable_timeout()
        .finish()
        .post(format!(
            "{}/entropy/upload/push/{hex_key}/{index}",
            peer.uri
        ))
        .trace_request()
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
        repair: mpsc::unbounded_channel().0,
        repair_finish: None,
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
    spawn(&data.pool, move || async move {
        let mut response = Client::new()
            .post(format!("{}/entropy/upload/query/{chunk_key}", uploader.uri))
            .trace_request()
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
    spawn(&data.pool, move || async move {
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
    spawn(&data.pool, move || async move {
        has_fragment.cancelled().await;
        // TODO spawn check repair task
        let mut response = Client::new()
            .post(format!(
                "{}/entropy/upload/up/{chunk_key}/{index}",
                message.uploader.uri,
            ))
            .trace_request()
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

async fn entropy_get(
    benchmark: Arc<Mutex<StateBenchmark>>,
    upload_chunk_states: Arc<Mutex<HashMap<ChunkKey, UploadChunkState>>>,
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
) {
    let download_chunks = {
        let mut benchmark = benchmark.lock().unwrap();
        assert!(benchmark.put_end.is_some());
        assert!(benchmark.get_start.is_none());
        benchmark.get_start = Some(SystemTime::now());
        Vec::from_iter(
            benchmark
                .chunk_keys
                .iter()
                .zip(repeat((upload_chunk_states, config.clone(), pool.clone())))
                .map(|(&chunk_key, (upload_chunk_states, config, cloned_pool))| {
                    spawn(&pool, move || {
                        entropy_download_chunk(chunk_key, upload_chunk_states, config, cloned_pool)
                    })
                }),
        )
    };
    let mut download_chunks_set = JoinSet::new();
    for download_chunk in download_chunks {
        download_chunks_set.spawn(download_chunk);
    }

    let mut object_decoder = WirehairDecoder::new(config.object_size(), config.chunk_size());
    while {
        let (chunk_index, chunk) = download_chunks_set
            .join_next()
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        !object_decoder.decode(chunk_index, &chunk).unwrap()
    } {}
    let mut object = vec![0; config.object_size() as usize];
    object_decoder.recover(&mut object).unwrap();

    let mut benchmark = benchmark.lock().unwrap();
    benchmark.get_end = Some(SystemTime::now());
    let object_hash = benchmark.object_hash;
    drop(benchmark);
    assert_eq!(Sha256::digest(object), object_hash.into());
}

async fn entropy_download_chunk(
    chunk_key: ChunkKey,
    upload_chunk_states: Arc<Mutex<HashMap<ChunkKey, UploadChunkState>>>,
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
) -> (u32, Vec<u8>) {
    let upload_chunk_state = upload_chunk_states.lock().unwrap()[&chunk_key].clone();
    let pull_fragments = Vec::from_iter(
        upload_chunk_state
            .members
            .into_values()
            .zip(repeat(hex_string(&chunk_key)))
            .map(|(member, hex_key)| {
                spawn(&pool, move || async move {
                    let mut response = Client::new()
                        .get(format!(
                            "{}/entropy/download/pull/{hex_key}/{}",
                            member.peer.uri, member.index
                        ))
                        .trace_request()
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::OK,
                        "{:?}",
                        response.body().await
                    );
                    (member.index, response.body().limit(16 << 20).await.unwrap())
                })
            }),
    );
    let mut pull_fragments_set = JoinSet::new();
    for pull_fragment in pull_fragments {
        pull_fragments_set.spawn(pull_fragment);
    }

    let mut decoder = WirehairDecoder::new(config.chunk_size() as _, config.fragment_size);
    while {
        let (index, fragment) = pull_fragments_set
            .join_next()
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        !decoder.decode(index, &fragment).unwrap()
    } {}
    let mut chunk = vec![0; config.chunk_size() as usize];
    decoder.recover(&mut chunk).unwrap();
    (upload_chunk_state.chunk_index, chunk)
}

#[get("/entropy/download/pull/{chunk_key}/{index}")]
async fn entropy_download_pull(data: Data<State>, path: Path<(String, u32)>) -> impl Responder {
    let (chunk_key, index) = path.into_inner();
    {
        let chunk_states = data.chunk_states.lock().unwrap();
        let chunk_state = &chunk_states[&parse_key(&chunk_key)];
        assert_eq!(chunk_state.index, index);
        assert!(chunk_state.has_fragment.is_cancelled());
    }
    let path = data
        .config
        .chunk_path
        .join(chunk_key)
        .join(index.to_string());
    let fragment = tokio::fs::read(&path).await.unwrap();
    if !data.config.repair {
        tokio::fs::remove_file(&path).await.unwrap();
    }
    fragment
}

async fn kademlia_put(
    benchmark: Arc<Mutex<StateBenchmark>>,
    peer_store: Arc<Mutex<peer::Store>>,
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
) {
    let mut object = vec![0; config.object_size() as usize];
    rand::thread_rng().fill_bytes(&mut object);
    let object_hash = Sha256::digest(&object);
    {
        let mut benchmark = benchmark.lock().unwrap();
        benchmark.object_hash = object_hash.into();
        benchmark.put_start = Some(SystemTime::now());
    }

    let object = Bytes::from(object);
    let upload_parts = Vec::from_iter(
        (0..config.inner_k * config.outer_k)
            .zip(repeat((peer_store, benchmark.clone())))
            .map(|(part_index, (peer_store, benchmark))| {
                let part = object.slice(
                    (part_index * config.fragment_size) as usize
                        ..((part_index + 1) * config.fragment_size) as usize,
                );
                spawn(&pool, move || async move {
                    let part_id = Sha256::digest(&part).into();
                    benchmark
                        .lock()
                        .unwrap()
                        .part_ids
                        .insert(part_index, part_id);
                    let peers = Vec::from_iter(
                        peer_store
                            .lock()
                            .unwrap()
                            .closest_peers(&part_id, 3)
                            .into_iter()
                            .cloned(),
                    );
                    for peer in peers {
                        let mut response = Client::builder()
                            .disable_timeout()
                            .finish()
                            .post(format!(
                                "{}/kademlia/upload/push/{}",
                                peer.uri,
                                hex_string(&part_id)
                            ))
                            .send_body(part.clone())
                            .await
                            .unwrap();
                        assert_eq!(
                            response.status(),
                            StatusCode::OK,
                            "{:?}",
                            response.body().await
                        );
                    }
                })
            }),
    );
    for upload_part in upload_parts {
        upload_part.await.unwrap();
    }
    let mut benchmark = benchmark.lock().unwrap();
    benchmark.put_end = Some(SystemTime::now());
}

#[post("/kademlia/upload/push/{part_id}")]
async fn kademlia_upload_push(
    data: Data<State>,
    part_id: Path<String>,
    part: Bytes,
) -> impl Responder {
    data.parts.lock().unwrap().insert(parse_key(&part_id));
    tokio::fs::write(data.config.chunk_path.join(part_id.into_inner()), part)
        .await
        .unwrap();
    HttpResponse::Ok()
}

async fn kademlia_get(
    benchmark: Arc<Mutex<StateBenchmark>>,
    peer_store: Arc<Mutex<peer::Store>>,
    pool: LocalPoolHandle,
) {
    let download_parts = {
        let mut benchmark = benchmark.lock().unwrap();
        assert!(benchmark.put_end.is_some());
        assert!(benchmark.get_start.is_none());
        benchmark.get_start = Some(SystemTime::now());
        Vec::from_iter(
            benchmark
                .part_ids
                .iter()
                .zip(repeat((peer_store, pool.clone())))
                .map(|((&part_index, &part_id), (peer_store, cloned_pool))| {
                    spawn(&pool, move || async move {
                        (
                            part_index,
                            kademlia_download_part(part_id, peer_store, cloned_pool).await,
                        )
                    })
                }),
        )
    };
    let mut parts = BTreeMap::new();
    for download_part in download_parts {
        let (index, part) = download_part.await.unwrap();
        parts.insert(index, part);
    }
    let mut object = Vec::new();
    for part in parts.into_values() {
        object.extend(part);
    }
    let mut benchmark = benchmark.lock().unwrap();
    benchmark.get_end = Some(SystemTime::now());
    let object_hash = benchmark.object_hash;
    drop(benchmark);
    assert_eq!(Sha256::digest(object), object_hash.into());
}

async fn kademlia_download_part(
    part_id: [u8; 32],
    peer_store: Arc<Mutex<peer::Store>>,
    pool: LocalPoolHandle,
) -> Bytes {
    let hex_id = hex_string(&part_id);
    let pull_parts = Vec::from_iter(
        peer_store
            .lock()
            .unwrap()
            .closest_peers(&part_id, 3)
            .into_iter()
            .cloned()
            .zip(repeat(hex_id))
            .map(|(peer, hex_id)| {
                spawn(&pool, move || async move {
                    let mut response = Client::new()
                        .get(format!("{}/kademlia/download/pull/{hex_id}", peer.uri))
                        .trace_request()
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::OK,
                        "{:?}",
                        response.body().await
                    );
                    response.body().limit(16 << 20).await.unwrap()
                })
            }),
    );
    let mut pull_parts_set = JoinSet::new();
    for pull_part in pull_parts {
        pull_parts_set.spawn(pull_part);
    }
    pull_parts_set.join_next().await.unwrap().unwrap().unwrap()
}

#[get("/kademlia/download/pull/{part_id}")]
async fn kademlia_download_pull(data: Data<State>, part_id: Path<String>) -> impl Responder {
    let path = data.config.chunk_path.join(part_id.into_inner());
    let part = tokio::fs::read(&path).await.unwrap();
    if !data.config.repair {
        tokio::fs::remove_file(&path).await.unwrap();
    }
    part
}

impl State {
    pub fn repair(&self, repair_epoch: u32, repair_finish: mpsc::UnboundedSender<[u8; 32]>) {
        let mut chunk_states = self.chunk_states.lock().unwrap();
        for chunk_key in Vec::from_iter(chunk_states.keys().copied()) {
            let chunk_state = chunk_states.get_mut(&chunk_key).unwrap();
            let (evicted, _) = chunk_state.members.pop_first().unwrap();
            if evicted == chunk_state.index {
                // tokio::fs::remove_file(
                //     self.config
                //         .chunk_path
                //         .join(hex_string(&chunk_key))
                //         .join(chunk_state.index.to_string()),
                // )
                // .await
                // .unwrap();
                chunk_states.remove(&chunk_key).unwrap();
                continue;
            }
            if chunk_state.index == *chunk_state.members.last_key_value().unwrap().0 {
                chunk_state.repair_finish = Some(repair_finish.clone());
                let mut peer;
                let mut index = chunk_state.index + 1;
                loop {
                    peer = self
                        .peer_store
                        .lock()
                        .unwrap()
                        .closest_peers(&fragment_id(&chunk_key, index), 1)[0]
                        .clone();
                    if !chunk_state
                        .members
                        .values()
                        .any(|member| member.peer.id == peer.id)
                    {
                        break;
                    }
                    index += 1;
                }

                let hex_key = hex_string(&chunk_key);
                let message = bincode::options().serialize(&chunk_state.members).unwrap();
                spawn(&self.pool, move || async move {
                    let mut response = Client::new()
                        .post(format!(
                            "{}/entropy/repair/invite/{hex_key}/{index}",
                            peer.uri
                        ))
                        .trace_request()
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
        }
        drop(chunk_states);

        let mut parts = self.parts.lock().unwrap();
        for part_id in parts.clone() {
            let peers = Vec::from_iter(
                self.peer_store
                    .lock()
                    .unwrap()
                    .closest_peers(&part_id, (repair_epoch + 4) as usize)
                    .into_iter()
                    .skip(repair_epoch as usize)
                    .cloned(),
            );
            assert!(peers.iter().any(|peer| peer.id == self.config.peer.id));
            if peers[0].id == self.config.peer.id {
                parts.remove(&part_id);
                let config = self.config.clone();
                let repair_finish = repair_finish.clone();
                spawn(&self.pool, move || async move {
                    let hex_id = hex_string(&part_id);
                    let path = config.chunk_path.join(&hex_id);
                    let part = tokio::fs::read(&path).await.unwrap();
                    tokio::fs::remove_file(&path).await.unwrap();
                    let mut response = Client::builder()
                        .disable_timeout()
                        .finish()
                        .post(format!(
                            "{}/kademlia/repair/push/{hex_id}",
                            peers.last().unwrap().uri
                        ))
                        .trace_request()
                        .send_body(part)
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::OK,
                        "{:?}",
                        response.body().await
                    );
                    repair_finish.send(part_id).unwrap();
                });
            }
        }
    }
}

#[post("/entropy/repair/invite/{chunk_key}/{index}")]
async fn entropy_repair_invite(
    data: Data<State>,
    path: Path<(String, u32)>,
    members: Bytes,
) -> impl Responder {
    let (chunk_key, index) = path.into_inner();
    let members = bincode::options()
        .deserialize::<BTreeMap<u32, ChunkMember>>(&members)
        .unwrap();
    // TODO generate proof

    let repair = mpsc::unbounded_channel();
    let chunk_state = ChunkState {
        index,
        members: members.clone(),
        has_fragment: CancellationToken::new(),
        repair: repair.0,
        repair_finish: None,
    };
    let duplicated = data
        .chunk_states
        .lock()
        .unwrap()
        .insert(parse_key(&chunk_key), chunk_state);
    assert!(duplicated.is_none());

    let local_member = ChunkMember {
        peer: data.config.peer.clone(),
        index,
        proof: (),
    };
    let message = Bytes::from(bincode::options().serialize(&local_member).unwrap());
    for (member, (chunk_key, message)) in members
        .into_values()
        .zip(repeat((chunk_key.clone(), message)))
    {
        spawn(&data.pool, move || async move {
            let mut response = Client::new()
                .post(format!(
                    "{}/entropy/repair/query/{chunk_key}",
                    member.peer.uri
                ))
                .trace_request()
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

    spawn(&data.pool, {
        let chunk_states = data.chunk_states.clone();
        let config = data.config.clone();
        let pool = data.pool.clone();
        move || {
            entropy_repair(
                parse_key(&chunk_key),
                local_member,
                chunk_states,
                repair.1,
                config,
                pool,
            )
        }
    });

    HttpResponse::Ok()
}

async fn entropy_repair(
    chunk_key: [u8; 32],
    member: ChunkMember,
    chunk_states: Arc<Mutex<HashMap<ChunkKey, ChunkState>>>,
    mut repair: UnboundedReceiver<(u32, Bytes)>,
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
) {
    let mut decoder = WirehairDecoder::new(config.chunk_size() as _, config.fragment_size);
    while {
        let (index, fragment) = repair.recv().await.unwrap();
        !decoder.decode(index, &fragment).unwrap()
    } {}
    let mut fragment = vec![0; config.fragment_size as usize];
    decoder
        .into_encoder()
        .unwrap()
        .encode(member.index, &mut fragment)
        .unwrap();

    let hex_key = hex_string(&chunk_key);
    tokio::fs::create_dir(config.chunk_path.join(&hex_key))
        .await
        .unwrap();
    tokio::fs::write(
        config
            .chunk_path
            .join(&hex_key)
            .join(member.index.to_string()),
        fragment,
    )
    .await
    .unwrap();

    let mut chunk_states = chunk_states.lock().unwrap();
    let chunk_state = chunk_states.get_mut(&chunk_key).unwrap();
    chunk_state.has_fragment.cancel();
    let members = chunk_state.members.clone();
    chunk_state.members.insert(member.index, member);

    let message_bytes = Bytes::from(
        bincode::options()
            .serialize(
                &(RepairUpMessage {
                    members: chunk_state.members.clone(),
                    index: chunk_state.index,
                    time: SystemTime::now(),
                    signature: (),
                }),
            )
            .unwrap(),
    );
    drop(chunk_states);

    for (member, (message, hex_key)) in members
        .into_values()
        .zip(repeat((message_bytes, hex_string(&chunk_key))))
    {
        spawn(&pool, move || async move {
            let mut response = Client::new()
                .post(format!("{}/entropy/repair/up/{hex_key}", member.peer.uri))
                .trace_request()
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
}

#[post("/entropy/repair/query/{chunk_key}")]
async fn entropy_repair_query(
    data: Data<State>,
    chunk_key: Path<String>,
    member: Bytes,
) -> impl Responder {
    let chunk_key = chunk_key.into_inner();
    let member = bincode::options()
        .deserialize::<ChunkMember>(&member)
        .unwrap();
    // TODO verify proof
    let index = {
        let chunk_states = data.chunk_states.lock().unwrap();
        let chunk_state = &chunk_states[&parse_key(&chunk_key)];
        assert!(chunk_state.has_fragment.is_cancelled());
        chunk_state.index
    };
    let path = data
        .config
        .chunk_path
        .join(&chunk_key)
        .join(index.to_string());
    spawn(&data.pool, move || async move {
        let fragment = tokio::fs::read(&path).await.unwrap();
        let mut response = Client::builder()
            .disable_timeout()
            .finish()
            .post(format!(
                "{}/entropy/repair/push/{chunk_key}/{index}",
                member.peer.uri
            ))
            .trace_request()
            .send_body(fragment)
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

#[post("/entropy/repair/push/{chunk_key}/{index}")]
async fn entropy_repair_push(
    data: Data<State>,
    path: Path<(String, u32)>,
    fragment: Bytes,
) -> impl Responder {
    let (chunk_key, index) = path.into_inner();
    let disconnected = data.chunk_states.lock().unwrap()[&parse_key(&chunk_key)]
        .repair
        .send((index, fragment))
        .is_err();
    if disconnected {
        //
    }
    HttpResponse::Ok()
}

#[post("/entropy/repair/up/{chunk_key}")]
async fn entropy_repair_up(
    data: Data<State>,
    chunk_key: Path<String>,
    message: Bytes,
) -> impl Responder {
    let message = bincode::options()
        .deserialize::<RepairUpMessage>(&message)
        .unwrap();
    let chunk_key = parse_key(&chunk_key);
    let mut chunk_states = data.chunk_states.lock().unwrap();
    let chunk_state = chunk_states.get_mut(&chunk_key).unwrap();
    // TODO verify proof
    chunk_state.members.extend(message.members);
    if chunk_state.members.len() >= data.config.inner_n as usize {
        if let Some(repair_finish) = chunk_state.repair_finish.take() {
            repair_finish.send(chunk_key).unwrap();
        }
    }
    HttpResponse::Ok()
}

#[post("/kademlia/repair/push/{part_id}")]
async fn kademlia_repair_push(
    data: Data<State>,
    part_id: Path<String>,
    part: Bytes,
) -> impl Responder {
    let inserted = data.parts.lock().unwrap().insert(parse_key(&part_id));
    assert!(inserted);
    tokio::fs::write(data.config.chunk_path.join(&*part_id), part)
        .await
        .unwrap();
    HttpResponse::Ok()
}
