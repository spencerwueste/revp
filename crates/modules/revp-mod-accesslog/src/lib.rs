//! Access log module: one JSON object per request.
//!
//! The request path only serialises the line and pushes it onto a bounded
//! channel; a single writer task per output does the buffered I/O. If the
//! channel is full the line is dropped and counted rather than blocking.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use parking_lot::Mutex;
use revp_core::{
    async_trait, parse_module_config, BuildCtx, Ctx, Error, Module, ModuleFactory, Phases, Session,
};
use serde::Deserialize;
use tokio::sync::mpsc;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Cfg {
    /// `stdout`, `stderr`, or a file path.
    output: String,
    /// Lines buffered before back-pressure turns into drops.
    queue: usize,
    /// Include `user_agent` and `referer`.
    client_headers: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            output: "stdout".into(),
            queue: 65_536,
            client_headers: true,
        }
    }
}

pub static DROPPED: AtomicU64 = AtomicU64::new(0);

static WRITERS: LazyLock<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn open_sink(output: &str) -> std::io::Result<Box<dyn Write + Send>> {
    Ok(match output {
        "stdout" => Box::new(std::io::stdout()),
        "stderr" => Box::new(std::io::stderr()),
        path => Box::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
        ),
    })
}

/// One writer task per distinct output, created on first use (inside tokio).
fn sender_for(output: &str, queue: usize) -> std::io::Result<mpsc::Sender<Vec<u8>>> {
    let mut w = WRITERS.lock();
    if let Some(tx) = w.get(output) {
        return Ok(tx.clone());
    }
    let sink = open_sink(output)?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>(queue);
    std::thread::Builder::new()
        .name(format!("revp-accesslog:{output}"))
        .spawn(move || writer_loop(sink, rx))?;
    w.insert(output.to_string(), tx.clone());
    Ok(tx)
}

/// Dedicated OS thread so slow disks never stall the async workers.
fn writer_loop(sink: Box<dyn Write + Send>, mut rx: mpsc::Receiver<Vec<u8>>) {
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, sink);
    loop {
        match rx.blocking_recv() {
            Some(line) => {
                let _ = out.write_all(&line);
                // Drain whatever else is queued, then flush once.
                while let Ok(line) = rx.try_recv() {
                    let _ = out.write_all(&line);
                }
                let _ = out.flush();
            }
            None => {
                let _ = out.flush();
                return;
            }
        }
    }
}

pub struct AccessLog {
    tx: mpsc::Sender<Vec<u8>>,
    client_headers: bool,
}

#[async_trait]
impl Module for AccessLog {
    fn name(&self) -> &'static str {
        "accesslog"
    }
    fn phases(&self) -> Phases {
        Phases::LOG
    }

    async fn on_log(&self, s: &mut Session, e: Option<&Error>, cx: &mut Ctx) {
        let req = s.req_header();
        let dur = cx.elapsed();
        let mut line = Vec::with_capacity(320);
        {
            let mut w = JsonLine::new(&mut line);
            w.str("ts", &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            w.str_opt(
                "client",
                s.client_addr().and_then(|a| a.as_inet()).map(|a| a.ip().to_string()).as_deref(),
            );
            w.str("method", req.method.as_str());
            w.str(
                "uri",
                req.uri.path_and_query().map(|p| p.as_str()).unwrap_or(req.uri.path()),
            );
            w.str_opt("host", revp_core::router::request_host(req));
            w.str("proto", version_str(req.version));
            w.num("status", cx.status as u64);
            w.num("bytes_sent", s.body_bytes_sent() as u64);
            w.num("bytes_recv", s.body_bytes_read() as u64);
            w.float("duration_ms", dur.as_secs_f64() * 1000.0);
            w.str("route", cx.route_name());
            w.str_opt("upstream", cx.upstream_addr.map(|a| a.to_string()).as_deref());
            w.bool("tls", s.digest().and_then(|d| d.ssl_digest.as_ref()).is_some());
            if s.cache.enabled() {
                w.str("cache", s.cache.phase().as_str());
            }
            if let Some(by) = cx.short_circuit {
                w.str("by", by);
            }
            if self.client_headers {
                w.str_opt("user_agent", header(req, "user-agent"));
                w.str_opt("referer", header(req, "referer"));
            }
            if let Some(e) = e {
                w.str("error", &e.to_string());
            }
            w.finish();
        }
        line.push(b'\n');
        if self.tx.try_send(line).is_err() {
            DROPPED.fetch_add(1, Relaxed);
        }
    }
}

fn header<'a>(req: &'a revp_core::RequestHeader, name: &str) -> Option<&'a str> {
    req.headers.get(name).and_then(|v| v.to_str().ok())
}

fn version_str(v: revp_core::http::Version) -> &'static str {
    use revp_core::http::Version;
    match v {
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2",
        Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/?",
    }
}

/// Minimal JSON object writer — avoids building a serde_json::Value per request.
struct JsonLine<'a> {
    buf: &'a mut Vec<u8>,
    first: bool,
}

impl<'a> JsonLine<'a> {
    fn new(buf: &'a mut Vec<u8>) -> Self {
        buf.push(b'{');
        Self { buf, first: true }
    }
    fn key(&mut self, k: &str) {
        if !self.first {
            self.buf.push(b',');
        }
        self.first = false;
        self.buf.push(b'"');
        self.buf.extend_from_slice(k.as_bytes());
        self.buf.extend_from_slice(b"\":");
    }
    fn str(&mut self, k: &str, v: &str) {
        self.key(k);
        // serde_json handles escaping correctly, including control chars.
        let _ = serde_json::to_writer(&mut *self.buf, v);
    }
    fn str_opt(&mut self, k: &str, v: Option<&str>) {
        if let Some(v) = v {
            self.str(k, v);
        }
    }
    fn num(&mut self, k: &str, v: u64) {
        self.key(k);
        let _ = write!(self.buf, "{v}");
    }
    fn float(&mut self, k: &str, v: f64) {
        self.key(k);
        let _ = write!(self.buf, "{v:.3}");
    }
    fn bool(&mut self, k: &str, v: bool) {
        self.key(k);
        self.buf.extend_from_slice(if v { b"true" } else { b"false" });
    }
    fn finish(self) {
        self.buf.push(b'}');
    }
}

pub struct AccessLogFactory;

impl ModuleFactory for AccessLogFactory {
    fn name(&self) -> &'static str {
        "accesslog"
    }
    fn priority(&self) -> i32 {
        10
    }
    fn build(
        &self,
        cfg: &revp_core::toml::Table,
        _bcx: &BuildCtx<'_>,
    ) -> revp_core::anyhow::Result<Arc<dyn Module>> {
        let c: Cfg = parse_module_config(cfg)?;
        let tx = sender_for(&c.output, c.queue)
            .map_err(|e| revp_core::anyhow::anyhow!("opening {:?}: {e}", c.output))?;
        Ok(Arc::new(AccessLog {
            tx,
            client_headers: c.client_headers,
        }))
    }
}

#[allow(dead_code)]
fn _assert_duration_used(_: Duration) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_line_escapes() {
        let mut buf = Vec::new();
        let mut w = JsonLine::new(&mut buf);
        w.str("a", "x\"y\n");
        w.num("n", 5);
        w.bool("b", true);
        w.float("f", 1.23456);
        w.finish();
        assert_eq!(String::from_utf8(buf).unwrap(), r#"{"a":"x\"y\n","n":5,"b":true,"f":1.235}"#);
    }
}
