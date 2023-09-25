use std::{
    collections::{HashMap, HashSet},
    iter::repeat,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use actix_web::{
    get, post,
    web::{Bytes, Data, Json, Path, ServiceConfig},
    HttpResponse, Responder,
};
use bincode::Options;
use opentelemetry::trace::FutureExt;
use rand::RngCore;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::{sync::CancellationToken, task::LocalPoolHandle};
use wirehair::{WirehairDecoder, WirehairEncoder};

use crate::{common::hex_string, peer, ChunkKey, Peer};

pub struct State {
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
    benchmarks: Mutex<Vec<Arc<Mutex<StateBenchmark>>>>,
    upload_chunk_states: Arc<Mutex<HashMap<ChunkKey, UploadChunkState>>>,
    chunk_states: Arc<Mutex<HashMap<ChunkKey, ChunkState>>>,
    peer_store: Arc<Mutex<peer::Store>>,
    client: Client,
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
}

#[derive(Debug, Clone)]
struct UploadChunkState {
    chunk_index: u32,
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

impl State {
    pub fn new(config: StateConfig, pool: LocalPoolHandle, peer_store: peer::Store) -> Self {
        Self {
            config: Arc::new(config),
            pool,
            benchmarks: Default::default(),
            upload_chunk_states: Default::default(),
            chunk_states: Default::default(),
            peer_store: Arc::new(Mutex::new(peer_store)),
            client: Client::new(),
        }
    }

    pub fn inject(config: &mut ServiceConfig, state: Data<Self>) {
        config
            .app_data(state)
            .service(benchmark_entropy)
            .service(benchmark_entropy_get)
            .service(poll_benchmark)
            .service(entropy_upload_invite)
            .service(entropy_upload_query)
            .service(entropy_upload_push)
            .service(entropy_upload_members)
            .service(entropy_upload_up)
            .service(entropy_download_pull);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkMembersMessage {
    members: HashMap<u32, ChunkMember>,
    uploader: Peer,
}

fn spawn<F: std::future::Future<Output = T> + 'static, T: Send + 'static>(
    pool: &LocalPoolHandle,
    create: impl FnOnce() -> F + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    let context = opentelemetry::Context::current();
    pool.spawn_pinned(move || create().with_context(context))
}

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
    let client = data.client.clone();
    spawn(&data.pool, move || {
        entropy_put(
            benchmark,
            upload_chunk_states,
            peer_store,
            config,
            pool,
            client,
        )
    });
    Json(id)
}

#[post("/benchmark/entropy/{index}/get")]
async fn benchmark_entropy_get(data: Data<State>, index: Path<usize>) -> impl Responder {
    let benchmarks = data.benchmarks.lock().unwrap();
    let benchmark = benchmarks[index.into_inner()].clone();
    let upload_chunk_states = data.upload_chunk_states.clone();
    let config = data.config.clone();
    let pool = data.pool.clone();
    let client = data.client.clone();
    spawn(&data.pool, move || {
        entropy_get(benchmark, upload_chunk_states, config, pool, client)
    });
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
    client: Client,
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
                client,
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
                        client,
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
                            client,
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
    client: Client,
) {
    let mut chunk = vec![0; config.chunk_size() as usize];
    chunk_encoder.encode(chunk_index, &mut chunk).unwrap();
    let chunk_key = Sha256::digest(&chunk).into();
    benchmark.lock().unwrap().chunk_keys.push(chunk_key);

    let mut peers = HashMap::new();
    let mut peer_id_index = HashMap::new();
    for index in 0..config.inner_n * 4 {
        let peer = peer_store
            .lock()
            .unwrap()
            .closest_peers(&fragment_id(&chunk_key, index), 1)[0]
            .clone();
        if peer.uri.parse::<actix_web::http::Uri>().unwrap().host()
            == config
                .peer
                .uri
                .parse::<actix_web::http::Uri>()
                .unwrap()
                .host()
        {
            continue;
        }
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
    for ((index, peer), (hex_key, message, client)) in
        peers
            .into_iter()
            .zip(repeat((hex_key.clone(), message, client.clone())))
    {
        spawn(&pool, move || async move {
            let response = client
                .post(format!(
                    "{}/entropy/upload/invite/{hex_key}/{index}",
                    peer.uri
                ))
                // .trace_request()
                .body(message)
                .send()
                .await
                .expect(&hex_string(&peer.id));
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{:?}",
                response.bytes().await
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
            let client = client.clone();
            spawn(&pool, move || {
                // acquire resource?
                entropy_push_fragment(chunk_key, index, encoder, peer, config, client)
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
    for (member, (hex_key, message, client)) in
        members.values().zip(repeat((hex_key, message, client)))
    {
        let uri = member.peer.uri.clone();
        spawn(&pool, move || async move {
            let response = client
                .post(format!("{uri}/entropy/upload/members/{hex_key}"))
                // .trace_request()
                .body(message)
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{:?}",
                response.bytes().await
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
    client: Client,
) {
    let mut fragment = vec![0; config.fragment_size as usize];
    encoder.encode(index, &mut fragment).unwrap();
    let hex_key = hex_string(&chunk_key);
    // let _resource = client_resource.acquire().await;
    let response = client
        .post(format!(
            "{}/entropy/upload/push/{hex_key}/{index}",
            peer.uri
        ))
        // .trace_request()
        .body(fragment)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{:?}",
        response.bytes().await
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
    let client = data.client.clone();
    spawn(&data.pool, move || async move {
        let response = client
            .post(format!("{}/entropy/upload/query/{chunk_key}", uploader.uri))
            // .trace_request()
            .body(message)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{:?}",
            response.bytes().await
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
    let client = data.client.clone();
    spawn(&data.pool, move || async move {
        has_fragment.cancelled().await;
        // TODO spawn check repair task
        let response = client
            .post(format!(
                "{}/entropy/upload/up/{chunk_key}/{index}",
                message.uploader.uri,
            ))
            // .trace_request()
            .body(Bytes::default()) // TODO signature
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{:?}",
            response.bytes().await
        );
    });
    HttpResponse::Ok()
}

async fn entropy_get(
    benchmark: Arc<Mutex<StateBenchmark>>,
    upload_chunk_states: Arc<Mutex<HashMap<ChunkKey, UploadChunkState>>>,
    config: Arc<StateConfig>,
    pool: LocalPoolHandle,
    client: Client,
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
                .zip(repeat((
                    upload_chunk_states,
                    config.clone(),
                    pool.clone(),
                    client,
                )))
                .map(
                    |(&chunk_key, (upload_chunk_states, config, cloned_pool, client))| {
                        spawn(&pool, move || {
                            entropy_download_chunk(
                                chunk_key,
                                upload_chunk_states,
                                config,
                                cloned_pool,
                                client,
                            )
                        })
                    },
                ),
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
    client: Client,
) -> (u32, Vec<u8>) {
    let upload_chunk_state = upload_chunk_states.lock().unwrap()[&chunk_key].clone();
    let pull_fragments = Vec::from_iter(
        upload_chunk_state
            .members
            .into_values()
            .zip(repeat((hex_string(&chunk_key), client)))
            .map(|(member, (hex_key, client))| {
                spawn(&pool, move || async move {
                    let response = client
                        .get(format!(
                            "{}/entropy/download/pull/{hex_key}/{}",
                            member.peer.uri, member.index
                        ))
                        // .trace_request()
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::OK,
                        "{:?}",
                        response.bytes().await
                    );
                    (member.index, response.bytes().await.unwrap())
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
    tokio::fs::read(
        data.config
            .chunk_path
            .join(chunk_key)
            .join(index.to_string()),
    )
    .await
    .unwrap()
}
