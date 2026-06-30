# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

Farbstrom is a private low-latency streaming platform for color-grading review sessions. It combines OvenMediaEngine (OME) for broadcast ingest/delivery, LiveKit for participant voice/video, and a Rust/Axum backend for API and session management. The frontend is TypeScript compiled with `tsc` (no bundler, no runtime npm deps) emitted as plain ES modules.

## Commands

### Backend (Rust — `backend/`)

```bash
cargo check                              # fast type check
cargo build --release                    # production binary
cargo test                               # run all ~100 integration tests
cargo test --test rooms_public_test      # single test file
cargo test --test rooms_public_test join_creates_participant  # single test
RUST_LOG=debug cargo test -- --nocapture # tests with logs
cargo fmt                                # format
cargo clippy --all-targets -- -D warnings
cargo audit
cargo about generate about.hbs -o ../docs/THIRD_PARTY_NOTICES.md
```

Hot-reload during development (requires `watchexec-cli`):
```bash
watchexec -r -e rs -- cargo run
```

### Full stack (single-container — repo root)

The whole stack — Caddy, the Rust backend, OvenMediaEngine, LiveKit, and Valkey
— ships as **one** image (`farbhaus/farbstrom`) run by `supervisord`. There is a
single compose service, `farbstrom`.

```bash
# Local dev — opt into docker-compose.dev.yml (build from source + ./www mount).
# A thin Makefile wraps the two-file invocations.
make dev                                 # build + start (== -f docker-compose.yml -f docker-compose.dev.yml up -d --build)
make logs                                # all services' logs (interleaved)
make status                              # per-service supervisord state
make down

# Deploy hosts — a plain `docker compose up -d` uses ONLY the base file and
# pulls the published image (no accidental source build). Equivalent: make deploy.
docker compose up -d                     # start (pulls farbhaus/farbstrom)
make update                              # pull newest image + recreate
```

The image (`farbhaus/farbstrom`) is published to Docker Hub by
`.github/workflows/docker-single.yml` on every push to `main` (tags `:latest`
and `:sha-<short>`) and on a `v*` release tag (adds `:vX.Y.Z` + `:X.Y` for
reproducible prod pinning), linux/amd64. It is self-contained — the Dockerfile compiles
the Rust backend AND the TypeScript frontend internally, so deploy hosts need
neither the source nor a Node/Rust toolchain. Deploy hosts pin a tag via
`FARBSTROM_TAG` in `.env`. Requires repo secrets `DOCKERHUB_USERNAME` and
`DOCKERHUB_TOKEN`.

One-command production deploy to a fresh VPS: `./deploy.sh your.domain.com`.

### Environment setup

```bash
cp .env.example .env
# Fill in secrets — generate with: openssl rand -hex 32
```

**Required** (`backend/src/config.rs` panics at startup if missing or too short):

| Var | Min | Purpose |
|---|---|---|
| `JWT_SECRET` | 32 | HMAC secret for admin JWTs |
| `OME_WEBHOOK_SECRET` | 32 | HMAC-SHA1 key for OME admission webhook verification |
| `OME_SIGNED_POLICY_SECRET` | 32 | HMAC-SHA1 key for OME SignedPolicy SRT-playback tokens (`/api/watch/:slug`). Must match `<SignedPolicy><SecretKey>` in `ome/origin_conf/Server.xml`. |
| `OME_API_TOKEN` | 32 | Auth token for calls to the OME REST API |
| `LIVEKIT_API_SECRET` | 32 | HMAC secret for LiveKit access tokens |
| `ADMIN_PASSWORD` | 12 | Bcrypt-hashed once at startup |
| `LIVEKIT_API_KEY` | — | Identifier; becomes the `iss` claim |

**Optional** (with defaults):

| Var | Default | Purpose |
|---|---|---|
| `PORT` | `4001` | Axum bind port |
| `DB_PATH` | `/data/stream.db` | SQLite file |
| `DATA_PATH` | `/data` | Uploads and branding |
| `OME_API_URL` | `http://localhost:8081/v1` | OME admin API |
| `LIVEKIT_INTERNAL_URL` | `http://localhost:7880` | LiveKit HTTP signaling |
| `LIVEKIT_URL` | `ws://localhost:7880` | WebSocket URL sent to browser clients. In the container, **derived from `PUBLIC_HOST`** by `entrypoint.sh` (`wss://<PUBLIC_HOST>/livekit`). |
| `PUBLIC_ORIGIN` | `http://localhost:4001` | WebAuthn RP origin/ID — must match the browser origin exactly. In the container, **derived from `PUBLIC_HOST`** (`https://<PUBLIC_HOST>`). |
| `SITE_ADDRESS` | `localhost` | Caddy site address for the container's own Caddy. Set to your domain for standalone TLS, or `:80` (plain HTTP) to run behind an external TLS proxy. |
| `PUBLIC_HOST` | `$SITE_ADDRESS` | Browser-facing host that `PUBLIC_ORIGIN`/`LIVEKIT_URL` derive from. Defaults to `SITE_ADDRESS` (standalone). **Required** when `SITE_ADDRESS=:80` — a bare port is not a valid host, so without it the backend panics. |
| `WEB_BIND` | `0.0.0.0` | Host interface the published HTTP/HTTPS ports bind to. Set to `127.0.0.1` when behind an external proxy so the plain-HTTP port isn't internet-reachable (Docker bypasses ufw). |
| `SRT_PUBLIC_HOST` | host of `PUBLIC_ORIGIN` | SRT host returned by `/api/watch/:slug`. |
| `SRT_PUBLIC_PORT` | `9998` | SRT playback UDP port returned by `/api/watch/:slug`. |
| `SRT_LATENCY_MS` | `500` | SRT latency advertised to clients. |
| `STREAM_DISABLE_RATE_LIMIT` | unset | Set to `1` to disable rate limiting (integration tests do this). |

Generate secrets with `openssl rand -hex 32`.

## Architecture

All five processes run inside **one container** under `supervisord`, talking to
each other over `localhost`. Caddy owns the single TLS origin and routes by path.

```
Encoder (SRT/RTMP/WHIP)
  └─→ OvenMediaEngine (localhost, in-container)
        ├─ admission webhook → backend localhost:4001 (HMAC-SHA1 verified)
        └─→ Browser (OvenPlayer via LLHLS/WebRTC, routed through Caddy /live/*)

backend (Rust/Axum, localhost:4001)
  ├─ HTTP API   /api/*        — rooms, participants, chat, stream keys, admin auth
  ├─ WebSocket  /ws/room/:slug — presence, chat, pointer overlay, kick events
  ├─ Static     /admin/, /watch/:slug, /  — serves /www files
  └─ SQLite WAL  /data/stream.db

Browser (viewer page) — one origin, fronted by Caddy:
  ├─ OvenPlayer — stream video        (Caddy /live/*  → localhost:3333)
  ├─ LiveKit JS SDK — cam/mic/screen  (Caddy /livekit/* → localhost:7880, wss)
  └─ WebSocket — chat, presence       (Caddy /* → localhost:4001)
```

**Single container (`farbstrom`), processes under supervisord** (start order):
Valkey → backend (`user=app`) → OvenMediaEngine + LiveKit → Caddy (TLS + routing).
Caddy/OME/LiveKit run as root (privileged ports / TURN); the backend and Valkey
drop to unprivileged users. `entrypoint.sh` generates `livekit.yaml`, chowns
`/data`, and derives the browser-facing URLs from `PUBLIC_HOST` (which defaults
to `SITE_ADDRESS`); the Caddyfile
([`caddy/Caddyfile`](caddy/Caddyfile)) and supervisor config
([`supervisord.conf`](supervisord.conf)) are baked into the image.

## Backend structure

- `src/main.rs` — startup: config, DB pool, background tasks, Axum router mount on :4001
- `src/lib.rs` — re-exports the app builder so integration tests can spin up the server in-process
- `src/config.rs` — `AppConfig::from_env`, secret length validation (fail-fast)
- `src/state.rs` — `AppState` (Arc'd, cloned into handlers)
- `src/db.rs` — R2D2 SQLite pool (8 connections), WAL mode, schema bootstrap from `schema.sql`
- `src/error.rs` — `AppError` + `IntoResponse` impl; central error → HTTP mapping
- `src/events.rs` — typed WS event payloads shared between hub and routes
- `src/auth.rs` — JWT (HS256, 7d) + bcrypt helpers
- `src/livekit.rs` — hand-rolled LiveKit client: AccessToken JWT minting + RoomService HTTP
- `src/ws.rs` — WebSocket hub, broadcast channels per room
- `src/presence.rs` — in-memory presence registry for native SRT (Farbplay) viewers; their admission SSE connection is the heartbeat (browser viewers use the WS in `ws.rs` instead)
- `src/tasks.rs` — background pollers: OME stream status, room expiry, file cleanup
- `src/uploads.rs` — chunked multipart upload helper: streams a field to a temp file, Sha256-hashes as it goes, enforces the size cap (bounded memory, atomic rename)
- `src/routes/` — one file per resource: `rooms`, `rooms_public`, `files`, `admin_files`, `stream_keys`, `webhook`, `branding`, `metrics`, `ome`, `auth`, `admin_settings`, `rate_limit`, `watch` (Farbplay SRT room-link playback), `pages` (server-rendered link-preview HTML for the landing/viewer pages)
- `src/credentials.rs` — single-admin credential helpers: `settings` accessors, DB-or-env password resolver, TOTP, recovery codes, WebAuthn RP builder
- `tests/common/mod.rs` — shared test fixtures (in-memory DB, app setup)

## Frontend structure

TypeScript sources live under `frontend/`, compiled by `tsc` to `www/dist/` (plain ES modules, no bundler). HTML pages under `www/` import the compiled modules via `<script type="module" src="/dist/<page>/main.js">`. Static files are served directly by the Axum backend — only the type-checking step needs Node.

```bash
cd frontend && npm install        # one-time
npm run watch                     # tsc --watch, rebuilds on every save
npm run typecheck                 # CI gate
npm run build                     # production build (CI + prod host)
```

- `frontend/admin/` — admin SPA modules (`main`, `auth`, `rooms`, `stream-keys`, `files`, `branding`, `dashboard`, `settings`, `webauthn`, `types`)
- `frontend/viewer/` — viewer SPA modules (`main`, `types`, `state`, `session`, `screens`, `ws`, `player`, `livekit`/`conference`, `chat`, `pointer`, `roster`, `layout`, plus `globals.d.ts` for the CDN-loaded LiveKit/OvenPlayer globals)
- `frontend/landing/` — landing page
- `frontend/shared/` — `store.ts` (tiny reactive store), `utils.ts` (typed API wrapper, toast, formatters), `branding.ts` (read-only branding loader), `components.ts` (modal helpers)
- `www/shared/` — design system CSS (`tokens.css`, `components.css`, `utils.css`); conventions in the [Design system](#design-system) section below
- `www/{admin,viewer,landing}/index.html` — HTML markup, page-specific `<style>`, and the `<script type="module">` tag pointing at the compiled bundle
- `www/dist/` — build output (gitignored; CI / prod host produces it)

CDN-loaded runtime deps stay as `<script>` tags in the HTML: OvenPlayer, HLS.js, LiveKit client. No npm runtime deps.

## Design system

CSS tokens in [`www/shared/tokens.css`](www/shared/tokens.css), shared components in `components.css`, utilities in `utils.css`. The admin Branding API overrides `--bg/surface/text/accent/danger/green` at runtime via inline style on `:root`.

Conventions:
- Reference tokens (colors, spacing, radii, motion, z-index) — no hardcoded values in shared CSS.
- No `!important` in shared CSS. `.u-hidden` deliberately omits it so an inline `style.display` set from JS still wins.
- Class names: descriptive, hyphenated. No BEM. No CSS-in-JS.
- Z-index only from the `--z-*` scale; new layers extend the scale, not invent ad-hoc values.
- Page-specific CSS stays inline in the page's `<style>` block. Promote duplicated styles to `components.css`.

## Key implementation details

**Authentication roles:**
- Admin: `POST /api/auth/login` (password → JWT, 7d) — required for all `/api/rooms/*` mutations
- Participant: `POST /api/public/rooms/:slug/join` — returns a scoped JWT for WS + file access
- Presenter role is admin-only (`POST /api/rooms/:id/enter`), never grantable from the public join flow

**Presenter entry handoff.** Admin clicks "Enter Room" → backend creates `role='presenter', is_admitted=1` → admin JS writes `{jwt, participantId}` to `localStorage['viewer_presession_{slug}']` and opens `/watch/{slug}` in a new tab → viewer reads the presession on load, moves it into `sessionStorage['viewer_session_{slug}']`, and deletes the localStorage entry. The localStorage key exists for milliseconds. No public URL grants presenter role.

**Session isolation.** `viewer_session_{slug}` is in `sessionStorage` (per-tab, survives refresh, cleared on tab close); `viewer_name__/pass__{slug}` stay in `localStorage` (shared across tabs — intentional). `viewer_kicked_{slug}` is set on `{type:'kicked'}` or WS close 1008 and is checked at page load *before* WS connect, so a kicked viewer sees "Removed" instantly on refresh. If the sessionStorage flag is lost, the WS hub re-detects `is_kicked=1` and re-expels on reconnect.

**Database:** All queries use prepared statements with `?N` placeholders — no string interpolation. Schema in `backend/schema.sql`, bootstrapped on every startup.

**Rate limiting:** `/api/auth/login` → 5 req/min; `/api/public/rooms/:slug/join` → 30 req/min; passkey ceremonies → 30 req/min in a separate bucket so an OS prompt doesn't burn the login budget. Uses `tower_governor` with `SmartIpKeyExtractor` (honours `X-Forwarded-For` from Caddy).

**Error handling:** `AppError::Internal` and `AppError::BadGateway` return a generic message to the client; actual error is logged server-side only.

**LiveKit:** Hand-rolled, no official Rust SDK. Token generation and RoomService calls are in `src/livekit.rs`.

**Public participant status.** `GET /api/public/rooms/:slug/status/:participantId?token=…` returns `{admitted, kicked, room_status: 'scheduled|live|ended'}`. Companion SSE stream at `/api/public/rooms/:slug/waiting/events/:participantId` emits `admitted`, `kicked`, `room_ended`, `ping` — waiting-room clients drive the full state machine from SSE alone without holding a WS open.

**Moderation audit.** Kick and mute are logged via `tracing::info!` with `room_slug`, `actor_id`, `target_id` for after-the-fact audits. If LiveKit `remove_participant` fails the backend retries once after 250 ms and `error!`s on the second failure — the DB `is_kicked=1` flag and WS force-close happen first, so UI state is correct even when LiveKit is momentarily unreachable.

**Farbplay room-link SRT playback** (`src/routes/watch.rs`, GitHub #165). `GET /api/watch/:slug?participantId=&token=` lets the native SRT viewer (Farbplay) connect from a room link instead of a raw `srt://` URL. The flow mirrors the browser viewer: Farbplay first `POST /api/public/rooms/:slug/join`s to become a `participants` row (password is checked there, not here), waits on the existing admission SSE (`…/waiting/events/:pid`) if the room has a waiting room, then calls this **admission-gated** endpoint. It returns `{srt: {host, port, streamid, latency}, ttlSeconds, title}` where `streamid` is `default/live/<key_token>?policy=<b64url>&signature=<b64url-hmac-sha1>`, signed with `OME_SIGNED_POLICY_SECRET` and expiring after ~30 s (`url_expire`). **OME signs the `srt://`-prefixed URL** (`srt://default/live/<key>?policy=…`, scheme + vhost as host), so the backend must HMAC that form even though the client sends only the path. OME validates it via the `<SignedPolicy>` block (scoped to the SRT publisher only). The signed streamid is minted **only for an admitted, non-kicked participant**: missing `participantId`/`token` or kicked/not-yet-admitted → **403**; unknown participant / wrong token / wrong slug / ended / expired / no stream key → **404**. A kicked viewer therefore can't reconnect (the backstop behind the SSE self-disconnect; no server-side SRT sever today — contract O1/O2). **Security caveat:** this gives expiry/replay-limiting, *not* secrecy — Farbstrom's OME stream name *is* the ingest stream key (`OutputStreamName=${OriginStreamName}`), so the key is in the streamid in plaintext (and is already handed to web viewers on join). Decoupling the playback identity from the ingest key is a separate follow-up.

## CI

GitHub Actions runs on push/PR (`.github/workflows/ci.yml`):
- **build** — `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, `cargo test`.
- **audit** — `cargo audit` (advisory DB check).
- **frontend** — `npm run typecheck`.
- **licenses** — regenerates `docs/THIRD_PARTY_NOTICES.md` via `cargo about` (pinned 0.9.0) and fails if the diff is non-empty.

## Useful reference docs

- `README.md` — what Farbstrom is and what it does
- `docs/DEPLOYMENT.md` — architecture diagram, tech stack, ingest protocols, deployment/ops

## Recommended tests to add

Thin areas in the integration suite worth regression coverage:
1. Viewer JWT → presenter endpoints (`/conference/kick`, `/conference/mute`) → 403.
2. `POST /api/rooms/:id/enter` (admin JWT) produces `role='presenter' AND is_admitted=1`; no public endpoint reaches the same state.
3. Kick blocks re-join by case-insensitive name match (`POST /api/public/rooms/:slug/join` → 403).
4. WS hub rejects kicked participants — `{type:'kicked'}` frame, close 1008.
5. Webhook HMAC: wrong signature → 401; tampered body → 401.
6. Rate limiter: 6th `/api/auth/login` in a minute → 429 (requires the real HTTP server, not `TestServer`, so `ConnectInfo` is populated).
7. Status endpoint shape: `GET /api/public/rooms/:slug/status/:pid?token=…` → `{admitted, kicked, room_status}` for each of waiting/admitted/kicked/ended.

## Gotchas

Non-obvious facts that aren't derivable from reading the code.

**LiveKit**
- `entrypoint.sh` generates `/livekit.yaml` with a `keys:` map (`KEY: SECRET`) — the **space after the colon** is required (YAML), else LiveKit boots with no auth and only logs "Could not parse keys". The backend must mint tokens with the same `LIVEKIT_API_KEY`/`LIVEKIT_API_SECRET`. Keys are inlined into the YAML so the LiveKit process needs no key secrets in its env (`supervisord.conf` strips them with `env -u`).
- No upstream Rust SDK — `src/livekit.rs` is hand-rolled (AccessToken JWT + RoomService over `reqwest`) against `LIVEKIT_INTERNAL_URL` (HTTP, not WSS).
- Caddy `/livekit/*` block needs `header_up Host {upstream_hostport}` for WebSocket signaling to work through the proxy ([`caddy/Caddyfile`](caddy/Caddyfile)).

**OvenPlayer**
- `ovenplayer.js` does NOT bundle `hls.js` — load it separately or LLHLS fails silently.
- `controls: false` is a silent no-op. Hide the UI via CSS `.op-ui-container { display: none !important }`.
- Error/notification overlay sits OUTSIDE `.op-ui-container` — also hide `.op-message-container, .op-notification-container`.
- LLHLS + Safari + H.265 fails (Safari MSE blocks HEVC) — rely on WebRTC-first with LLHLS autoFallback.

**Player sizing**
- CSS `aspect-ratio` is unreliable in flex containers — the viewer uses JS `sizePlayer()` for exact 16:9 pixel dimensions.
- iOS orientation change: call `sizePlayer()` at 0/50/150/300/500 ms because iOS animates rotation over ~300 ms and dimensions are stale mid-transition.

**iOS Safari**
- `HTMLMediaElement.volume` is read-only — volume is hardware-only; the slider is hidden on mobile.
- Viewport meta needs `maximum-scale=1.0, user-scalable=no` to prevent auto-zoom on rotation.

**Timezones**
- `expires_at` is stored as a UTC ISO string. Admin `datetime-local` is converted both ways. Rooms created before this fix may be off by the UTC offset — re-save them in admin to correct.

**Docker (single container)**
- Everything is baked into the image — `docker restart` does NOT pick up code/config changes. Rebuild: `make dev` (selects `docker-compose.dev.yml`, builds from source). `Server.xml`, `caddy/Caddyfile`, `supervisord.conf`, and the compiled `www/dist` are all baked in, so editing them on the host needs a rebuild too (except `./www` while the dev overlay's bind mount is active).
- `$` in `.env` values must be doubled (`$$`) — Compose interpolates `$VAR`.
- Start order is supervisord **priority**, not `depends_on`: Valkey → backend → OME/LiveKit → Caddy, so the admission webhook (backend, `localhost:4001`) is up before OME accepts ingests. Admission is fail-closed — if the backend is down, ingests are denied (no unauthorised streaming).
- The OME admission webhook URL is `localhost:4001` (env-overridable `OME_WEBHOOK_URL` in `Server.xml`); LiveKit's Redis is `localhost:6379` (generated `livekit.yaml`) — no Docker service names resolve inside the single container.
- Privilege & secrets: Caddy/OME/LiveKit run as root (privileged ports / TURN); the backend (`user=app`) and Valkey (`user=valkey`) drop privileges. `supervisord.conf` removes the backend-only secrets (`JWT_SECRET`, `ADMIN_PASSWORD`, `LIVEKIT_API_KEY/SECRET`) from the third-party processes via `env -u` — add any new such secret to those four `-u` lists.
- Persist Caddy's `caddy_data` volume (`/root/.local/share/caddy`): it holds the internal CA + Let's Encrypt certs. Losing it re-issues certs on every recreate (Let's Encrypt rate limits) / regenerates the local CA.
- Keep the UDP RTC range narrow (50000-50100) — Docker writes one iptables rule per port; wide ranges make `compose up/down` take minutes.
- Valkey is the BSD-3 fork of Redis 7.2; LiveKit talks to it as a plain RESP server, so the swap from upstream Redis is invisible.
