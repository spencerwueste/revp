# Multi-stage build. Works with docker and podman:
#   podman build -t revp .
#   podman run --rm -p 80:80 -p 443:443 -v ./revp.toml:/etc/revp/revp.toml:ro revp
#
# Build only the modules you want:
#   podman build --build-arg FEATURES="metrics,accesslog" -t revp .
ARG RUST_VERSION=1.98
FROM docker.io/library/rust:${RUST_VERSION}-bookworm AS build
ARG FEATURES="full"
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release -p revp --no-default-features --features "${FEATURES}" \
 && strip target/release/revp

FROM docker.io/library/debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd -r -u 10001 -s /usr/sbin/nologin revp \
 && mkdir -p /etc/revp /var/lib/revp && chown revp:revp /var/lib/revp
COPY --from=build /src/target/release/revp /usr/local/bin/revp
COPY examples/revp.toml /etc/revp/revp.toml
USER revp
EXPOSE 80 443 9090
# Pingora's pid/upgrade files default under /tmp in the example config.
ENTRYPOINT ["/usr/local/bin/revp"]
CMD ["-c", "/etc/revp/revp.toml"]
