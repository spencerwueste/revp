//! revp core: a lean reverse proxy on Pingora with a per-route module pipeline.
//!
//! Module authors depend on this crate only; the re-exports below cover what a
//! module needs without pulling Pingora crates directly.

pub mod acme;
pub mod admin;
pub mod config;
pub mod ctx;
pub mod l4;
pub mod manager;
pub mod module;
pub mod proxy;
pub mod router;
pub mod runtime;
pub mod server;
pub mod tls;
pub mod upstream;

pub use ctx::Ctx;
pub use module::{
    write_response_header, AdminEndpoint, BuildCtx, Flow, Module, ModuleFactory, Phases, Registry,
};

// Re-exports for modules.
pub use anyhow;
pub use async_trait::async_trait;
pub use bytes;
pub use http;
pub use log;
pub use pingora_cache;
pub use pingora_core;
pub use pingora_error::{Error, ErrorType, Result};
pub use pingora_http::{RequestHeader, ResponseHeader};
pub use pingora_proxy::Session;
pub use serde;
pub use toml;

/// Parse a module's TOML table into a `Deserialize` config struct.
pub fn parse_module_config<T: serde::de::DeserializeOwned>(t: &toml::Table) -> anyhow::Result<T> {
    T::deserialize(toml::Value::Table(t.clone())).map_err(|e| anyhow::anyhow!("{e}"))
}
