//! Tiny keep-alive HTTP/1.1 origin for benchmarking. Deliberately dumb and fast
//! so that it is never the bottleneck when measuring the proxy.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use clap::Parser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1:3000")]
    listen: String,
    /// Response body size in bytes.
    #[arg(short, long, default_value_t = 13)]
    body_size: usize,
    /// Worker threads (0 = cores).
    #[arg(short, long, default_value_t = 0)]
    threads: usize,
    /// Add `Cache-Control: max-age=N` to responses (0 = none).
    #[arg(long, default_value_t = 0)]
    max_age: u64,
}

fn main() {
    let args = Args::parse();
    let threads = if args.threads == 0 {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    } else {
        args.threads
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(serve(args));
}

async fn serve(args: Args) {
    let body: Vec<u8> = if args.body_size == 13 {
        b"Hello, world!".to_vec()
    } else {
        (0..args.body_size).map(|i| b'a' + (i % 26) as u8).collect()
    };
    let mut head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nServer: bench-upstream\r\n",
        body.len()
    );
    if args.max_age > 0 {
        head.push_str(&format!("Cache-Control: public, max-age={}\r\n", args.max_age));
    }
    head.push_str("\r\n");
    let mut response = head.into_bytes();
    response.extend_from_slice(&body);
    let response: Arc<[u8]> = response.into();
    let hits = Arc::new(AtomicU64::new(0));

    let listener = TcpListener::bind(&args.listen).await.expect("bind");
    eprintln!("bench-upstream on {} ({} byte body)", args.listen, body.len());

    {
        let hits = hits.clone();
        tokio::spawn(async move {
            let mut last = 0;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let now = hits.load(Ordering::Relaxed);
                if now != last {
                    eprintln!("served {} requests total", now);
                    last = now;
                }
            }
        });
    }

    loop {
        let Ok((mut sock, _)) = listener.accept().await else { continue };
        let _ = sock.set_nodelay(true);
        let response = response.clone();
        let hits = hits.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 16 * 1024];
            let mut filled = 0usize;
            loop {
                let n = match sock.read(&mut buf[filled..]).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                filled += n;
                // Answer every complete request header in the buffer.
                loop {
                    let Some(end) = find_header_end(&buf[..filled]) else { break };
                    let req = &buf[..end];
                    let close = contains_ci(req, b"connection: close");
                    hits.fetch_add(1, Ordering::Relaxed);
                    // `/echo` returns the raw request head so tests can inspect
                    // what the proxy forwarded (Host, X-Forwarded-*, path).
                    let out: std::borrow::Cow<[u8]> = if req.starts_with(b"GET /echo") {
                        let mut v = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
                            req.len()
                        )
                        .into_bytes();
                        v.extend_from_slice(req);
                        v.into()
                    } else {
                        (&response[..]).into()
                    };
                    if sock.write_all(&out).await.is_err() {
                        return;
                    }
                    if close {
                        return;
                    }
                    buf.copy_within(end..filled, 0);
                    filled -= end;
                }
                if filled == buf.len() {
                    return; // header too large
                }
            }
        });
    }
}

fn find_header_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}
