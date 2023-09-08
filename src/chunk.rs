use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use sha2::{Digest, Sha256};
use tokio::fs;
use wirehair::{WirehairDecoder, WirehairEncoder};

use crate::common::hex_string;

#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    fragment_size: u32,
    inner_k: u32,

    upload_chunks: HashMap<ChunkKey, Arc<WirehairEncoder>>,
    recovers: HashMap<ChunkKey, Arc<Mutex<Option<WirehairDecoder>>>>,
}

pub type ChunkKey = [u8; 32];

impl Store {
    pub fn new(path: PathBuf, fragment_size: u32, inner_k: u32) -> Self {
        Self {
            path,
            fragment_size,
            inner_k,

            upload_chunks: Default::default(),
            recovers: Default::default(),
        }
    }

    pub fn upload_chunk(&mut self, chunk: Vec<u8>) -> ChunkKey {
        let key = Sha256::digest(&chunk).into();
        let encoder = WirehairEncoder::new(chunk, self.fragment_size);
        self.upload_chunks.insert(key, Arc::new(encoder));
        key
    }

    pub fn generate_fragment(
        &mut self,
        key: &ChunkKey,
        index: u32,
    ) -> impl Future<Output = Vec<u8>> {
        let encoder = self.upload_chunks[key].clone();
        let fragment_size = self.fragment_size;
        async move {
            let mut fragment = vec![0; fragment_size as usize];
            encoder.encode(index, &mut fragment).unwrap();
            fragment
        }
    }

    pub fn finish_upload(&mut self, key: &ChunkKey) {
        self.upload_chunks.remove(key);
    }

    fn chunk_dir(&self, key: &ChunkKey) -> PathBuf {
        self.path.join(hex_string(&key[..]))
    }

    pub fn put_fragment(
        &self,
        key: &ChunkKey,
        index: u32,
        fragment: Vec<u8>,
    ) -> impl Future<Output = ()> {
        assert_eq!(fragment.len(), self.fragment_size as usize);
        let chunk_dir = self.chunk_dir(key);
        async move {
            fs::create_dir(&chunk_dir).await.unwrap();
            fs::write(chunk_dir.join(format!("{index}")), fragment)
                .await
                .unwrap();
        }
    }

    pub fn get_fragment(&self, key: &ChunkKey, index: u32) -> impl Future<Output = Vec<u8>> {
        let chunk_dir = self.chunk_dir(key);
        async move { fs::read(chunk_dir.join(format!("{index}"))).await.unwrap() }
    }

    pub fn recover_chunk(&mut self, key: &ChunkKey) {
        if self.recovers.contains_key(key) {
            return;
        }
        let decoder = WirehairDecoder::new(
            self.fragment_size as u64 * self.inner_k as u64,
            self.fragment_size,
        );
        self.recovers
            .insert(*key, Arc::new(Mutex::new(Some(decoder))));
    }

    fn with_fragment(
        &self,
        key: &ChunkKey,
        remote_index: u32,
        remote_fragment: Vec<u8>,
    ) -> impl Future<Output = Option<WirehairDecoder>> {
        assert_eq!(remote_fragment.len(), self.fragment_size as usize);
        let decoder_cell = self.recovers[key].clone();
        async move {
            let mut decoder_cell = decoder_cell.lock().unwrap();
            let Some(decoder) = &mut *decoder_cell else {
                return None;
            };
            if !decoder.decode(remote_index, &remote_fragment).unwrap() {
                return None;
            }
            Some(decoder_cell.take().unwrap())
        }
    }

    pub fn recover_with_fragment(
        &self,
        key: &ChunkKey,
        remote_index: u32,
        remote_fragment: Vec<u8>,
    ) -> impl Future<Output = Option<Vec<u8>>> {
        let decoder = self.with_fragment(key, remote_index, remote_fragment);
        let fragment_size = self.fragment_size;
        let inner_k = self.inner_k;
        async move {
            decoder.await.map(|decoder| {
                let mut chunk = vec![0; (fragment_size * inner_k) as usize];
                decoder.recover(&mut chunk).unwrap();
                chunk
            })
        }
    }

    pub fn encode_with_fragment(
        &self,
        key: &ChunkKey,
        remote_index: u32,
        remote_fragment: Vec<u8>,
        index: u32,
    ) -> impl Future<Output = Option<Vec<u8>>> {
        let decoder = self.with_fragment(key, remote_index, remote_fragment);
        let fragment_size = self.fragment_size;
        async move {
            decoder.await.map(|decoder| {
                let mut fragment = vec![0; fragment_size as usize];
                decoder
                    .into_encoder()
                    .unwrap()
                    .encode(index, &mut fragment)
                    .unwrap();
                fragment
            })
        }
    }

    pub fn finish_recover(&mut self, key: &ChunkKey) {
        self.recovers.remove(key);
        // assert returns None?
    }
}
