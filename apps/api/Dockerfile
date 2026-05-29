# Multi-stage build for the Gas Town Rust API.
#   builder → compiles the workspace (gt-web read-side API + gt composition root).
#   runtime → slim image carrying just the two binaries.
# DB drivers are pure-Rust (mysql_async for Dolt, sqlx postgres without TLS), so the
# runtime image needs no OpenSSL — only ca-certificates for outbound TLS if ever enabled.

FROM rust:1.95-slim-bookworm AS builder
WORKDIR /build
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release -p gt-web -p gt -p gt-mcp && \
    cp target/release/gt-web target/release/gt target/release/gt-mcp /usr/local/bin/

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --create-home --home-dir /var/lib/gastown --shell /usr/sbin/nologin gastown
COPY --from=builder /usr/local/bin/gt-web /usr/local/bin/gt-web
COPY --from=builder /usr/local/bin/gt /usr/local/bin/gt
COPY --from=builder /usr/local/bin/gt-mcp /usr/local/bin/gt-mcp
# Baked default MCP scope (root-owned, world-readable) so gt-mcp reads it regardless of the
# runtime user. Override by bind-mounting a different file at this path. mkdir first so the
# dir keeps 0755 (a --chmod COPY would otherwise set the auto-created dir non-traversable).
RUN mkdir -p /etc/gastown
COPY --chmod=0644 deploy/mcp-scope.toml /etc/gastown/mcp-scope.toml
USER gastown
WORKDIR /var/lib/gastown
# gt-web defaults; gt service overrides command. Bind/token/URLs come from env.
EXPOSE 8787
CMD ["gt-web"]
