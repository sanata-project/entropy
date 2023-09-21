use std::{
    collections::{HashMap, HashSet},
    future::Future,
    ops::Range,
    sync::Arc,
    time::SystemTime,
};

use actix_web::{
    get,
    http::StatusCode,
    post,
    web::{Bytes, Data, Json, Path, ServiceConfig},
    HttpResponse,
};
use actix_web_opentelemetry::ClientExt;
use awc::Client;
use ed25519_dalek::{Signer, SigningKey};
use opentelemetry::{trace::FutureExt, Context};
use rand::{thread_rng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    spawn,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::{sync::CancellationToken, task::LocalPoolHandle};
use tracing::{info, instrument};
use wirehair::{WirehairDecoder, WirehairEncoder};

use crate::{
    chunk::{self, ChunkKey},
    common::hex_string,
    peer::{self, Peer},
};

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

enum AppCommand {
    Put(oneshot::Sender<usize>),
    PutStatus(usize, oneshot::Sender<PutState>),
    Get(usize),

    UploadInvite(ChunkKey, u32, Peer, oneshot::Sender<bool>),
    UploadQueryFragment(ChunkKey, ChunkMember),
    UploadPushFragment(ChunkKey, Bytes),
    UploadPushMembers(ChunkKey, Vec<ChunkMember>, Peer),
    UploadOk(ChunkKey, PingMessage),
    DownloadQueryFragment(ChunkKey, Peer),
    DownloadPushFragment(ChunkKey, u32, Bytes),
    RepairInvite(ChunkKey, u32, Vec<ChunkMember>, oneshot::Sender<bool>),
    RepairQueryFragment(ChunkKey, ChunkMember),
    RepairPushFragment(ChunkKey, u32, Bytes),
    RepairOk(ChunkKey, PingMessage),

    UploadChunk(usize, u32, Vec<u8>),
    UploadExtraInvite(ChunkKey),
    AcceptRepairFragment(ChunkKey, u32, Bytes),
    RepairRecoverFinish(ChunkKey, Vec<u8>),
    DownloadFinish(ChunkKey, Vec<u8>),
}

struct StateMessage {
    command: AppCommand,
    context: Context,
}

impl From<AppCommand> for StateMessage {
    fn from(value: AppCommand) -> Self {
        Self {
            command: value,
            context: Context::current(),
        }
    }
}

type AppState = mpsc::UnboundedSender<StateMessage>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkMember {
    peer: Peer,
    index: u32,
    proof: (),
}

#[derive(Debug, Serialize, Deserialize)]
struct PingMessage {
    members: Vec<ChunkMember>,
    index: u32,
    time: SystemTime,
    signature: ed25519_dalek::Signature,
}

#[post("/upload/invite/{key}/{index}")]
#[instrument(skip(data))]
async fn upload_invite(
    data: Data<AppState>,
    path: Path<(String, u32)>,
    message: Json<Peer>,
) -> HttpResponse {
    let result = oneshot::channel();
    data.send(AppCommand::UploadInvite(parse_key(&path.0), path.1, message.0, result.0).into())
        .unwrap();
    if result.1.await.unwrap() {
        HttpResponse::Ok().finish()
    } else {
        HttpResponse::new(StatusCode::IM_A_TEAPOT)
    }
}

// a little bit tricky whether this should be GET or PUT
// on the one hand this mutates the uploader's state by recording a new valid
// chunk member, and even triggers side effect of PushMembers
// on the other hand other QueryFragment i.e. during downloading and repairing
// are immutable operations and should be GET
// this is finally decided to be GET because essentially it is a shorthand of
// - GET /upload/query-fragment/{key} to query fragment
// - POST /upload/pre-ok/{key} to register member (which is omitted)
// it is uploader's internal detail that the QueryFragment also serves as a
// registration and should not be exposed to the interface
#[get("/upload/query/{key}")]
#[instrument(skip(data))]
async fn upload_query_fragment(
    data: Data<AppState>,
    path: Path<String>,
    message: Json<ChunkMember>,
) -> HttpResponse {
    data.send(AppCommand::UploadQueryFragment(parse_key(&path), message.0).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[post("/upload/fragment/{key}")]
#[instrument(skip(data))]
async fn upload_push_fragment(
    data: Data<AppState>,
    path: Path<String>,
    payload: Bytes,
) -> HttpResponse {
    data.send(AppCommand::UploadPushFragment(parse_key(&path), payload).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[post("/upload/members/{key}")]
#[instrument(skip(data, message))]
async fn upload_push_members(
    data: Data<AppState>,
    path: Path<String>,
    message: Json<(Vec<ChunkMember>, Peer)>,
) -> HttpResponse {
    let (members, peer) = message.into_inner();
    data.send(AppCommand::UploadPushMembers(parse_key(&path), members, peer).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[post("/upload/ok/{key}")]
async fn upload_ok(
    data: Data<AppState>,
    path: Path<String>,
    message: Json<PingMessage>,
) -> HttpResponse {
    data.send(AppCommand::UploadOk(parse_key(&path), message.0).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[get("/download/query/{key}")]
#[instrument(skip(data))]
async fn download_query_fragment(
    data: Data<AppState>,
    path: Path<String>,
    message: Json<Peer>,
) -> HttpResponse {
    data.send(AppCommand::DownloadQueryFragment(parse_key(&path), message.0).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[post("/download/fragment/{key}/{index}")]
#[instrument(skip(data))]
async fn download_push_fragment(
    data: Data<AppState>,
    path: Path<(String, u32)>,
    payload: Bytes,
) -> HttpResponse {
    data.send(AppCommand::DownloadPushFragment(parse_key(&path.0), path.1, payload).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[post("/repair/invite/{key}/{index}")]
#[instrument(skip(data, message))]
async fn repair_invite(
    data: Data<AppState>,
    path: Path<(String, u32)>,
    message: Json<Vec<ChunkMember>>,
) -> HttpResponse {
    let result = oneshot::channel();
    data.send(AppCommand::RepairInvite(parse_key(&path.0), path.1, message.0, result.0).into())
        .unwrap();
    if result.1.await.unwrap() {
        HttpResponse::Ok().finish()
    } else {
        HttpResponse::new(StatusCode::IM_A_TEAPOT)
    }
}

#[get("/repair/query/{key}")]
#[instrument(skip(data))]
async fn repair_query_fragment(
    data: Data<AppState>,
    path: Path<String>,
    message: Json<ChunkMember>,
) -> HttpResponse {
    data.send(AppCommand::RepairQueryFragment(parse_key(&path), message.0).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[post("/repair/ok/{key}")]
#[instrument(skip(data, message))]
async fn repair_ok(
    data: Data<AppState>,
    path: Path<String>,
    message: Json<PingMessage>,
) -> HttpResponse {
    data.send(AppCommand::RepairOk(parse_key(&path), message.0).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[post("/repair/fragment/{key}/{index}")]
#[instrument(skip(data))]
async fn repair_push_fragment(
    data: Data<AppState>,
    path: Path<(String, u32)>,
    payload: Bytes,
) -> HttpResponse {
    data.send(AppCommand::RepairPushFragment(parse_key(&path.0), path.1, payload).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[post("/benchmark/put")]
#[instrument(skip(data))]
async fn benchmark_put(data: Data<AppState>) -> HttpResponse {
    let result = oneshot::channel();
    data.send(AppCommand::Put(result.0).into()).unwrap();
    HttpResponse::Ok().json(result.1.await.unwrap())
}

#[post("/benchmark/get/{id}")]
#[instrument(skip(data))]
async fn benchmark_get(data: Data<AppState>, path: Path<usize>) -> HttpResponse {
    data.send(AppCommand::Get(path.into_inner()).into())
        .unwrap();
    HttpResponse::Ok().finish()
}

#[get("/benchmark/put/{id}")]
#[instrument(skip(data))]
async fn benchmark_put_status(data: Data<AppState>, path: Path<usize>) -> HttpResponse {
    let result = oneshot::channel();
    data.send(AppCommand::PutStatus(path.into_inner(), result.0).into())
        .unwrap();
    HttpResponse::Ok().json(result.1.await.unwrap())
}

#[derive(Debug)]
pub struct State {
    local_peer: Peer,
    local_secret: SigningKey,

    fragment_size: u32,
    inner_n: u32,
    inner_k: u32,
    outer_n: u32,
    outer_k: u32,

    peer_store: peer::Store,
    chunk_store: chunk::Store,

    chunk_states: HashMap<ChunkKey, ChunkState>,
    put_states: Vec<PutState>,
    put_uploads: HashMap<ChunkKey, UploadChunkState>,
    get_recovers: HashMap<usize, WirehairDecoder>,

    messages: mpsc::WeakUnboundedSender<StateMessage>,
    local: LocalPoolHandle,
}

#[derive(Debug)]
struct ChunkState {
    local_index: u32,
    members: Vec<ChunkMember>,
    pinged: HashSet<u32>,
    indexes: Range<u32>,
    fragment_present: CancellationToken,
}

#[derive(Debug, Clone)]
struct UploadChunkState {
    benchmark_id: usize,
    chunk_index: u32,
    members: Vec<ChunkMember>,
    next_invite: u32,
    present_fragments: HashSet<u32>,
    recovered: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PutState {
    key: [u8; 32],
    uploads: HashSet<ChunkKey>,
    put_start: SystemTime,
    put_end: Option<SystemTime>,
    get_start: Option<SystemTime>,
    get_end: Option<SystemTime>,
}

impl State {
    pub fn create(
        local_peer: Peer,
        local_secret: SigningKey,
        fragment_size: u32,
        inner_n: u32,
        inner_k: u32,
        outer_n: u32,
        outer_k: u32,
        peer_store: peer::Store,
        chunk_store: chunk::Store,
        local: LocalPoolHandle,
    ) -> (
        impl Future<Output = Self>,
        impl FnOnce(&mut ServiceConfig) + Clone,
    ) {
        let messages = mpsc::unbounded_channel();
        let mut state = State {
            local_peer,
            local_secret,
            fragment_size,
            inner_n,
            inner_k,
            outer_n,
            outer_k,
            peer_store,
            chunk_store,
            chunk_states: Default::default(),
            put_states: Default::default(),
            put_uploads: Default::default(),
            get_recovers: Default::default(),
            messages: messages.0.downgrade(),
            local,
        };
        let run_state = async move {
            state.run(messages.1).await;
            state
        };
        (run_state, |config| Self::config(config, messages.0))
    }

    async fn run(&mut self, mut messages: mpsc::UnboundedReceiver<StateMessage>) {
        while let Some(message) = messages.recv().await {
            let _attach = message.context.attach();
            match message.command {
                AppCommand::Put(result) => self.handle_put(result),
                AppCommand::PutStatus(id, result) => self.handle_put_state(id, result),
                AppCommand::Get(id) => self.handle_get(id),

                AppCommand::UploadChunk(id, index, chunk) => {
                    self.handle_upload_chunk(id, index, chunk)
                }
                AppCommand::UploadInvite(key, index, message, result) => {
                    self.handle_upload_invite(&key, index, message, result)
                }
                AppCommand::UploadExtraInvite(key) => self.handle_upload_extra_invite(&key),
                AppCommand::UploadQueryFragment(key, message) => {
                    self.handle_upload_query_fragment(&key, message)
                }
                AppCommand::UploadPushFragment(key, fragment) => {
                    self.handle_upload_push_fragment(&key, fragment)
                }
                AppCommand::UploadPushMembers(key, members, peer) => {
                    self.handle_upload_push_members(&key, members, peer)
                }
                AppCommand::UploadOk(key, message) => self.handle_upload_ok(&key, message),

                AppCommand::DownloadQueryFragment(key, peer) => {
                    self.handle_download_query_fragment(&key, peer)
                }
                AppCommand::DownloadPushFragment(key, index, fragment) => {
                    self.handle_download_push_fragment(&key, index, fragment)
                }
                AppCommand::DownloadFinish(key, chunk) => self.handle_download_finish(&key, chunk),

                AppCommand::RepairInvite(key, index, message, result) => {
                    self.handle_repair_invite(&key, index, message, result)
                }
                AppCommand::RepairQueryFragment(key, message) => {
                    self.handle_repair_query_fragment(&key, message)
                }
                AppCommand::RepairPushFragment(key, index, fragment) => {
                    self.handle_repair_push_fragment(&key, index, fragment)
                }
                AppCommand::RepairOk(key, message) => self.handle_ping(&key, message),
                AppCommand::RepairRecoverFinish(key, fragment) => {
                    self.handle_recover_finish(&key, fragment)
                }
                _ => todo!(),
            }
        }
    }

    fn config(config: &mut ServiceConfig, app_data: AppState) {
        config
            .app_data(Data::new(app_data))
            .service(upload_invite)
            .service(upload_query_fragment)
            .service(upload_push_fragment)
            .service(upload_push_members)
            .service(upload_ok)
            .service(download_query_fragment)
            .service(download_push_fragment)
            .service(repair_invite)
            .service(repair_query_fragment)
            .service(repair_push_fragment)
            .service(repair_ok)
            .service(benchmark_put)
            .service(benchmark_put_status)
            .service(benchmark_get);
    }

    fn spawn_local<F: Future<Output = T> + 'static, T: Send + 'static>(
        local: LocalPoolHandle,
        create: impl FnOnce() -> F + Send + 'static,
    ) -> JoinHandle<T> {
        let context = Context::current();
        local.spawn_pinned(move || create().with_context(context))
    }

    #[instrument(skip(self))]
    fn handle_put(&mut self, result: oneshot::Sender<usize>) {
        if let Some(put_state) = self.put_states.last() {
            if put_state.put_end.is_none() {
                tracing::warn!("previous PUT operation not end");
            }
        }

        let id = self.put_states.len();
        let _ = result.send(id);

        let mut object = vec![0; (self.fragment_size * self.inner_k * self.outer_k) as _];
        tracing::info_span!("generate object").in_scope(|| {
            thread_rng().fill_bytes(&mut object);
        });
        self.put_states.push(PutState {
            key: Sha256::digest(&object).into(),
            uploads: Default::default(),
            put_start: SystemTime::now(),
            put_end: None,
            get_start: None,
            get_end: None,
        });
        let encoder = tracing::info_span!("create encoder").in_scope(|| {
            Arc::new(WirehairEncoder::new(
                object,
                self.fragment_size * self.inner_k,
            ))
        });
        for outer_index in 0..self.outer_n {
            let encoder = encoder.clone();
            let fragment_size = self.fragment_size;
            let inner_k = self.inner_k;
            let messages = self.messages.clone();
            spawn(
                async move {
                    let mut chunk = vec![0; (fragment_size * inner_k) as _];
                    tracing::info_span!("encode chunk", outer_index).in_scope(|| {
                        encoder.encode(outer_index, &mut chunk).unwrap();
                    });
                    messages
                        .upgrade()?
                        .send(AppCommand::UploadChunk(id, outer_index, chunk).into())
                        .ok()?;
                    Some(())
                }
                .with_current_context(),
            );
        }
    }

    #[instrument(skip(self))]
    fn handle_upload_chunk(&mut self, id: usize, index: u32, chunk: Vec<u8>) {
        let key = tracing::info_span!("create upload chunk")
            .in_scope(|| self.chunk_store.upload_chunk(chunk));
        self.put_uploads.insert(
            key,
            UploadChunkState {
                benchmark_id: id,
                chunk_index: index,
                members: Default::default(),
                next_invite: self.inner_n,
                present_fragments: Default::default(),
                recovered: false,
            },
        );
        for inner_index in 0..self.inner_n {
            self.upload_invite(&key, inner_index);
        }
    }

    fn upload_invite(&self, key: &ChunkKey, index: u32) {
        let peer = self.peer_store.closest_peers(&fragment_id(key, index), 1)[0];
        let hex_key = hex_string(key);
        let peer_uri = peer.uri.clone();
        let local_peer = self.local_peer.clone();
        let messages = self.messages.clone();
        let key = *key;
        Self::spawn_local(self.local.clone(), move || async move {
            let response = Client::new()
                .post(format!("{}/upload/invite/{hex_key}/{index}", peer_uri))
                .trace_request()
                .send_json(&local_peer)
                .await
                .ok()?;
            if response.status() != StatusCode::OK {
                messages
                    .upgrade()?
                    .send(AppCommand::UploadExtraInvite(key).into())
                    .ok()?;
            }
            Some(())
        });
    }

    #[instrument(skip(self))]
    fn handle_upload_extra_invite(&mut self, key: &ChunkKey) {
        let put_upload = self.put_uploads.get_mut(key).unwrap();
        let index = put_upload.next_invite;
        put_upload.next_invite += 1;
        self.upload_invite(key, index);
    }

    #[instrument(skip(self))]
    fn handle_upload_invite(
        &mut self,
        key: &ChunkKey,
        index: u32,
        message: Peer,
        result: oneshot::Sender<bool>,
    ) {
        let local_member = if let Some(chunk_state) = self.chunk_states.get(key) {
            if chunk_state.fragment_present.is_cancelled() {
                let _ = result.send(true);
                return;
            }
            chunk_state
                .members
                .iter()
                .find(|member| member.peer.id == self.local_peer.id)
                .unwrap()
                .clone()
        } else {
            let mut chunk_state = ChunkState {
                local_index: index,
                members: Default::default(),
                pinged: Default::default(),
                indexes: 0..1, // TODO
                fragment_present: CancellationToken::new(),
            };
            // TODO generate proof
            let local_member = ChunkMember {
                peer: self.local_peer.clone(),
                index,
                proof: (),
            };
            chunk_state.members.push(local_member.clone());
            self.chunk_states.insert(*key, chunk_state);
            local_member
        };
        if local_member.index != index {
            info!(
                local_index = local_member.index,
                index, "reject duplicated invite"
            );
            let _ = result.send(false);
            return; //
        }

        let hex_key = hex_string(key);
        Self::spawn_local(self.local.clone(), move || async move {
            Client::new()
                .get(format!("{}/upload/query/{hex_key}", message.uri))
                .trace_request()
                .send_json(&local_member)
                .await
                .ok()?;
            Some(())
        });
        let _ = result.send(true);
    }

    #[instrument(skip(self))]
    fn handle_upload_query_fragment(&mut self, key: &ChunkKey, message: ChunkMember) {
        let upload = self.put_uploads.get_mut(key).unwrap();
        if upload.members.len() == self.inner_n as usize {
            return;
        }

        // TODO verify proof
        // TODO deduplicate
        let task = self.chunk_store.generate_fragment(key, message.index);
        let local = self.local.clone();
        let uri = message.peer.uri.clone();
        let hex_key = hex_string(key);
        spawn(
            async move {
                let fragment = task.with_current_context().await;
                Self::spawn_local(local, move || async move {
                    Client::new()
                        .post(format!("{uri}/upload/fragment/{hex_key}"))
                        .trace_request()
                        .send_body(fragment)
                        .await
                        .ok()?;
                    Some(())
                });
            }
            .with_current_context(),
        );

        upload.members.push(message);
        if upload.members.len() == self.inner_n as usize {
            let upload = self.put_uploads.get(key).unwrap();
            let hex_key = hex_string(key);
            for member in upload.members.clone() {
                let hex_key = hex_key.clone();
                let members = upload.members.clone();
                let peer = self.local_peer.clone();
                Self::spawn_local(self.local.clone(), || async move {
                    Client::new()
                        .post(format!("{}/upload/members/{hex_key}", member.peer.uri))
                        .trace_request()
                        .send_json(&(members, peer))
                        .await
                        .ok()?;
                    Some(())
                });
            }
        }
    }

    #[instrument(skip(self, fragment))]
    fn handle_upload_push_fragment(&mut self, key: &ChunkKey, fragment: Bytes) {
        let chunk_state = self.chunk_states.get_mut(key).unwrap();
        let put_fragment =
            self.chunk_store
                .put_fragment(key, chunk_state.local_index, fragment.to_vec());
        let fragment_present = chunk_state.fragment_present.clone();
        spawn(
            async move {
                put_fragment.with_current_context().await;
                fragment_present.cancel();
            }
            .with_current_context(),
        );
    }

    #[instrument(skip(self))]
    fn handle_upload_push_members(
        &mut self,
        key: &ChunkKey,
        members: Vec<ChunkMember>,
        peer: Peer,
    ) {
        let chunk_state = self.chunk_states.get_mut(key).unwrap();
        chunk_state.members = members;
        // TODO index

        let fragment_present = chunk_state.fragment_present.clone();
        let index = chunk_state.local_index;
        let local_secret = self.local_secret.clone();
        let hex_key = hex_string(key);
        Self::spawn_local(self.local.clone(), move || async move {
            fragment_present.cancelled().await;
            let message = PingMessage {
                members: Default::default(),
                index,
                time: SystemTime::now(),
                signature: local_secret.sign(&index.to_le_bytes()),
            };
            Client::new()
                .post(format!("{}/upload/ok/{hex_key}", peer.uri))
                .trace_request()
                .send_json(&message)
                .await
                .ok()?;
            Some(())
        });
    }

    #[instrument(skip(self))]
    fn handle_upload_ok(&mut self, key: &ChunkKey, message: PingMessage) {
        // TODO verify signature
        let upload = self.put_uploads.get_mut(key).unwrap();
        upload.present_fragments.insert(message.index);
        if upload.present_fragments.len() == self.inner_n as usize {
            self.chunk_store.finish_upload(key);
            let put_state = &mut self.put_states[upload.benchmark_id];
            assert!(put_state.put_end.is_none());
            put_state.uploads.insert(*key);
            if put_state.uploads.len() == self.outer_n as usize {
                put_state.put_end = Some(SystemTime::now());
            }
        }
    }

    #[instrument(skip(self))]
    fn handle_get(&mut self, id: usize) {
        let put_state = &mut self.put_states[id];
        assert!(put_state.put_end.is_some());
        assert!(put_state.get_start.is_none());
        put_state.get_start = Some(SystemTime::now());
        self.get_recovers.insert(
            id,
            WirehairDecoder::new(
                self.fragment_size as u64 * self.inner_k as u64 * self.outer_k as u64,
                self.fragment_size * self.inner_k,
            ),
        );
        for key in &self.put_states[id].uploads {
            self.chunk_store.recover_chunk(key);
        }
        for key in &self.put_states[id].uploads {
            for member in &self.put_uploads[key].members {
                let peer_uri = member.peer.uri.clone();
                let hex_key = hex_string(key);
                let local_peer = self.local_peer.clone();
                Self::spawn_local(self.local.clone(), move || async move {
                    Client::new()
                        .get(format!("{}/download/query/{hex_key}", peer_uri))
                        .trace_request()
                        .send_json(&local_peer)
                        .await
                        .ok()?;
                    Some(())
                });
            }
        }
    }

    #[instrument(skip(self))]
    fn handle_download_query_fragment(&mut self, key: &ChunkKey, peer: Peer) {
        let Some(chunk_state) = self.chunk_states.get(key) else {
            return;
        };
        assert!(chunk_state.fragment_present.is_cancelled());
        let task = self.chunk_store.get_fragment(key, chunk_state.local_index);
        let local = self.local.clone();
        let hex_key = hex_string(key);
        let index = chunk_state.local_index;
        spawn(
            async move {
                let fragment = task.with_current_context().await;
                Self::spawn_local(local, move || async move {
                    Client::builder()
                        .disable_timeout()
                        .finish()
                        .post(format!("{}/download/fragment/{hex_key}/{index}", peer.uri))
                        .trace_request()
                        .send_body(fragment)
                        .await
                        .ok()?;
                    Some(())
                });
            }
            .with_current_context(),
        );
    }

    #[instrument(skip(self, fragment))]
    fn handle_download_push_fragment(&mut self, key: &ChunkKey, index: u32, fragment: Bytes) {
        if self.put_uploads[key].recovered
            || !self
                .get_recovers
                .contains_key(&self.put_uploads[key].benchmark_id)
        {
            return;
        }
        let task = self
            .chunk_store
            .recover_with_fragment(key, index, fragment.to_vec());
        let messages = self.messages.clone();
        let key = *key;
        spawn(
            async move {
                let chunk = task.with_current_context().await?;
                messages
                    .upgrade()?
                    .send(AppCommand::DownloadFinish(key, chunk).into())
                    .ok()?;
                Some(())
            }
            .with_current_context(),
        );
    }

    #[instrument(skip(self, chunk))]
    fn handle_download_finish(&mut self, key: &ChunkKey, chunk: Vec<u8>) {
        let upload = self.put_uploads.get_mut(key).unwrap();
        assert!(!upload.recovered);
        upload.recovered = true;
        self.chunk_store.finish_recover(key);
        let Some(decoder) = self.get_recovers.get_mut(&upload.benchmark_id) else {
            return;
        };
        if decoder.decode(upload.chunk_index, &chunk).unwrap() {
            let decoder = self.get_recovers.remove(&upload.benchmark_id).unwrap();
            let mut object =
                vec![
                    0;
                    self.fragment_size as usize * self.inner_k as usize * self.outer_k as usize
                ];
            decoder.recover(&mut object).unwrap();
            assert!(self.put_states[upload.benchmark_id].get_end.is_none());
            self.put_states[upload.benchmark_id].get_end = Some(SystemTime::now());
            assert_eq!(
                Sha256::digest(object),
                self.put_states[upload.benchmark_id].key.into()
            );
        }
    }

    #[instrument(skip(self, result))]
    fn handle_put_state(&mut self, id: usize, result: oneshot::Sender<PutState>) {
        let _ = result.send(self.put_states[id].clone());
    }

    #[instrument(skip(self, message))]
    fn handle_repair_invite(
        &mut self,
        key: &ChunkKey,
        index: u32,
        message: Vec<ChunkMember>,
        result: oneshot::Sender<bool>,
    ) {
        let local_member = if let Some(chunk_state) = self.chunk_states.get(key) {
            if chunk_state.fragment_present.is_cancelled() {
                let _ = result.send(true);
                return;
            }
            chunk_state
                .members
                .iter()
                .find(|member| member.peer.id == self.local_peer.id)
                .unwrap()
                .clone()
        } else {
            let mut chunk_state = ChunkState {
                local_index: index,
                members: Default::default(),
                pinged: Default::default(),
                indexes: 0..1, // TODO
                fragment_present: CancellationToken::new(),
            };
            // TODO generate proof
            let local_member = ChunkMember {
                peer: self.local_peer.clone(),
                index,
                proof: (),
            };
            chunk_state.members.push(local_member.clone());
            self.chunk_states.insert(*key, chunk_state);
            local_member
        };
        if local_member.index != index {
            info!(
                local_index = local_member.index,
                index, "reject duplicated invite"
            );
            let _ = result.send(false);
            return; //
        }

        self.chunk_store.recover_chunk(key);
        let hex_key = hex_string(key);
        for member in message {
            // TODO skip query for already-have fragments
            let local_member = local_member.clone();
            let hex_key = hex_key.clone();
            let messages = self.messages.clone();
            let key = *key;
            Self::spawn_local(self.local.clone(), move || async move {
                let fragment = Client::new()
                    .get(format!("{}/repair/query/{hex_key}", member.peer.uri))
                    .trace_request()
                    .send_json(&local_member)
                    .await
                    .ok()?
                    .body()
                    .await
                    .ok()?;
                messages
                    .upgrade()?
                    .send(AppCommand::AcceptRepairFragment(key, member.index, fragment).into())
                    .ok()?;
                Some(())
            });
        }
        let _ = result.send(true);
    }

    #[instrument(skip(self))]
    fn handle_repair_query_fragment(&mut self, key: &ChunkKey, message: ChunkMember) {
        // TODO verify proof
        let Some(chunk_state) = self.chunk_states.get(key) else {
            return;
        };
        assert!(chunk_state.fragment_present.is_cancelled());
        let task = self.chunk_store.get_fragment(key, chunk_state.local_index);
        let local = self.local.clone();
        let hex_key = hex_string(key);
        spawn(
            async move {
                let fragment = task.with_current_context().await;
                Self::spawn_local(local, move || async move {
                    Client::new()
                        .post(format!("{}/repair/fragment/{hex_key}", message.peer.uri))
                        .trace_request()
                        .send_body(fragment)
                        .await
                        .ok()?;
                    Some(())
                });
            }
            .with_current_context(),
        );
    }

    #[instrument(skip(self, message))]
    fn handle_ping(&mut self, key: &ChunkKey, message: PingMessage) {
        //
    }

    #[instrument(skip(self, fragment))]
    fn handle_repair_push_fragment(&mut self, key: &ChunkKey, index: u32, fragment: Bytes) {
        let chunk_state = &self.chunk_states[key];
        if chunk_state.fragment_present.is_cancelled() {
            //
            return;
        }
        assert_ne!(index, chunk_state.local_index);
        let task = self.chunk_store.encode_with_fragment(
            key,
            index,
            fragment.to_vec(),
            chunk_state.local_index,
        );
        let messages = self.messages.clone();
        let key = *key;
        spawn(
            async move {
                let fragment = task.with_current_context().await?;
                messages
                    .upgrade()?
                    .send(AppCommand::RepairRecoverFinish(key, fragment).into())
                    .ok()?;
                Some(())
            }
            .with_current_context(),
        );
    }

    #[instrument(skip(self, fragment))]
    fn handle_recover_finish(&mut self, key: &ChunkKey, fragment: Vec<u8>) {
        self.chunk_store.finish_recover(key);
        let chunk_state = self.chunk_states.get_mut(key).unwrap();
        let put_fragment = self
            .chunk_store
            .put_fragment(key, chunk_state.local_index, fragment);
        let fragment_present = chunk_state.fragment_present.clone();
        spawn(
            async move {
                put_fragment.with_current_context().await;
                fragment_present.cancel();
            }
            .with_current_context(),
        );
    }
}
