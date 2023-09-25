use std::{cmp::Reverse, convert::identity};

use serde::{Deserialize, Serialize};
use sha2::Digest;

pub type PeerId = [u8; 32];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub id: PeerId,
    pub key: ed25519_dalek::VerifyingKey,
    pub uri: String,
}

impl Peer {
    pub fn signing_key(uri: &str) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from(&sha2::Sha256::digest(uri).into())
    }

    pub fn new(uri: String) -> Self {
        let uri = crate::common::aws_dns_to_ip(uri);
        let key = Self::signing_key(&uri).verifying_key();
        Self {
            key,
            id: sha2::Sha256::digest(key).into(),
            uri,
        }
    }
}

#[derive(Debug)]
pub struct Store {
    peers: Vec<Peer>,
}

impl Store {
    pub fn new(mut peers: Vec<Peer>) -> Self {
        peers.sort_unstable_by_key(|peer| peer.id);
        Self { peers }
    }
}

fn common_prefix_length(id1: &PeerId, id2: &PeerId) -> u32 {
    let byte_count = (0..32).take_while(|&i| id1[i] == id2[i]).count() as u32;
    if byte_count == 32 {
        32 * 8
    } else {
        byte_count * 8 + (id1[byte_count as usize] ^ id2[byte_count as usize]).leading_zeros()
    }
}

impl Store {
    pub fn closest_peers(&self, target: &[u8; 32], count: usize) -> Vec<&Peer> {
        assert!(self.peers.len() >= count);
        let pos = self
            .peers
            .binary_search_by_key(target, |peer| peer.id)
            .unwrap_or_else(identity);
        let mut candidates = Vec::from_iter(
            // (pos.saturating_sub(count)..(pos + count).min(self.peers.len()))
            (pos.saturating_sub(10)..(pos + 10).min(self.peers.len()))
                .map(|i| &self.peers[i]),
        );
        candidates.sort_by_key(|peer| Reverse(common_prefix_length(&peer.id, target)));
        if candidates.len() > count {
            candidates.truncate(count);
        }
        candidates
    }
}

#[cfg(test)]
mod tests {
    use rand::rngs::OsRng;
    use sha2::Digest;

    use super::*;

    #[test]
    fn always_contain_identical_peer() {
        let peers = Vec::from_iter((0..100).map(|_| {
            let k = ed25519_dalek::SigningKey::generate(&mut OsRng).verifying_key();
            Peer {
                id: sha2::Sha256::digest(k).into(),
                key: k,
                uri: Default::default(),
            }
        }));
        let store = Store::new(peers);
        for peer in &store.peers {
            // println!("{:?}", peer as *const _);
            let closest = store.closest_peers(&peer.id, 10);
            assert_eq!(closest.len(), 10);
            assert!(closest.into_iter().any(|p| std::ptr::eq(p, peer)));
        }
    }
}
