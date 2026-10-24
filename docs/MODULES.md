# Writing a revp module

A module is a crate that depends on `revp-core` and exports a `ModuleFactory`.
The binary registers the factory behind a Cargo feature; config turns it on per
route. Nothing about a module exists at runtime unless both happen.

## Lifecycle

```
config load  ─► Registry::build_for_route ─► factory.build(table) ─► Arc<dyn Module>
                 (once per route, per config generation)

request      ─► early_request ─► request ─► [cache] ─► upstream_request
             ─► response ─► response_body* ─► log
```

Modules declare the phases they use with `phases()`. The route's `Pipeline` is
pre-split per phase at build time, so a route only calls hooks that exist. A
route with no modules runs zero module code.

| Phase              | Hook                  | Use it for                                                   |
|--------------------|-----------------------|--------------------------------------------------------------|
| `EARLY_REQUEST`    | `on_early_request`    | Configuring Pingora's built-in downstream modules (compression). Runs before they see the request. Cannot respond. |
| `REQUEST`          | `on_request`          | Access control, rate limits, early responses (`Flow::Done`). |
| `CACHE`            | `cache_enable/key/response` | One module per route. Enable `session.cache`, build keys, decide cacheability. |
| `UPSTREAM_REQUEST` | `on_upstream_request` | Rewrite headers/path sent to the backend.                    |
| `RESPONSE`         | `on_response`         | Rewrite response headers (also runs on cache hits).          |
| `RESPONSE_BODY`    | `on_response_body`    | Inspect/transform body chunks. Costs a call per chunk.       |
| `LOG`              | `on_log`              | Metrics, access logs. Status is in `cx.status`.              |

## Minimal module

```rust
use std::sync::Arc;
use revp_core::{async_trait, parse_module_config, BuildCtx, Ctx, Flow, Module,
                ModuleFactory, Phases, Result, Session};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Cfg { header: String }
impl Default for Cfg { fn default() -> Self { Self { header: "x-hello".into() } } }

pub struct Hello { name: http::HeaderName }

#[async_trait]
impl Module for Hello {
    fn name(&self) -> &'static str { "hello" }
    fn phases(&self) -> Phases { Phases::RESPONSE }
    async fn on_response(&self, _s: &mut Session, resp: &mut revp_core::ResponseHeader,
                         cx: &mut Ctx) -> Result<()> {
        resp.insert_header(self.name.clone(), cx.route_name())?;
        Ok(())
    }
}

pub struct HelloFactory;
impl ModuleFactory for HelloFactory {
    fn name(&self) -> &'static str { "hello" }
    fn priority(&self) -> i32 { 250 }             // see bands below
    fn build(&self, t: &revp_core::toml::Table, _b: &BuildCtx<'_>)
        -> revp_core::anyhow::Result<Arc<dyn Module>> {
        let c: Cfg = parse_module_config(t)?;
        Ok(Arc::new(Hello { name: c.header.parse()? }))
    }
}
```

Register it in `crates/revp/src/main.rs` behind a feature and add the feature
to `crates/revp/Cargo.toml`. Then:

```toml
[[routes]]
upstream = "web"
[routes.modules.hello]
header = "x-served-by"
```

## Ordering

Within a phase, modules run in ascending `priority()`:

| Band    | Purpose              | Shipped modules        |
|---------|----------------------|------------------------|
| 0–99    | observability        | metrics (0), accesslog (10) |
| 100–199 | access control       | ratelimit (100)        |
| 200–299 | rewriting            | headers (250)          |
| 300–399 | caching              | cache (300)            |
| 400+    | encoding             | compress (400)         |

## Rules that keep the hot path lean

- Parse everything at `build()` time (header names, patterns, sizes). Return
  errors there; `revp --check` surfaces them before deploy.
- Don't allocate per request unless you must. `Ctx::set/get` is there for
  per-request state, but prefer reading what `Session`/`Ctx` already hold.
- Anything slow (disk, network) goes to a channel + background task. See
  `revp-mod-accesslog` for the pattern (bounded channel, drop-and-count on overflow).
- State that must survive config reloads (counters, buckets) lives in a
  process-wide `static`, keyed by route name. Module instances are rebuilt on
  every reload.
- `Flow::Done` means you already wrote the full response. Set `cx.status` and
  `cx.short_circuit` so logs/metrics stay correct.

## Admin endpoints

`ModuleFactory::admin_endpoints()` can expose extra paths on the admin listener.
`metrics` uses this for `/metrics`.
