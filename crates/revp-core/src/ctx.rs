//! Per-request context shared across phases and modules.

use std::any::{Any, TypeId};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use crate::router::Route;

/// Created once per request. Kept allocation-free unless a module stores state.
pub struct Ctx {
    pub start: Instant,
    pub route: Option<Arc<Route>>,
    pub upstream_addr: Option<SocketAddr>,
    /// Final response status, filled in before `on_log`.
    pub status: u16,
    /// Set when a module short-circuited the request (e.g. 429 from ratelimit).
    pub short_circuit: Option<&'static str>,
    slots: Vec<(TypeId, Box<dyn Any + Send + Sync>)>,
}

impl Ctx {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            route: None,
            upstream_addr: None,
            status: 0,
            short_circuit: None,
            slots: Vec::new(),
        }
    }

    /// Store per-request state for a module. Replaces any existing value of the same type.
    pub fn set<T: Any + Send + Sync>(&mut self, v: T) {
        let id = TypeId::of::<T>();
        if let Some(slot) = self.slots.iter_mut().find(|(t, _)| *t == id) {
            slot.1 = Box::new(v);
        } else {
            self.slots.push((id, Box::new(v)));
        }
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        let id = TypeId::of::<T>();
        self.slots
            .iter()
            .find(|(t, _)| *t == id)
            .and_then(|(_, b)| b.downcast_ref())
    }

    pub fn get_mut<T: Any + Send + Sync>(&mut self) -> Option<&mut T> {
        let id = TypeId::of::<T>();
        self.slots
            .iter_mut()
            .find(|(t, _)| *t == id)
            .and_then(|(_, b)| b.downcast_mut())
    }

    pub fn route_name(&self) -> &str {
        self.route.as_ref().map(|r| r.name.as_str()).unwrap_or("-")
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.start.elapsed()
    }
}

impl Default for Ctx {
    fn default() -> Self {
        Self::new()
    }
}
