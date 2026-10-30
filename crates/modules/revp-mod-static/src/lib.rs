//! Static module: serve files from a directory.
//!
//! ```toml
//! [routes.modules.static]
//! root = "/srv"                 # required
//! index = "index.html"          # served for a directory path; "" turns it off
//! fallback = "/index.html"      # served when nothing matches (single-page apps)
//! hidden = false                # serve dot-files and dot-directories
//!
//! [[routes.modules.static.headers]]
//! paths = ["/assets/*"]         # matched against the file served, after index/fallback
//! set = { cache-control = "public, max-age=31536000, immutable" }
//! ```
//!
//! GET and HEAD only. On a miss with no `fallback` the request goes to the
//! route's upstream if it has one, else 404, so a route can serve files first
//! and proxy the rest. Answers `If-None-Match` / `If-Modified-Since` with 304.
//! Paths with `.` or `..` segments get 400; symlinks may not leave `root`.
//! No Range requests: every 200 is the whole file.

use std::collections::BTreeMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use revp_core::bytes::BytesMut;
use revp_core::http::header::{
    ALLOW, CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED,
    LOCATION,
};
use revp_core::http::{HeaderName, HeaderValue, Method};
use revp_core::{
    async_trait, log, parse_module_config, write_response_header, BuildCtx, Ctx, Error, ErrorType,
    Flow, Module, ModuleFactory, Phases, ResponseHeader, Result, Session,
};
use serde::Deserialize;
use tokio::io::AsyncReadExt;

/// Body chunk size. Files at or under this go out in one write.
const CHUNK: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cfg {
    root: PathBuf,
    #[serde(default = "default_index")]
    index: String,
    #[serde(default)]
    fallback: Option<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    headers: Vec<RuleCfg>,
}

fn default_index() -> String {
    "index.html".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleCfg {
    paths: Vec<String>,
    set: BTreeMap<String, String>,
}

/// `/assets/*` matches everything under `/assets/`; anything else is exact.
#[derive(Debug, PartialEq)]
enum Pat {
    Exact(String),
    Prefix(String),
}

impl Pat {
    fn parse(p: &str) -> Pat {
        match p.strip_suffix('*') {
            Some(pre) => Pat::Prefix(pre.to_string()),
            None => Pat::Exact(p.to_string()),
        }
    }
    fn matches(&self, path: &str) -> bool {
        match self {
            Pat::Exact(e) => path == e,
            Pat::Prefix(pre) => path.starts_with(pre.as_str()),
        }
    }
}

struct Rule {
    paths: Vec<Pat>,
    set: Vec<(HeaderName, HeaderValue)>,
}

/// A request path after percent-decoding and checking.
#[derive(Debug, PartialEq)]
enum Clean {
    Ok(String),
    /// Names a dot-file and `hidden` is off: treat as a miss.
    Hidden,
    /// `.`/`..` segments, NUL, backslash, or not UTF-8: refuse.
    Invalid,
}

fn clean(raw: &str, hidden: bool) -> Clean {
    let Ok(decoded) = percent_encoding::percent_decode_str(raw).decode_utf8() else {
        return Clean::Invalid;
    };
    if !decoded.starts_with('/') || decoded.contains(['\0', '\\']) {
        return Clean::Invalid;
    }
    let mut is_hidden = false;
    for seg in decoded.split('/') {
        if seg == "." || seg == ".." {
            return Clean::Invalid;
        }
        if seg.starts_with('.') {
            is_hidden = true;
        }
    }
    if is_hidden && !hidden {
        return Clean::Hidden;
    }
    Clean::Ok(decoded.into_owned())
}

/// What a URL path resolves to on disk.
enum Hit {
    /// The file and the URL path it is served as (used for header rules).
    File(PathBuf, String),
    /// A directory requested without its trailing slash.
    AddSlash,
}

pub struct Static {
    root: PathBuf,
    canon_root: OnceLock<PathBuf>,
    index: Option<String>,
    fallback: Option<String>,
    hidden: bool,
    rules: Vec<Rule>,
    has_upstream: bool,
}

impl Static {
    /// The canonical root, resolved on first success so a root created after
    /// startup (or missing on the machine running `--check`) still works.
    fn canon_root(&self) -> Option<&Path> {
        if let Some(r) = self.canon_root.get() {
            return Some(r);
        }
        let c = std::fs::canonicalize(&self.root).ok()?;
        Some(self.canon_root.get_or_init(|| c))
    }

    async fn lookup(&self, url: &str) -> Option<Hit> {
        let full = self.root.join(url.trim_start_matches('/'));
        let meta = tokio::fs::metadata(&full).await.ok()?;
        let (file, served_as) = if meta.is_file() {
            (full, url.to_string())
        } else if meta.is_dir() {
            let index = self.index.as_ref()?;
            let file = full.join(index);
            if !tokio::fs::metadata(&file).await.ok()?.is_file() {
                return None;
            }
            if !url.ends_with('/') {
                return Some(Hit::AddSlash);
            }
            (file, format!("{url}{index}"))
        } else {
            return None;
        };
        // Symlinks may point anywhere; only serve what really lives under root.
        let real = tokio::fs::canonicalize(&file).await.ok()?;
        if !real.starts_with(self.canon_root()?) {
            return None;
        }
        Some(Hit::File(real, served_as))
    }

    async fn status_only(
        &self,
        s: &mut Session,
        cx: &mut Ctx,
        status: u16,
        extra: Option<(HeaderName, HeaderValue)>,
    ) -> Result<Flow> {
        let mut h = ResponseHeader::build(status, Some(2))?;
        h.insert_header(CONTENT_LENGTH, "0")?;
        if let Some((k, v)) = extra {
            h.insert_header(k, v)?;
        }
        write_response_header(s, h, true, cx).await?;
        Ok(Flow::Done)
    }

    async fn serve(
        &self,
        s: &mut Session,
        cx: &mut Ctx,
        path: &Path,
        served_as: &str,
        head: bool,
    ) -> Result<Flow> {
        let io_err = |e: std::io::Error| Error::because(ErrorType::InternalError, "static: read file", e);
        let file = tokio::fs::File::open(path).await.map_err(io_err)?;
        let meta = file.metadata().await.map_err(io_err)?;
        let len = meta.len();
        let mtime = meta.modified().ok();
        let etag = etag(&meta);

        let req = s.req_header();
        let not_modified = match req.headers.get(IF_NONE_MATCH) {
            Some(inm) => etag_matches(inm.as_bytes(), &etag),
            None => match (req.headers.get(IF_MODIFIED_SINCE), mtime) {
                (Some(ims), Some(t)) => ims
                    .to_str()
                    .ok()
                    .and_then(|v| httpdate::parse_http_date(v).ok())
                    .is_some_and(|since| secs(t) <= secs(since)),
                _ => false,
            },
        };

        let mut h = ResponseHeader::build(if not_modified { 304 } else { 200 }, Some(8))?;
        if !not_modified {
            h.insert_header(CONTENT_TYPE, content_type(path))?;
            h.insert_header(CONTENT_LENGTH, len.to_string())?;
        }
        h.insert_header(ETAG, etag)?;
        if let Some(t) = mtime {
            h.insert_header(LAST_MODIFIED, httpdate::fmt_http_date(t))?;
        }
        for rule in &self.rules {
            if rule.paths.iter().any(|p| p.matches(served_as)) {
                for (k, v) in &rule.set {
                    h.insert_header(k.clone(), v.clone())?;
                }
            }
        }

        let no_body = head || not_modified || len == 0;
        write_response_header(s, h, no_body, cx).await?;
        if no_body {
            return Ok(Flow::Done);
        }

        // Send exactly the length promised, even if the file changes underneath.
        let mut reader = file.take(len);
        let mut left = len;
        let mut buf = BytesMut::with_capacity(CHUNK.min(len as usize));
        while left > 0 {
            buf.reserve(CHUNK.min(left as usize));
            let n = reader.read_buf(&mut buf).await.map_err(io_err)?;
            if n == 0 {
                return Error::e_explain(ErrorType::InternalError, "static: file shrank while sending");
            }
            left -= n as u64;
            s.write_response_body(Some(buf.split().freeze()), left == 0)
                .await?;
        }
        Ok(Flow::Done)
    }
}

#[async_trait]
impl Module for Static {
    fn name(&self) -> &'static str {
        "static"
    }
    fn phases(&self) -> Phases {
        Phases::REQUEST
    }
    fn serves(&self) -> bool {
        true
    }

    async fn on_request(&self, s: &mut Session, cx: &mut Ctx) -> Result<Flow> {
        let method = &s.req_header().method;
        let head = *method == Method::HEAD;
        if !head && *method != Method::GET {
            if self.has_upstream {
                return Ok(Flow::Continue);
            }
            let allow = (ALLOW, HeaderValue::from_static("GET, HEAD"));
            return self.status_only(s, cx, 405, Some(allow)).await;
        }

        let url = match clean(s.req_header().uri.path(), self.hidden) {
            Clean::Ok(u) => Some(u),
            Clean::Hidden => None,
            Clean::Invalid => return self.status_only(s, cx, 400, None).await,
        };
        let hit = match &url {
            Some(u) => self.lookup(u).await,
            None => None,
        };
        match hit {
            Some(Hit::File(path, served_as)) => self.serve(s, cx, &path, &served_as, head).await,
            Some(Hit::AddSlash) => {
                let uri = &s.req_header().uri;
                let loc = match uri.query() {
                    Some(q) => format!("{}/?{q}", uri.path()),
                    None => format!("{}/", uri.path()),
                };
                let loc = HeaderValue::from_str(&loc)
                    .map_err(|e| Error::because(ErrorType::InternalError, "static: location", e))?;
                self.status_only(s, cx, 308, Some((LOCATION, loc))).await
            }
            None => {
                if let Some(fb) = &self.fallback {
                    if let Some(Hit::File(path, served_as)) = self.lookup(fb).await {
                        return self.serve(s, cx, &path, &served_as, head).await;
                    }
                }
                if self.has_upstream {
                    Ok(Flow::Continue)
                } else {
                    self.status_only(s, cx, 404, None).await
                }
            }
        }
    }
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Changes whenever the file's mtime or size does.
fn etag(meta: &Metadata) -> String {
    let d = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    format!("\"{:x}.{:x}-{:x}\"", d.as_secs(), d.subsec_nanos(), meta.len())
}

/// `If-None-Match` uses weak comparison: `W/"x"` matches `"x"`.
fn etag_matches(header: &[u8], etag: &str) -> bool {
    let Ok(header) = std::str::from_utf8(header) else { return false };
    header.split(',').map(str::trim).any(|t| {
        t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag
    })
}

fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("md") => "text/markdown; charset=utf-8",
        Some("xml") => "application/xml",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("pdf") => "application/pdf",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

pub struct StaticFactory;

impl ModuleFactory for StaticFactory {
    fn name(&self) -> &'static str {
        "static"
    }
    fn priority(&self) -> i32 {
        500
    }
    fn build(
        &self,
        cfg: &revp_core::toml::Table,
        bcx: &BuildCtx<'_>,
    ) -> revp_core::anyhow::Result<Arc<dyn Module>> {
        let c: Cfg = parse_module_config(cfg)?;
        if c.root.as_os_str().is_empty() {
            revp_core::anyhow::bail!("static: `root` is required");
        }
        if c.index.contains('/') {
            revp_core::anyhow::bail!("static: `index` is a file name, not a path");
        }
        if let Some(fb) = &c.fallback {
            if !matches!(clean(fb, true), Clean::Ok(_)) {
                revp_core::anyhow::bail!("static: `fallback` must be a clean path starting with '/'");
            }
        }
        let canon_root = OnceLock::new();
        match std::fs::canonicalize(&c.root) {
            Ok(r) if r.is_dir() => {
                let _ = canon_root.set(r);
            }
            Ok(_) => revp_core::anyhow::bail!("static: root {:?} is not a directory", c.root),
            Err(e) => log::warn!(
                "route {:?}: static root {:?} not readable yet ({e}); serving 404s until it is",
                bcx.route_name,
                c.root
            ),
        }
        let mut rules = Vec::with_capacity(c.headers.len());
        for r in &c.headers {
            let mut set = Vec::with_capacity(r.set.len());
            for (k, v) in &r.set {
                set.push((
                    HeaderName::from_bytes(k.as_bytes())
                        .map_err(|e| revp_core::anyhow::anyhow!("static: header name {k:?}: {e}"))?,
                    HeaderValue::from_str(v)
                        .map_err(|e| revp_core::anyhow::anyhow!("static: header value for {k:?}: {e}"))?,
                ));
            }
            rules.push(Rule {
                paths: r.paths.iter().map(|p| Pat::parse(p)).collect(),
                set,
            });
        }
        Ok(Arc::new(Static {
            root: c.root,
            canon_root,
            index: (!c.index.is_empty()).then_some(c.index),
            fallback: c.fallback,
            hidden: c.hidden,
            rules,
            has_upstream: !bcx.route.upstream.is_empty(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_paths() {
        assert_eq!(clean("/a/b.js", false), Clean::Ok("/a/b.js".into()));
        assert_eq!(clean("/a%20b", false), Clean::Ok("/a b".into()));
        assert_eq!(clean("/../etc/passwd", false), Clean::Invalid);
        assert_eq!(clean("/a/%2e%2e/b", false), Clean::Invalid);
        assert_eq!(clean("/a/./b", false), Clean::Invalid);
        assert_eq!(clean("/a%00b", false), Clean::Invalid);
        assert_eq!(clean("/a%5cb", false), Clean::Invalid);
        assert_eq!(clean("/%ff", false), Clean::Invalid);
        assert_eq!(clean("/.env", false), Clean::Hidden);
        assert_eq!(clean("/.git/config", false), Clean::Hidden);
        assert_eq!(clean("/.env", true), Clean::Ok("/.env".into()));
        assert_eq!(clean("/a..b", false), Clean::Ok("/a..b".into()));
    }

    #[test]
    fn patterns() {
        assert!(Pat::parse("/assets/*").matches("/assets/x.js"));
        assert!(!Pat::parse("/assets/*").matches("/assetsx"));
        assert!(Pat::parse("/index.html").matches("/index.html"));
        assert!(!Pat::parse("/index.html").matches("/a/index.html"));
        assert!(Pat::parse("*").matches("/anything"));
    }

    #[test]
    fn etags() {
        assert!(etag_matches(b"\"a\"", "\"a\""));
        assert!(etag_matches(b"W/\"a\"", "\"a\""));
        assert!(etag_matches(b"\"x\", \"a\"", "\"a\""));
        assert!(etag_matches(b"*", "\"a\""));
        assert!(!etag_matches(b"\"b\"", "\"a\""));
    }

    #[test]
    fn types() {
        assert_eq!(content_type(Path::new("a/B.JS")), "text/javascript; charset=utf-8");
        assert_eq!(content_type(Path::new("x.woff2")), "font/woff2");
        assert_eq!(content_type(Path::new("noext")), "application/octet-stream");
    }
}
