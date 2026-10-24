//! HTTP-01 challenge responder state.
//!
//! The ACME client (separate crate) inserts `token → key-authorization` pairs here;
//! the HTTP proxy answers `GET /.well-known/acme-challenge/<token>` from this map
//! on any plaintext listener. Kept in core so the proxy compiles without ACME.

use parking_lot::RwLock;
use std::collections::HashMap;

pub const CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

#[derive(Default)]
pub struct AcmeChallenges {
    tokens: RwLock<HashMap<String, String>>,
}

impl AcmeChallenges {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, token: String, key_auth: String) {
        self.tokens.write().insert(token, key_auth);
    }

    pub fn remove(&self, token: &str) {
        self.tokens.write().remove(token);
    }

    pub fn get(&self, token: &str) -> Option<String> {
        self.tokens.read().get(token).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.read().is_empty()
    }
}
