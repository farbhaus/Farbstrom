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
`.github/workflows/docker-single.yml` **only on a `v*.*.*` release tag** —
immutable `:vX.Y.Z` + `:vX.Y` for reproducible prod pinning, plus `:latest`
moved to the newest release. Pushes to `main` and PRs build the image as a CI
safety net but publish nothing (linux/amd64). It is self-contained — the Dockerfile compiles
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
- `src/presence.rs` — in-memory presence registry for native SRT (Farbplay) viewers; their admission SSE connection is the heartbeat (browser viewers use the WS in `ws.rs` instead). Current Farbplay builds hold a WS *as well*, marked `client: "farbplay"` — see the roster gotcha below
- `src/signed_policy.rs` — OME SignedPolicy streamid minting for SRT playback; the single signing helper behind both `/api/watch/:slug` (Farbplay) and the admin `/api/stream-keys/:id/srt-playback`
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

- `frontend/admin/` — admin SPA modules (`main`, `auth`, `rooms`, `stream-keys`, `files`, `branding`, `dashboard`, `settings`, `webauthn`, `types`). **A new `data-action` needs registering in `initDelegatedClicks()` in `main.ts`** — one delegated listener routes clicks to each module through an explicit `switch`, so an unlisted action is silently inert (the button just does nothing; no console error)
- `frontend/viewer/` — viewer SPA modules (`main`, `types`, `state`, `session`, `screens`, `ws`, `player`, `livekit`/`conference`, `chat`, `pointer`, `roster`, `layout`, `scopes`/`scope-draw`/`scope-color`, `tour`, plus `globals.d.ts` for the CDN-loaded LiveKit/OvenPlayer globals). **A new toolbar button needs registering in `layout.ts`** — `TOOLBAR_EXTRA_IDS` (the mobile ⋯ sheet) and one of `LANDSCAPE_{LEFT,RIGHT}_IDS` (the short-landscape side pills). Miss them and the button simply vanishes on mobile, silently, exactly like an unlisted `data-action` does in admin
- `frontend/landing/` — landing page
- `frontend/shared/` — `store.ts` (tiny reactive store), `utils.ts` (typed API wrapper, toast, formatters), `branding.ts` (read-only branding loader), `components.ts` (modal helpers)
- `www/shared/` — design system CSS (`tokens.css`, `components.css`, `utils.css`); conventions in the [Design system](#design-system) section below
- `www/{admin,viewer,landing}/index.html` — HTML markup, page-specific `<style>`, and the `<script type="module">` tag pointing at the compiled bundle
- `www/dist/` — build output (gitignored; CI / prod host produces it)

CDN-loaded runtime deps stay as `<script>` tags in the HTML: OvenPlayer, HLS.js, LiveKit client. No npm runtime deps.

## Design system

CSS tokens in [`www/shared/tokens.css`](www/shared/tokens.css), shared components in `components.css`, utilities in `utils.css`. The admin Branding API overrides `--bg/surface/text/accent/danger/green` at runtime via inline style on `:root`.

**`tokens.css` is the only file allowed to contain raw values.** Everything else
— `components.css`, `utils.css`, and every page's `<style>` block — references
tokens. The one sanctioned exception is `.btn-tab { border-radius: 0 }`, a
deliberate reset.

**Radii — pick by the element's role, never by eye.** The scale has only three
size steps, deliberately far enough apart to be distinguishable. A previous
`4/6/8/10/12` ramp drifted badly (43 hardcoded values, 28 of them restating a
token) precisely because 6px, 8px and 10px are indistinguishable, so nobody
could tell which was correct.

| Token | Value | Use for |
|---|---|---|
| `--r-inset` | 4px | micro-elements nested inside a control: swatches, tags, status badges |
| `--r-control` | 8px | buttons, inputs, selects, icon buttons, chips |
| `--r-card` | 12px | cards, panels, tiles, modals, entry cards, toasts, toolbars, sheets |
| `--r-pill` | 999px | nav pills, count badges, progress bars and tracks |
| `--r-circle` | 50% | status dots, slider thumbs |

**Typography** is `--fs-2xs/xs/sm/base/md/lg/xl/2xl` (10/11/12/13/15/18/20/28px);
`--fs-base` is 13px, the product's actual body size. Weights are
`--fw-normal/semi/bold`. If a size isn't on the scale, round to the nearest
step rather than adding one. The uppercase micro-label is **one** recipe —
`.label-micro` / `.label-micro-sm` in `components.css`, which also owns the
size/weight/tracking/casing of every named label selector (`.section-title`,
`.stat-label`, `.panel-tab`, …). Page CSS keeps only its own layout and color.

**Derived colors must use `color-mix()` off a brandable base** (`--accent-tint`,
`--green-tint`, `--text-hover`, `--tint-hover`, …). A hardcoded tint freezes
when an admin rebrands — that was a live bug in four places. `--media-bg` /
`--on-media` are the deliberate exceptions: video chrome stays black/white.

Conventions:
- Reference tokens (colors, spacing, radii, typography, motion, borders, z-index) — no hardcoded values outside `tokens.css`.
- No `!important` in shared CSS. `.u-hidden` deliberately omits it so an inline `style.display` set from JS still wins.
- Class names: descriptive, hyphenated. No BEM. No CSS-in-JS.
- Z-index only from the `--z-*` scale; new layers extend the scale, not invent ad-hoc values. The scale includes a sub-100 band (`--z-tile-*`, 5–8) for the stacking context *inside* a video tile.
- Every button uses `--bw-control` (1.5px); dividers and containers use `--bw-hair` (1px).
- Page-specific CSS stays inline in the page's `<style>` block. Promote duplicated styles to `components.css`.
- Looping animations must be guarded by `@media (prefers-reduced-motion: reduce)`.

**Enforced by CI.** `./scripts/design-lint.sh` (run it locally before pushing)
fails on a hardcoded radius, z-index, font-size, font-weight, letter-spacing or
color outside `tokens.css`, and on any token that is referenced-but-undefined or
defined-but-unused. It allowlists exactly three things: the `.btn-tab` radius
reset, `--focus-aspect` (set per element from `conference.ts`), and
`--panel-w*`/`--strip-w*` (read via `getComputedStyle` in `layout.ts`, so no
`var()` reference exists to find). The conventions above were documented long
before this gate and drifted anyway — hence the gate.

**File rows are two different components, named differently on purpose.**
`.file-row` is admin's 8-column grid table row (`frontend/admin/files.ts`);
`.shared-file` (+ `-name/-size/-dl/-show/-del`) is the viewer's flex chip row in
chat and the Files tab (`frontend/viewer/chat.ts`). They both used to be called
`.file-row`, which was harmless only because they never shared a page — and it
meant neither could be promoted to `components.css`. Keep them distinct.

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

**First-run room tour** (`frontend/viewer/tour.ts`, GitHub #230). Six spotlight
steps over the room chrome, shown once per device and never again, and only to
participants — a `role: 'presenter'` host is never offered it (and isn't marked
as having seen it, so their browser still gets it if it later joins a room as a
participant). The seen
record is `localStorage['farbstrom_tour_v1']` **plus** a `farbstrom_tour` cookie,
either of which counts; neither is slug-scoped, because every room is the same
origin and one record covers all of them. It is written when the tour is
*offered*, so a mid-tour reload doesn't re-prompt. A private window or a fresh
browser profile is a first visit by definition — that is why the tour can look
like it never sticks while you're testing it. `main.ts` starts it after the PTT
notice and the cam/mic prompt resolve (`showConfPrompt()` returns a promise for
exactly that), so dialogs never stack.

Each step names the selectors it points at; the spotlight covers whichever of
them are visible, and a step with none left is dropped before the tour starts.
That is what keeps it honest across the three toolbar layouts and every room
shape — no pointer button outside focus view, no player controls in an app-only
room — and in compact (mobile) mode the last step points at the ⋯ sheet instead
of the controls hidden inside it. The tour never operates the room: a blocker
plus `inert` on `#app` see to that, and the chat panel (which one step opens) is
put back on the way out. New steps are a `TourStep` in `buildSteps()`.

**The privacy page is the cookie disclosure** (`www/privacy/index.html`, gh
#247). It is linked from the landing page, the join screen and the device
picker, and it enumerates every key this app writes to the browser —
`farbstrom_tour` (the tour's cookie, the **only** cookie set anywhere), the
`sessionStorage` session, and the `localStorage` room/tool preferences including
`viewer_scopes`. Anything new that writes to a cookie, `localStorage` or
`sessionStorage` belongs on that page in the same commit; it shipped claiming
"not in cookies" for a whole release after the tour landed, and claiming the
room password was saved behind a checkbox that has never existed.

**The ? toolbar button** (`frontend/viewer/shortcuts.ts`) opens the shortcuts
sheet, and offers the tour as its second button rather than launching it. The
sheet is generated from `SHORTCUTS` — the same list the key handler reads — so a
new single-key shortcut cannot ship undocumented.

**Public participant status.** `GET /api/public/rooms/:slug/status/:participantId?token=…` returns `{admitted, kicked, room_status: 'scheduled|live|ended'}`. Companion SSE stream at `/api/public/rooms/:slug/waiting/events/:participantId` emits `admitted`, `kicked`, `room_ended`, `ping` — waiting-room clients drive the full state machine from SSE alone without holding a WS open.

**Moderation audit.** Kick and mute are logged via `tracing::info!` with `room_slug`, `actor_id`, `target_id` for after-the-fact audits. If LiveKit `remove_participant` fails the backend retries once after 250 ms and `error!`s on the second failure — the DB `is_kicked=1` flag and WS force-close happen first, so UI state is correct even when LiveKit is momentarily unreachable.

**Farbplay room-link SRT playback** (`src/routes/watch.rs`, GitHub #165). `GET /api/watch/:slug?participantId=&token=` lets the native SRT viewer (Farbplay) connect from a room link instead of a raw `srt://` URL. The flow mirrors the browser viewer: Farbplay first `POST /api/public/rooms/:slug/join`s to become a `participants` row (password is checked there, not here), waits on the existing admission SSE (`…/waiting/events/:pid`) if the room has a waiting room, then calls this **admission-gated** endpoint. It returns `{srt: {host, port, streamid, latency}, ttlSeconds, title}` where `streamid` is `default/live/<key_token>?policy=<b64url>&signature=<b64url-hmac-sha1>`, signed with `OME_SIGNED_POLICY_SECRET` and expiring after ~30 s (`url_expire`). **OME signs the `srt://`-prefixed URL** (`srt://default/live/<key>?policy=…`, scheme + vhost as host), so the backend must HMAC that form even though the client sends only the path. OME validates it via the `<SignedPolicy>` block (scoped to the SRT publisher only). The signed streamid is minted **only for an admitted, non-kicked participant**: missing `participantId`/`token` or kicked/not-yet-admitted → **403**; unknown participant / wrong token / wrong slug / ended / expired / no stream key → **404**. A kicked viewer therefore can't reconnect (the backstop behind the SSE self-disconnect; no server-side SRT sever today — contract O1/O2). **Security caveat:** this gives expiry/replay-limiting, *not* secrecy — Farbstrom's OME stream name *is* the ingest stream key (`OutputStreamName=${OriginStreamName}`), so the key is in the streamid in plaintext (and is already handed to web viewers on join). Decoupling the playback identity from the ingest key is a separate follow-up.

**Every SRT playback target is signed, by one helper** (`src/signed_policy.rs`, GitHub #226). `<SignedPolicy>` is enabled for the SRT publisher, and OME rejects an **unsigned** streamid exactly as it rejects an invalid one — so there is no such thing as a static, copy-pasteable SRT playback URL. Both callers mint through `signed_policy::sign_streamid(secret, stream_name, ttl)`: `/api/watch/:slug` (Farbplay, 30 s) and `GET /api/stream-keys/:id/srt-playback` (admin JWT, 300 s), which backs the **Generate** button on the Stream Keys tab's playback SRT row. The frontend cannot build this URL — the secret is backend-only — and must **percent-encode the streamid** inside the outer `srt://…?streamid=` URL, or a player reads the streamid's own `?policy=…&signature=…` as sibling SRT socket options. `url_expire` is checked at connect time only, so a session established inside the TTL keeps running.

**Farbplay is marked on the WS roster** (`client: "farbplay"`, GitHub #227). Farbplay used to open no WebSocket at all and was identified purely by *SSE presence without WS presence*; pointer collaboration changed that, and the viewer's Farbplay roster section silently emptied. Its auth frame now carries `"client": "farbplay"` (absent ⇒ browser), which `ws.rs` whitelists via `normalize_client`, stores on `WsParticipant` — **on the reconnect branch too**, or a dropped socket demotes the viewer to the browser list — and republishes in `participants:update`. `roster.ts` renders marked entries in the Farbplay section, unioned with the SSE-only list that older Farbplay builds still produce. Farbplay entries stay in `viewerStore.roster`, so pointer cursors and conference tiles treat them like any other participant.

## CI

GitHub Actions runs on push/PR (`.github/workflows/ci.yml`):
- **build** — `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, `cargo test`.
- **audit** — `cargo audit` (advisory DB check).
- **frontend** — `npm run typecheck`, `npm run build`, then `./scripts/design-lint.sh` (see below).

Third-party notices are **not** a CI gate. `docs/THIRD_PARTY_NOTICES.md` is a
generated file, and gating it per-PR turned CI red on every dependabot cargo
bump while blocking nothing (`main` is unprotected, so auto-merge shipped the
drift regardless — that is how a stale line reached `main` and broke unrelated
PRs). It is now produced where it actually matters: the Dockerfile's
backend-builder stage generates it from the tree it just compiled and ships it
at `/usr/share/doc/farbstrom/THIRD_PARTY_NOTICES.md` inside the image, and the
`notices` job in `docker-single.yml` refreshes the repo copy on `v*.*.*` tags
only. Regenerate by hand any time with the `cargo about` command above; nothing
fails if you don't.

## Updating pinned components

Dependabot watches `cargo`, `github-actions`, and the literal Docker `FROM` bases
(`rust`, `ubuntu`, `node`). **It cannot see the four component pins** — they are `ARG`s
interpolated into `FROM` (`FROM caddy:${CADDY_VERSION}`), which Dependabot does not
resolve. Nor does anything watch the CDN `<script>` tags. Both lists below are **manual**;
check them when you touch the stack.

**Component versions — change all three places together**, or a local `make dev` build
silently disagrees with what CI bakes:

| Where | What it drives |
|---|---|
| [`Dockerfile`](Dockerfile) `ARG *_VERSION` | **the source of truth — what CI bakes into the published image** |
| [`docker-compose.dev.yml`](docker-compose.dev.yml) `args:` | local source builds (`make dev`), via `*_TAG` from `.env` |
| [`.env.example`](.env.example) `*_TAG` | the documented default for both |

The four are `OME_VERSION`, `LIVEKIT_VERSION`, `CADDY_VERSION`, `VALKEY_VERSION`. CI passes
no `build-args`, so `Dockerfile` alone decides the published image. `VALKEY_TAG` may carry
an image suffix (`9.1.1-alpine`); the Dockerfile strips it to the source tag.

**CDN scripts** — in *both* [`www/viewer/index.html`](www/viewer/index.html) and
[`www/admin/index.html`](www/admin/index.html) (livekit-client is viewer-only). Keep the
pages in sync. **OvenPlayer is pinned exactly**, the others float on minor: OvenPlayer's
DOM class names are load-bearing for the chrome-hiding CSS in each page's `<style>` block
and have drifted before, so it must never move on its own.

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
- **Browser support, measured on OME v0.21.0** (WebRTC delivery, real 1080p ingests): H.264 plays in Chrome, Safari **and** Firefox; H.265 plays in Chrome and Safari, not Firefox. The H.265 tested was `hvc1.4.10.L120.bd.8` — profile 4 (Range Extensions, 4:2:2 10-bit), decoded via Apple Silicon hardware. So HEVC over WebRTC is **not** Farbplay-only, and `deliveryMode` is a genuine per-room choice rather than a codec workaround.
- LLHLS + Safari + H.265 historically failed because Safari MSE rejects the `hev1` sample entry. **v0.21.0 changed the packaging** (upstream #2258): verified that an H.265 ingest now yields `CODECS="hvc1…"` in the LL-HLS master playlist and an `hvc1`/`hvcC` sample entry in the fMP4 init segment — the form Safari requires. **Whether Safari decodes H.265 over LL-HLS is still untested** (the tests above all ran through WebRTC). Test it before relying on LLHLS for an H.265 room, and record the result here.

**WebRTC transport (OME)**
- Both `<IceCandidates>` blocks in [`ome/origin_conf/Server.xml`](ome/origin_conf/Server.xml) **force the built-in TURN relay** (`<TcpRelayForce>true</TcpRelayForce>` — v0.21.0's rename of the deprecated `<TcpForce>`). This looks wasteful and it is tempting to "fix"; **don't, without testing Firefox.** Direct ICE was tried on v0.21.0 (`${PublicIP}` candidates + RFC 6544 TCP ICE on `10000/tcp`, `TcpRelayForce=false`) and Chrome and Safari worked while **Firefox never nominated a candidate**, looping `Added session`/`Removed session` at OvenPlayer's 8 s `connectionTimeout` forever. Offering the relay *alongside* direct candidates (`<DefaultTransport>all</DefaultTransport>`) does not rescue it either: behind Docker NAT, OME can only advertise the relay at `${PublicIP}` and `172.x`, and neither is reachable as `localhost`. Relay-forced is the only configuration verified to work in all three browsers.
- **Adding a new ICE port means three edits**: `Server.xml`, the `EXPOSE`/`ports:` entries, and `FW_TCP`/`FW_UDP` in [`deploy.sh`](deploy.sh).
- Debugging ICE: OME logs `nominated candidate` + `DTLS peer certificate verified` on success. A repeating `Added session`/`Removed session` pair with neither line in between is ICE connectivity failure, not codec or signalling — signalling clearly succeeded if a session was created at all.
- `OME_HOST_IP` (feeding `<Distribution>`) is derived in [`entrypoint.sh`](entrypoint.sh) from `PUBLIC_HOST`, so its export must stay *below* where `PUBLIC_HOST` is resolved. It previously read a `DOMAIN` var nothing sets and silently resolved to `localhost`.

**Codecs**
- Video is `<Bypass>true</Bypass>` — never transcoded. The ingest codec is exactly what every viewer's browser must decode, so codec choice is a product decision, not a server one. Audio *is* transcoded (AAC for LLHLS/SRT, Opus for WebRTC) with `BypassIfMatch` short-circuits.
- AV1 (OME v0.21.0) is **WHIP / enhanced-RTMP only — SRT cannot carry it**, because OME's SRT ingest is MPEG-TS and its demuxer has no AV1 support. Transcoding *to* AV1 isn't viable either: the only encoder is libaom, and upstream disabled its realtime/RC/tiling options (`#if 0`) over a crash. Two guards exist because an undecodable stream otherwise looks like a black tile: `findUnplayableCodec()` in [`frontend/viewer/player.ts`](frontend/viewer/player.ts) reads `CODECS` from the LL-HLS playlist and `MediaSource.isTypeSupported()`s each one (LL-HLS only — WebRTC negotiates codecs in SDP, so there's no equivalent pre-flight), and `browserCodecWarning()` in [`frontend/admin/dashboard.ts`](frontend/admin/dashboard.ts) flags AV1/H.265 ingests on the dashboard.

**Player sizing**
- CSS `aspect-ratio` is unreliable in flex containers — the viewer uses JS `sizePlayer()` for exact 16:9 pixel dimensions.
- iOS orientation change: call `sizePlayer()` at 0/50/150/300/500 ms because iOS animates rotation over ~300 ms and dimensions are stale mid-transition.

**Scopes** (viewer video analysis, gh #229)
- **What ships is one scope at a time, locked to Rec.709 / IRE and a 16:9 drawing area.** Luma waveform, RGB parade and vectorscope (which has 1–16× zoom, by button or wheel). `drawHistogram` and `drawFalseColor` are complete and tested but deliberately not offered — adding them back is one line in `PANELS` in `scopes.ts`. Likewise the working-space and scale selectors were removed: `scope-draw`/`scope-color` are still fully parameterised (Display-P3, Rec.2020, PQ-in-nits, ARRI LogC), so restoring them is a UI change, not a maths one.
- **The scopes button is offered only when this browser has something to sample** — a stream key delivered to the browser, or a presenter-displayed file (`scopesAvailable()` in `scopes.ts`, gh #246). A call-only room (no stream key) and an app-only (SRT) room both leave it hidden; it appears the moment a key is attached, because the check runs off a `viewerStore` subscription. The gate is a `no-scopes` class on `<body>`, **not** `u-hidden` on the button — `layout.ts` re-parents that button into the ⋯ sheet and the landscape pills, and a body-level rule survives the move. Availability is deliberately separate from the saved `open` pref: a call-only room parks the window instead of clearing the preference. `shortcuts.ts` (the `?` sheet) and `tour.ts` read the same predicate, so neither documents a control the room doesn't have.
- **A canvas readback is display-mapped, not source-referred.** The browser tone-maps HDR into the SDR canvas *before* `getImageData` sees anything, so absolute nits are not measurable on that path. That is *why* the UI is pinned to Rec.709/IRE — for an SDR stream the display output is the signal, so there is nothing to caveat. **Anything that re-exposes the PQ/HDR scales has to restore the provenance labelling with them** (a `display-mapped` badge and `~`-prefixed nits), or the scope will quietly report tone-mapped values as measurements. The accurate path is WebCodecs: `VideoFrame.copyTo()` with **no** `format` option returns the frame's original planar data, `I420P10` included. Not implemented — whether a `VideoFrame` built from a `<video>` exposes a readable `format` (rather than an opaque GPU frame) varies by browser and is **unmeasured**; measure it before building on it.
- **The pinned TypeScript DOM lib ships pre-HDR WebCodecs enums.** `VideoTransferCharacteristics` has no `'pq'`/`'hlg'`, `VideoColorPrimaries` no `'bt2020'`, `VideoPixelFormat` no `'I420P10'` — comparing against those literals is a **compile error** even though browsers emit exactly those values. `scope-color.ts` reads them as plain `string` and matches by substring.
- **There is no `rec2100-pq` or `rec2020` canvas colour space** — `colorSpace` accepts only `srgb` and `display-p3`, so a Rec.2020 working space is a numeric interpretation of a P3 readback. `colorType: 'float16'` (real HDR headroom) is Chrome-flag-only with no Safari, hence deliberately unused.
- **`.u-hidden` cannot hide anything styled by an id selector** — it's a class (0-1-0) and deliberately carries no `!important` (see the Design system section). The scopes empty state uses a `.no-source` class on the window instead. Silent no-op if you get this wrong.
- **The vectorscope skin-tone line is derived from a reference skin RGB, never by rotating the quoted "123°".** That angle is defined in the YUV (U,V) plane, whose axes scale differently from Cb/Cr; the transform is anisotropic and does **not** preserve angles. Rotated naively the line lands at 134.5° — ~11° off, since measured skin clusters at 124–126° in the Cb/Cr plot. Same reason the 75% bar targets are computed by pushing bar RGB through the live `chroma()` rather than hardcoding graticule angles.
- **ARRI's false-colour zones are LogC *exposure* values, not display IRE.** Applied raw to a graded Rec.709 feed, ARRI's 38–42 IRE "18% grey" band lights up ~28% grey. So the `sdr` scale re-anchors the zones to where those tones actually land display-referred (18% grey at 46.1 IRE, skin one stop over at 63.4); the `log` scale carries ARRI's published numbers, for a feed that really is log.
- **On a HiDPI display one device pixel is half a CSS pixel** — that is why the trace and graticule first shipped looking like hairlines. Canvas backing stores are sized `css × dpr`, so anything one device pixel wide renders at 0.5 CSS px on a 2× screen. The trace deposits a `stampSize(dpr)²` block per sample and every `lineWidth` is `dpr × HAIR_PX`; sizes in `scope-draw.ts` are quoted in CSS pixels and multiplied up, never in raw device pixels. Sparse trace cells also get a `TRACE_MIN_ALPHA` floor, or the parts you are squinting at fade out entirely.
- Vectorscope point placement **rounds** rather than `| 0`-truncates. Truncation is right for a bin index but asymmetric about the centre, and exact neutral carries a float epsilon that a high zoom gain multiplies across the pixel boundary — enough to jitter the centre point.
- One `getImageData` per frame, shared by whatever is drawing — never one per scope. `requestVideoFrameCallback` drives the loop where it exists (Firefox has none; rAF fallback), throttled to 15 fps, and idles out entirely when the window is closed or the tab is hidden.
- The window's height is **derived** from its width (`fitGeometry`) to hold the canvas at 16:9, so the resize grip tracks horizontal drag only, and `#scopes-body` carries no padding — padding would break the ratio the height was computed for.
- **Vectorscope zoom magnifies the trace only** — ring, crosshair, skin line and the 75% targets are a fixed reference and stay put, so the trace clips at the panel edge once it outgrows the ring.
- The **1:1 / 1:2 / 1:4 sampling control is the performance dial**, and it does not scale the way pixel count suggests. Accumulation scales with the sample; blitting the trace and stroking the graticule walk the whole canvas regardless, so there is a fixed floor. Measured JS-side at a 920×518 panel, dpr 2: 1:2 gives 1.6–2.3× and 1:4 gives 2.1–3.7×, not 4× and 16×. Optimising further means attacking `blitTrace`, not the sample size.

**iOS Safari**
- `HTMLMediaElement.volume` is read-only — volume is hardware-only; the slider is hidden on mobile.
- Viewport meta needs `maximum-scale=1.0, user-scalable=no` to prevent auto-zoom on rotation.

**SRT playback (OME SignedPolicy)** — all of the below measured against the pinned OME on a real ingest (#226), not inferred from docs.
- **SignedPolicy is all-or-nothing per publisher.** Enabling it for `<Publishers>srt` does not mean "validate a token when one is present": an unsigned streamid is rejected outright, with
  `W SRTPublisher | There is no signature key(signature) in url(srt://default/live/<key>)` and zero bytes delivered. That is what killed the admin Stream Keys playback URL for a whole release — it silently predated the SignedPolicy work in #165. A signed streamid on the same stream returns media and logs `A new session has started playing … on the SRT publisher`.
- **The `/playlist` suffix was never the problem.** `{host}/{app}/{stream}[/{playlist}]` is a valid streamid shape (`srt_stream_url_resolver.cpp` accepts 3 or 4 path parts), and this OME auto-creates an SRT playlist literally named `playlist` — `SRTPublisher | A SRT playist [playlist] has been created`. (Upstream docs say `master`; they disagree with the shipped binary, so trust the log.) The old admin URL failed *only* for lack of a signature. Farbstrom addresses the stream directly, matching what `/api/watch` signs.
- **Percent-encoding the streamid is safe and necessary.** OME calls `ov::Url::Decode` on every streamid before parsing, and SRT clients decode the `streamid=` query value too — verified by OME logging the fully-decoded `…?policy=…&signature=…`. What OME deprecated is the *`srt://…`-as-streamid* form, not encoding. Without encoding, the streamid's own `?`/`&` are parsed as sibling SRT socket options and the URL breaks.
- The signature covers the whole path, so forms are **not** interchangeable — you cannot append or strip a playlist segment on a signed streamid.

**SRT encryption** (DB-managed runtime toggle, gh #208)
- Toggled at runtime from the admin **Settings** tab (`POST /api/stream-keys/srt-encryption` `{ingest, playback}`). **No `.env` — the DB is the sole source of truth.** The two legs are **independent**: ingest (encoder → server, 9999) and playback (server → viewers, 9998) each have their own `srt_ingest_enabled` / `srt_playback_enabled` flag + generated passphrase (`srt_{ingest,playback}_passphrase`) in the `settings` table (`src/srt.rs`, hex via `rand`). Playback is the exposed leg (public internet), so the UI recommends it; ingest is usually a trusted network. `pbkeylen` is fixed at 16 (`srt::PBKEYLEN`). Absent flag ⇒ that leg disabled.
- The UI is **two checkboxes + one Apply button** (`frontend/admin/settings.ts`): the checkboxes stage a desired state and Apply sends the full `{ingest, playback}` state so OME restarts **once** even when both legs change. The Stream Keys tab still reads `srt-config` to append the passphrase to its SRT URLs, but the toggle lives in Settings.
- **Why it must restart OME, not hot-reload:** OME (v0.21.0) reads its SRT passphrase from `Server.xml` `${env:...}` only at process startup — `SIGHUP` reloads just `logger.xml`, and its REST API returns 403 for bind changes. So the backend can't push a new passphrase into a running OME. The bridge: `Server.xml` is unchanged (still `${env:...}`, one bind per leg); the backend writes both passphrases to `<data>/srt.env`; the `[program:ome]` command is the `ome_start.sh` wrapper that **sources `srt.env` then execs `ome_launcher.sh`**; the handler runs `supervisorctl restart ome` (`srt::restart_ome`) to re-read it. For that restart the unprivileged backend (`user=app`) needs the supervisor socket — `[unix_http_server]` is `chown=app:app chmod=0700` in `supervisord.conf`. The passphrases reach OME **only** via `srt.env` (the wrapper), never the container env.
- OME's SRT passphrase is **bind-level (per-port), not per-stream**. Wire confidentiality only, *not* access control (still the admission webhook + SignedPolicy). Restarting OME briefly drops **every** stream (SRT + browser), and enabling a leg is a **hard cutover** (that leg's encoders / Farbplay clients must reconnect with the passphrase) — the admin UI confirms before applying.
- Reads are live: `srt-config` (admin) and `/api/watch/:slug` (playback → Farbplay) resolve from the DB (`srt::resolve`). A disabled leg writes an empty passphrase to `srt.env`, so its bind stays plaintext.
- Cold-boot ordering: the backend writes `srt.env` from the DB on startup (`srt::init_startup`, which also migrates the pre-split combined `srt_encryption_enabled` flag into the two per-leg flags); `ome_start.sh` waits up to ~10 s for the backend (priority 20) to write it before OME (priority 30) launches; fail-open to unencrypted is safe because ingest admission is fail-closed while the backend is down. Tests set `STREAM_DISABLE_OME_RESTART=1` to skip the `supervisorctl` shell-out.

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
- **Docker Desktop (macOS) silently drops a UDP port publish.** With ~114 published UDP ports, one of them randomly fails to bind on the host per container start — `docker port` still lists the mapping and the container logs `listening on *:9998/SRT`, but nothing on the host holds the port and every packet is dropped before it reaches the container. Observed alternating between 9998 and 9999, which looks exactly like "SRT is broken" (Farbplay can't connect, ffplay hangs, OME logs nothing at all). Diagnose with `lsof -nP -iUDP:9998` — no `com.docker` line means the publish failed; `docker compose restart` re-rolls it. Only affects local dev on Docker Desktop, not Linux hosts.
- Valkey is the BSD-3 fork of Redis 7.2; LiveKit talks to it as a plain RESP server, so the swap from upstream Redis is invisible.
