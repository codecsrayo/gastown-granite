# Multi-stage build for the Gas Town Rust API.
#   web-builder → vite-builds the SvelteKit SPA (apps/web/build).
#   builder     → compiles the Rust workspace (gt-web + gt + gt-mcp).
#   runtime     → slim image carrying the three binaries plus the SPA at /srv/web.
# DB drivers are pure-Rust (mysql_async for Dolt, sqlx postgres without TLS), so the
# runtime image needs no OpenSSL — only ca-certificates for outbound TLS if ever enabled.
#
# Build context is the repo root so the web stage can copy `apps/web/` and the rust
# stage can copy `apps/api/`. Compose sets `build.context: .` for every service that
# uses this Dockerfile.

FROM node:20-bookworm-slim AS web-builder
RUN corepack enable && corepack prepare pnpm@9.15.0 --activate
WORKDIR /web
COPY apps/web/package.json ./package.json
RUN pnpm install --no-frozen-lockfile
COPY apps/web/ ./
RUN pnpm run build

FROM rust:1.95-slim-bookworm AS builder
WORKDIR /build
COPY apps/api ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release -p gt-web -p gt -p gt-mcp && \
    cp target/release/gt-web target/release/gt target/release/gt-mcp /usr/local/bin/

FROM debian:bookworm-slim AS runtime
# git: required by gt-web's `GET /api/worktrees` (hq-fe-api-r.8), which shells
# `git worktree list` / `status --porcelain=v2` / `rev-list main...HEAD` against the
# town root bind-mounted at `GT_TOWN_ROOT` (compose wires it for the gt-api service).
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --create-home --home-dir /var/lib/gastown --shell /usr/sbin/nologin gastown
COPY --from=builder /usr/local/bin/gt-web /usr/local/bin/gt-web
COPY --from=builder /usr/local/bin/gt /usr/local/bin/gt
COPY --from=builder /usr/local/bin/gt-mcp /usr/local/bin/gt-mcp
# Baked default MCP scope (root-owned, world-readable) so gt-mcp reads it regardless of the
# runtime user. Override by bind-mounting a different file at this path. mkdir first so the
# dir keeps 0755 (a --chmod COPY would otherwise set the auto-created dir non-traversable).
RUN mkdir -p /etc/gastown
COPY --chmod=0644 apps/api/deploy/mcp-scope.toml /etc/gastown/mcp-scope.toml
# SvelteKit static build. gt-web's `with_static_assets` (hq-fe-cut.1) serves this
# directory as the router fallback when GT_WEB_DIST points at it.
COPY --from=web-builder /web/build /srv/web
ENV GT_WEB_DIST=/srv/web
USER gastown
WORKDIR /var/lib/gastown
# gt-web defaults; gt service overrides command. Bind/token/URLs come from env.
EXPOSE 8787
CMD ["gt-web"]
