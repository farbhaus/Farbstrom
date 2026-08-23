# Single-container build for Farbstrom
# Combines OvenMediaEngine, Valkey, LiveKit, Caddy, and the Rust backend.
#
# Component versions are PINNED here — these defaults are what CI bakes into the
# published image, so a rebuild can't silently pull a new major. Override per
# build with --build-arg (the local docker-compose.override.yml wires these from
# .env: CADDY_TAG / LIVEKIT_TAG / OME_TAG / VALKEY_TAG). NOTE: deploy hosts that
# PULL farbhaus/farbstrom get whatever CI baked — pin the whole image there with
# FARBSTROM_TAG; the *_TAG vars only affect a local/source build.
ARG OME_VERSION=v0.21.0
ARG LIVEKIT_VERSION=v1.13.5
ARG CADDY_VERSION=2.11.4
ARG VALKEY_VERSION=9.1.1

# ------------------------------------------------------------------------------
# Stage 1 — Backend builder (Debian-based, matches final glibc)
# ------------------------------------------------------------------------------
FROM rust:1-bookworm AS backend-builder
RUN apt-get update && apt-get install -y libssl-dev pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release 2>/dev/null; rm -rf src
COPY backend/src/ ./src/
COPY backend/schema.sql ./
RUN touch src/main.rs && cargo build --release

# Third-party attribution, generated from the tree that was just compiled.
# The MIT/BSD/Apache crates linked into the binary above require their notices
# to travel with binary redistribution, and this image IS that redistribution —
# so the notices are produced here rather than gated in CI, where a checked-in
# copy could (and repeatedly did) drift from the actual dependency tree.
# cargo-about is version-pinned: notice output differs across versions. The musl
# builds are static, so they run on this Debian base; pick by build arch so an
# arm64 `make dev` works the same as CI's amd64. about.toml pins the target
# triples it reports on, so the output is identical on either arch.
ARG CARGO_ABOUT_VERSION=0.9.0
COPY backend/about.toml backend/about.hbs ./
RUN case "$(uname -m)" in \
        aarch64|arm64) ABOUT_ARCH=aarch64 ;; \
        *)             ABOUT_ARCH=x86_64  ;; \
    esac \
    && curl -fsSL "https://github.com/EmbarkStudios/cargo-about/releases/download/${CARGO_ABOUT_VERSION}/cargo-about-${CARGO_ABOUT_VERSION}-${ABOUT_ARCH}-unknown-linux-musl.tar.gz" \
        | tar -xz --strip-components=1 -C /usr/local/bin --wildcards '*/cargo-about' \
    && cargo about generate about.hbs -o /app/THIRD_PARTY_NOTICES.md

# ------------------------------------------------------------------------------
# Stage 2 — Valkey builder
# ------------------------------------------------------------------------------
FROM ubuntu:22.10 AS valkey-builder
RUN apt-get update && apt-get install -y build-essential curl && rm -rf /var/lib/apt/lists/*
ARG VALKEY_VERSION
# Build from source against this base's glibc (an alpine/musl prebuilt won't run
# on the Ubuntu final image). Strip any image-tag suffix so a VALKEY_TAG like
# `8.1.7-alpine` maps to the `8.1.7` source tag.
RUN VER="${VALKEY_VERSION%%-*}" \
    && curl -L "https://github.com/valkey-io/valkey/archive/refs/tags/${VER}.tar.gz" | tar xz -C /tmp \
    && cd /tmp/valkey-* && make && make install

# ------------------------------------------------------------------------------
# Stage 3 — Frontend builder (tsc → www/dist), so the image is self-contained:
# no Node needed on CI or deploy hosts. outDir is ../www/dist → /www/dist here.
# ------------------------------------------------------------------------------
FROM node:22-alpine AS frontend-builder
WORKDIR /frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# ------------------------------------------------------------------------------
# Stages 4–5 — version-pinned binary sources. Named stages let `COPY --from`
# use a pinned tag (BuildKit forbids variable expansion directly in COPY --from);
# the FROM lines resolve the pinned ARGs from global scope.
# ------------------------------------------------------------------------------
FROM caddy:${CADDY_VERSION} AS caddy-src
FROM livekit/livekit-server:${LIVEKIT_VERSION} AS livekit-src

# ------------------------------------------------------------------------------
# Stage 6 — Final image (OME Ubuntu base, version-pinned)
# ------------------------------------------------------------------------------
FROM ovenmedialabs/ovenmediaengine:${OME_VERSION}

# Runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    supervisor \
    ca-certificates \
    libssl3 \
    wget \
    && rm -rf /var/lib/apt/lists/*

# Unprivileged users for the services that don't need root. The backend (DB,
# admin auth, secrets) and Valkey drop privileges via supervisord `user=`;
# Caddy/LiveKit/OME stay root because they bind privileged ports (80/443, TURN
# 3478) — same as their official images. /data is chown'd to app at runtime by
# entrypoint.sh (it's a bind mount, so build-time ownership wouldn't stick).
RUN useradd -r -u 10001 -m -d /home/app app \
    && useradd -r -u 10002 valkey

# Valkey
COPY --from=valkey-builder /usr/local/bin/valkey-server /usr/local/bin/valkey-server

# LiveKit — official image ships the binary at /livekit-server (its entrypoint)
COPY --from=livekit-src /livekit-server /usr/local/bin/livekit-server

# Caddy — official image ships the binary at /usr/bin/caddy
COPY --from=caddy-src /usr/bin/caddy /usr/local/bin/caddy

# Backend binary + schema
COPY --from=backend-builder /app/target/release/stream-backend /usr/local/bin/stream-backend
COPY --from=backend-builder /app/schema.sql /app/schema.sql

# Dependency attribution notices, so the published image carries the licences of
# what is compiled into it (see the generating stage above).
COPY --from=backend-builder /app/THIRD_PARTY_NOTICES.md /usr/share/doc/farbstrom/THIRD_PARTY_NOTICES.md

# Static web assets (HTML/CSS), then the freshly compiled JS bundle on top.
COPY www/ /www/
COPY --from=frontend-builder /www/dist /www/dist

# OME configuration overrides
COPY ome/origin_conf/ /opt/ovenmediaengine/bin/origin_conf/

# Caddy config — single source of truth (entrypoint no longer generates it).
# Uses {$SITE_ADDRESS:localhost} env substitution at Caddy load time.
COPY caddy/Caddyfile /etc/caddy/Caddyfile

# Supervisor + entrypoint
COPY supervisord.conf /etc/supervisor/conf.d/supervisord.conf
COPY entrypoint.sh /entrypoint.sh
# OME launch wrapper: sources the DB-managed SRT passphrase (gh #208) before OME.
COPY ome_start.sh /usr/local/bin/ome_start.sh
RUN chmod +x /entrypoint.sh /usr/local/bin/ome_start.sh

# Single-container env defaults
ENV PORT=4001
ENV DB_PATH=/data/stream.db
ENV DATA_PATH=/data
ENV OME_API_URL=http://localhost:8081/v1
ENV LIVEKIT_INTERNAL_URL=http://localhost:7880
# LIVEKIT_URL and PUBLIC_ORIGIN are browser-facing and are derived from
# SITE_ADDRESS at runtime by entrypoint.sh; these are documentation-only
# defaults for the localhost case and are always overridden at startup.
ENV LIVEKIT_URL=wss://localhost/livekit
ENV PUBLIC_ORIGIN=https://localhost

# Caddy / LiveKit / OME / Backend / Valkey
EXPOSE 80 443 443/udp
EXPOSE 4001
EXPOSE 1935
EXPOSE 9999/udp 9998/udp
EXPOSE 3478
EXPOSE 10000-10009/udp
EXPOSE 7880 7881
EXPOSE 50000-50100/udp

# Honest AND mode-independent: backend /healthz (app up) AND a TCP probe that
# Caddy is listening on :80 — a dead web tier (the bug that shipped here) must
# read unhealthy. No TLS/SNI, so it behaves the same on localhost and a real
# domain. bash is present on the OME base; sh lacks /dev/tcp.
HEALTHCHECK --interval=15s --timeout=5s --retries=3 --start-period=30s \
    CMD wget -q -O /dev/null http://127.0.0.1:4001/healthz && bash -c '< /dev/tcp/127.0.0.1/80' || exit 1

ENTRYPOINT ["/entrypoint.sh"]
