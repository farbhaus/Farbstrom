# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

Farbstrom is a private low-latency streaming platform for color-grading review sessions. It combines OvenMediaEngine (OME) for broadcast ingest/delivery, LiveKit for participant voice/video, and a Rust/Axum backend for API and session management. The frontend is TypeScript compiled with `tsc` (no bundler, no runtime npm deps) emitted as plain ES modules.

## Commands

### Backend (Rust — `backend/`)

```bash
cargo check                              # fast type check
cargo build --release                    # production binary
cargo test                               # run the whole suite (unit + integration)
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
| `OME_API_URL` | `http://localhost:8081/v1` | OME admin API. Must be `http://`: the backend's HTTP client is built without TLS (both upstreams are container-local), and `config.rs` refuses anything else at startup. |
| `LIVEKIT_INTERNAL_URL` | `http://localhost:7880` | LiveKit's HTTP API (RoomService), for the backend only — never published or proxied to browsers. `http://` only, as above. |
| `LIVEKIT_URL` | `ws://localhost:7880` | WebSocket URL sent to browser clients. In the container, **derived from `PUBLIC_HOST`** by `entrypoint.sh` (`wss://<PUBLIC_HOST>/livekit`). |
| `PUBLIC_ORIGIN` | `http://localhost:4001` | WebAuthn RP origin/ID — must match the browser origin exactly. In the container, **derived from `PUBLIC_HOST`** (`https://<PUBLIC_HOST>`). |
| `SITE_ADDRESS` | `localhost` | Caddy site address for the container's own Caddy. Set to your domain for standalone TLS, or `:80` (plain HTTP) to run behind an external TLS proxy. |
| `PUBLIC_HOST` | `$SITE_ADDRESS` | Browser-facing host that `PUBLIC_ORIGIN`/`LIVEKIT_URL` derive from. Defaults to `SITE_ADDRESS` (standalone). **Required** when `SITE_ADDRESS=:80` — a bare port is not a valid host, so without it the backend panics. |
| `WEB_ROOT` | `/www` | Root of the built frontend (static mounts, `/privacy`, `/favicon.ico`, and the two HTML documents `routes::pages` rewrites). The Dockerfile copies there and the dev overlay bind-mounts `./www:/www`, so deployments never set it; it exists so tests can point at a fixture tree. |
| `OME_LOG_PATH` | `/var/log/ovenmediaengine/ovenmediaengine.log` | OME's on-disk log, tailed by the admin dashboard's **OME Logs** panel (`GET /api/admin/ome-logs`, gh #261). OME's REST API has no log endpoint, so this reads the file sink set in the image's `origin_conf/Logger.xml` (root-owned but world-readable). Like `WEB_ROOT`, deployments never set it; it exists for test fixtures. |
| `WEB_BIND` | `0.0.0.0` | Host interface the published HTTP/HTTPS ports bind to. Set to `127.0.0.1` when behind an external proxy so the plain-HTTP port isn't internet-reachable (Docker bypasses ufw). |
| `TRUSTED_PROXIES` | loopback only | Peers the container's Caddy trusts to report the client address (`X-Forwarded-For`). Set `private_ranges` behind an external proxy — `deploy.sh --behind-proxy` does — or every client is seen as the proxy's Docker bridge address and shares one rate-limit bucket. See the global block in [`caddy/Caddyfile`](caddy/Caddyfile). |
| `OME_ICE_ADDRESS` | `*`, or `127.0.0.1` when `PUBLIC_HOST` is localhost | ICE candidate address OME advertises to WHIP encoders. Derived in `entrypoint.sh`; set explicitly (e.g. a LAN IP) to push to a dev box from another machine. |
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
  ├─ LiveKit JS SDK — cam/mic/screen  (Caddy /livekit/rtc* → localhost:7880, wss; nothing else of 7880)
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
- `src/app.rs` — `build_app(state) -> Router`: the **whole** router (API + WS + static + SPA fallback + Cache-Control/Trace layers). `main.rs` and the tests build the same one. Anything mounted only in `main.rs` is by definition untestable, which is how `ws.rs` went 1350 lines with no coverage
- `src/config.rs` — `AppConfig::from_env`, secret length validation (fail-fast)
- `src/state.rs` — `AppState` (Arc'd, cloned into handlers)
- `src/db.rs` — R2D2 SQLite pool (8 connections), WAL mode, schema bootstrap from `schema.sql`. Connection-scoped pragmas go in `with_init` so they apply to **every** pooled connection — `foreign_keys`, `synchronous`, `busy_timeout`. `journal_mode = WAL` is set once instead, being persisted in the file header
- `src/error.rs` — `AppError` + `IntoResponse` impl; central error → HTTP mapping
- `src/events.rs` — typed WS event payloads shared between hub and routes
- `src/auth.rs` — the `AdminAuth` extractor (Bearer header only), admin token minting (7 d, 90 d trusted), and the files-only link token behind `AdminFileAccess` (`?ft=`)
- `src/jwt.rs` — HS256 JWT sign/verify over `hmac` + `sha2`, for the admin token and LiveKit's. Replaced the `jsonwebtoken` crate, whose RSA/EC support brought two advisories `cargo audit` had to ignore; verification refuses any `alg` but `HS256`, and a test pins that tokens minted by the old crate still verify
- `src/livekit.rs` — hand-rolled LiveKit client: AccessToken JWT minting + RoomService HTTP
- `src/ws.rs` — WebSocket hub, broadcast channels per room; per-connection message limits (`Limits`) and a 16 KiB frame cap
- `src/presence.rs` — in-memory presence registry for native SRT (Farbplay) viewers; their admission SSE connection is the heartbeat (browser viewers use the WS in `ws.rs` instead). Current Farbplay builds hold a WS *as well*, marked `client: "farbplay"` — see the roster gotcha below
- `src/signed_policy.rs` — OME SignedPolicy streamid minting for SRT playback; the single signing helper behind both `/api/watch/:slug` (Farbplay) and the admin `/api/stream-keys/:id/srt-playback`
- `src/tasks.rs` — background pollers: OME stream status, room expiry, file cleanup
- `src/uploads.rs` — chunked multipart upload helper: streams a field to a temp file, Sha256-hashes as it goes, enforces the size cap (bounded memory, atomic rename), and stops at `MIN_FREE_BYTES` (4 GiB) free on the data volume — checked as the file grows, so concurrent uploads can't fill the disk the database lives on
- `src/routes/` — one file per resource: `rooms`, `rooms_public`, `files`, `admin_files`, `stream_keys`, `webhook`, `branding`, `metrics`, `ome`, `ome_logs` (admin tail of OME's log file), `auth`, `admin_settings`, `rate_limit`, `watch` (Farbplay SRT room-link playback), `pages` (server-rendered link-preview HTML for the landing/viewer pages)
- `src/routes/sql.rs` — shared SQL: `row_to_json`, the room projection (`ROOM_SELECT_BY_ID` / `ROOM_COLS`, plus the `ROOM_LIST_*` variant carrying `waiting_count`) and `FILE_ROOM_SLUGS`. **A new `rooms` column is added here, not in seven handlers** — the projection and its column names are paired positionally, and a unit test asserts they stay the same length. The projections carry `has_password`, never `password_hash` (another test pins that)
- `src/credentials.rs` — single-admin credential helpers: `settings` accessors, DB-or-env password resolver, TOTP, recovery codes, `verify_totp_or_recovery` (the one definition of "valid second factor", shared by login and 2FA teardown), WebAuthn RP builder
- `src/time.rs` — `now_ms()`. **Every timestamp crossing the wire to a browser is epoch milliseconds**; route new ones through here rather than re-deriving, or they land in `new Date(ts)` as 1970
- `tests/common/mod.rs` — shared test fixtures (a fresh temp DB and data dir per test, app setup, seeding, WS clients)

## Frontend structure

TypeScript sources live under `frontend/`, compiled by `tsc` to `www/dist/` (plain ES modules, no bundler). HTML pages under `www/` import the compiled modules via `<script type="module" src="/dist/<page>/main.js">`. Static files are served directly by the Axum backend — only the type-checking step needs Node.

```bash
cd frontend && npm install        # one-time
npm run watch                     # tsc --watch, rebuilds on every save
npm run typecheck                 # CI gate
npm run build                     # production build (CI + prod host)
```

- `frontend/admin/` — admin SPA modules (`main`, `auth`, `rooms`, `stream-keys`, `files`, `branding`, `dashboard`, `settings`, `webauthn`, `types`). **A new `data-action` needs registering in `initDelegatedClicks()` in `main.ts`** — one delegated listener routes clicks to each module through an explicit `switch`, so an unlisted action is silently inert (the button just does nothing; no console error)
- `frontend/viewer/` — viewer SPA modules (`main`, `types`, `state`, `session`, `screens`, `ws`, `player`, `livekit`/`conference`, `devices` (saved camera/mic/speaker, speaker routing, the picker's mic meter), `chat`, `pointer`, `roster`, `layout`, `scopes`/`scope-draw`/`scope-color`, `tour`, plus `globals.d.ts` for the LiveKit/OvenPlayer globals the vendored `<script>` tags define). **A new toolbar button needs registering in `layout.ts`** — `TOOLBAR_EXTRA_IDS` (the mobile ⋯ sheet) and one of `LANDSCAPE_{LEFT,RIGHT}_IDS` (the short-landscape side pills). Miss them and the button simply vanishes on mobile, silently, exactly like an unlisted `data-action` does in admin
- `frontend/landing/` — landing page
- `frontend/privacy/` — the privacy page's branding (its only script; kept out of the HTML so the CSP can forbid inline scripts)
- `frontend/shared/` — `store.ts` (tiny reactive store), `utils.ts` (typed API wrapper, toast, formatters), `branding.ts` (read-only branding loader), `components.ts` (modal helpers), `timezone.ts` (zone labels and datetime-local ⇄ UTC on a named zone's clock, all via `Intl`)
- `www/shared/` — design system CSS (`tokens.css`, `components.css`, `utils.css`); conventions in the [Design system](#design-system) section below
- `www/{admin,viewer,landing}/index.html` — HTML markup, page-specific `<style>`, and the `<script type="module">` tag pointing at the compiled bundle
- `www/dist/` — build output (gitignored; CI / prod host produces it)

The three player libraries — OvenPlayer, hls.js, the LiveKit client — are classic `<script>` tags in the HTML, but **served from this origin**, not a CDN: exact versions are pinned as `devDependencies` in `frontend/package.json` (hashes in the lockfile) and `frontend/vendor.mjs` copies their prebuilt bundles into `www/dist/vendor/` as part of `npm run build` / `npm run watch`. That is what lets the CSP be `script-src 'self'`. They used to come from cdn.jsdelivr.net with no integrity check, two on floating versions, into pages holding the admin token.

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
color outside `tokens.css` — and on a hardcoded radius, z-index or type value in
a `style=""` the TypeScript writes into generated markup (`frontend/**/*.ts`;
colors excepted there, since the scopes draw on canvas in hex), and on any token that is referenced-but-undefined or
defined-but-unused. It allowlists exactly one thing: the `.btn-tab` radius
reset. Both token allowlists (`UNDEFINED_OK`, `UNUSED_OK`) are empty since #248
retired `--focus-aspect` and the `--panel-w-max`/`--strip-w-min`/`--strip-w-max`
widths that `layout.ts` used to read via `getComputedStyle`; put a name back
only for a token genuinely consumed in a way the scan cannot see. The
conventions above were documented long before this gate and drifted anyway —
hence the gate.

**File rows are two different components, named differently on purpose.**
`.file-row` is admin's 8-column grid table row (`frontend/admin/files.ts`);
`.shared-file` (+ `-name/-size/-dl/-show/-del`) is the viewer's flex chip row in
chat and the Files tab (`frontend/viewer/chat.ts`). They both used to be called
`.file-row`, which was harmless only because they never shared a page — and it
meant neither could be promoted to `components.css`. Keep them distinct.

## Key implementation details

**Authentication roles:**
- Admin: `POST /api/auth/login` (password → JWT) — required for all `/api/rooms/*` mutations. 7 days by default, **90 days** when the login form's "trust this browser" box is ticked (`trust: true` in the body; the passkey login takes it too)
- Participant: `POST /api/public/rooms/:slug/join` — returns a participant id + a random 48-hex token (stored on the `participants` row, not a JWT), the pair the WS, file, status and playback paths all take
- Presenter: the admin's own "Enter Room" (`POST /api/rooms/:id/enter`), or the public join with the room's host link (`#role=presenter&pk=<presenter_key>`). The host link is the only public path to presenter, and its sessions are marked `participants.via_host_link = 1`: rotating the link revokes exactly those, never the admin's Enter Room session (it used to revoke every presenter, signing the admin out of the room they were hosting)

**Presenter entry handoff.** Admin clicks "Enter Room" → backend creates `role='presenter', is_admitted=1` → admin JS writes `{participantId, token, deliveryMode, playbackId, role}` to `localStorage['_presession_{slug}']` and opens `/watch/{slug}` in a new tab → viewer reads the presession on load, moves it into `sessionStorage['viewer_session_{slug}']`, and deletes the localStorage entry. The localStorage key exists for milliseconds. (The key really is `_presession_{slug}`, with no `viewer` prefix, unlike every other viewer key — see `PRESESSION_KEY` in `frontend/viewer/session.ts` and the writer in `frontend/admin/rooms.ts`.)

**Session isolation.** `viewer_session_{slug}` is in `sessionStorage` (per-tab, survives refresh, cleared on tab close); so is `viewer_pass_{slug}`, the room password a joiner typed — a refresh, or the automatic rejoin once a kick is lifted, fills it back in, and closing the tab forgets it. It used to sit in `localStorage` indefinitely; `session.ts` moves this room's legacy copy into the tab and drops every other room's on load. `viewer_name_{slug}` stays in `localStorage` (shared across tabs — intentional). A room password in the link is read from `#p=` (never sent to the server) or `?p=` (redacted from the request log), and wiped from the address bar either way. `viewer_kicked_{slug}` is set on `{type:'kicked'}` or WS close 1008 and is checked at page load *before* WS connect, so a kicked viewer sees "Removed" instantly on refresh. If the sessionStorage flag is lost, the WS hub re-detects `is_kicked=1` and re-expels on reconnect.

**Database:** All queries use prepared statements with `?N` placeholders — no string interpolation. Schema in `backend/schema.sql`, bootstrapped on every startup.

**Rate limiting:** `/api/auth/login` → 5 req/min; `/api/public/rooms/:slug/join` → 30 req/min; passkey ceremonies → 30 req/min in a separate bucket so an OS prompt doesn't burn the login budget; and the routes a whole room hits at once — the `/ws/room/:slug` upgrade, the waiting-room event stream, `/:slug/info`, participant uploads — each a bucket of 30, refilling 1/s. Uses `tower_governor` with `SmartIpKeyExtractor`, which takes the **first** `X-Forwarded-For` address — so Caddy sends exactly one, its own `{client_ip}` (see `TRUSTED_PROXIES`), never a list a client could have started. Inside a socket, `ws.rs` caps each connection per message kind (chat 10 then 2/s; pointer moves 60 then 40/s; `pointer:hide` its own bucket, since Farbplay never throttles it; controls 20 then 10/s) and drops what's over unread. **Uploads** are bounded in bytes separately: 2.5 GB a file, 20 GB a room (`ROOM_UPLOAD_QUOTA`, drafts included), and never below 4 GiB free on the data volume; a room that has ended takes none.

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
picker, and it discloses every *category* of browser storage this app writes:
the session token + participant id and the kicked flag (`sessionStorage`), the
per-room device/audio preferences, the tool preferences (scopes window, tour
seen), the saved name and room password, and the admin sign-in token (7 days, or 90 with "trust this browser"). The only
key named literally is `farbstrom_tour` — the tour's cookie, and the **only**
cookie set anywhere; the rest are described in prose rather than by key name
(`viewer_scopes`, `conf_pref_*`, `stream_token` are not spelled out). Anything
new that writes to a cookie, `localStorage` or `sessionStorage` belongs on that
page in the same commit; it shipped claiming "not in cookies" for a whole
release after the tour landed, and claiming the room password was saved behind a
checkbox that has never existed.

**The ? toolbar button** (`frontend/viewer/shortcuts.ts`) opens the shortcuts
sheet, and offers the tour as its second button rather than launching it. The
sheet is generated from `SHORTCUTS` — the same list the key handler reads — so a
new single-key shortcut cannot ship undocumented.

**Public participant status.** `GET /api/public/rooms/:slug/status/:participantId?token=…` returns `{admitted, kicked, room_status: 'scheduled|live|ended', started, playback_id}` — `playback_id` is the room's stream as it is *now*, and null unless admitted and not kicked; a resumed viewer session takes it from here rather than from what it saved, and so does a waiting joiner the moment it is admitted — it holds no socket while it waits, so a key attached or swapped meanwhile never reached it as `stream:assigned`. Companion SSE stream at `/api/public/rooms/:slug/waiting/events/:participantId` emits `admitted`, `kicked`, `room_ended`, `ping` — waiting-room clients drive the full state machine from SSE alone without holding a WS open, and the browser one listens for all three outcomes (it used to hear only `admitted`, so a denied or ended joiner waited forever).

**Moderation audit.** Kick and mute are logged via `tracing::info!` with `room_slug`, `actor_id`, `target_id` for after-the-fact audits. If LiveKit `remove_participant` fails the backend retries once after 250 ms and `error!`s on the second failure — the DB `is_kicked=1` flag and WS force-close happen first, so UI state is correct even when LiveKit is momentarily unreachable.

**Farbplay room-link SRT playback** (`src/routes/watch.rs`, GitHub #165). `GET /api/watch/:slug?participantId=&token=` lets the native SRT viewer (Farbplay) connect from a room link instead of a raw `srt://` URL. The flow mirrors the browser viewer: Farbplay first `POST /api/public/rooms/:slug/join`s to become a `participants` row (password is checked there, not here), waits on the existing admission SSE (`…/waiting/events/:pid`) if the room has a waiting room, then calls this **admission-gated** endpoint. It returns `{srt: {host, port, streamid, latency}, ttlSeconds, title}` where `streamid` is `default/live/<playback_id>?policy=<b64url>&signature=<b64url-hmac-sha1>`, signed with `OME_SIGNED_POLICY_SECRET` and expiring after ~30 s (`url_expire`). **OME signs the `srt://`-prefixed URL** (`srt://default/live/<playback_id>?policy=…`, scheme + vhost as host), so the backend must HMAC that form even though the client sends only the path. OME validates it via the `<SignedPolicy>` block (scoped to the SRT publisher only). The signed streamid is minted **only for an admitted, non-kicked participant**: missing `participantId`/`token` or kicked/not-yet-admitted → **403**; unknown participant / wrong token / wrong slug / ended / expired / no stream key → **404**. A kicked viewer therefore can't reconnect (the backstop behind the SSE self-disconnect; no server-side SRT sever today — contract O1/O2). The stream name in the streamid is the key's playback id, which is no secret — see "Playback ids" below.

**Playback ids: what viewers get is never the ingest key** (`src/playback.rs`, `src/routes/webhook.rs`). Each stream key has a random `playback_id`, and OME publishes every stream under it: the admission webhook answers an allowed ingest with `new_url`, renaming the stream (measured on OME v0.21.0 for RTMP, SRT and WHIP). Join, the admin's Enter Room, `stream:assigned`, `/api/watch` and the status endpoint all hand out the playback id; the `key_token` stays in the admin's Stream Keys tab, for encoders. It used to be handed to every joiner — including ones held in the waiting room — which made every viewer able to push video into the room and to watch it without being admitted. Browser playback is admission-gated as well: `<Publishers>webrtc,llhls</Publishers>` makes OME ask the webhook before each WebRTC / LL-HLS session, and the backend allows an admitted, non-kicked participant of a running room on that stream (`participantId` + `token` in the playback URL's query, the same pair the file endpoints take) or an admin preview token (`apt`, 5 min, `GET /api/ome/playback-token`). OME asks once per session — LL-HLS chunklists ride on its own `session` parameter — so a kick refuses *new* sessions and one already playing runs on until the viewer reconnects. The OME stream list, the poller, the dashboard and the stream kick all speak playback ids; the webhook's ingest lookups still go by `key_token`, because OME's callbacks carry the URL the encoder used (`url`) beside the renamed one (`new_url`).

**Every SRT playback target is signed, by one helper** (`src/signed_policy.rs`, GitHub #226). `<SignedPolicy>` is enabled for the SRT publisher, and OME rejects an **unsigned** streamid exactly as it rejects an invalid one — so there is no such thing as a static, copy-pasteable SRT playback URL. Both callers mint through `signed_policy::sign_streamid(secret, stream_name, ttl)`: `/api/watch/:slug` (Farbplay, 30 s) and `GET /api/stream-keys/:id/srt-playback` (admin JWT, 300 s), which backs the **Generate** button on the Stream Keys tab's playback SRT row (its WebRTC and LL-HLS rows have one too, minting a preview token — see "Playback ids"). The frontend cannot build this URL — the secret is backend-only — and must **percent-encode the streamid** inside the outer `srt://…?streamid=` URL, or a player reads the streamid's own `?policy=…&signature=…` as sibling SRT socket options. `url_expire` is checked at connect time only, so a session established inside the TTL keeps running.

**Farbplay is marked on the WS roster** (`client: "farbplay"`, GitHub #227). Farbplay used to open no WebSocket at all and was identified purely by *SSE presence without WS presence*; pointer collaboration changed that, and the viewer's Farbplay roster section silently emptied. Its auth frame now carries `"client": "farbplay"` (absent ⇒ browser), which `ws.rs` whitelists via `normalize_client`, stores on `WsParticipant` — **on the reconnect branch too**, or a dropped socket demotes the viewer to the browser list — and republishes in `participants:update`. `roster.ts` renders marked entries in the Farbplay section, unioned with the SSE-only list that older Farbplay builds still produce. Farbplay entries stay in `viewerStore.roster`, so pointer cursors and conference tiles treat them like any other participant.

## CI

GitHub Actions runs on push/PR (`.github/workflows/ci.yml`):
- **build** — `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, `cargo test`.
- **audit** — `cargo audit` (advisory DB check). Nothing is ignored: there is no `.cargo/audit.toml`, since replacing `jsonwebtoken` took the last flagged crates (`rsa`, `crypto-bigint`) out of the tree. An advisory with no fix should be ignored there, with its reason, rather than left red.
- **frontend** — Node 26 (the active LTS, as in the Dockerfile): `npm run typecheck`, `npm run build`, then `./scripts/design-lint.sh` (see below).

Every workflow runs with a read-only `GITHUB_TOKEN` unless a job asks for more
(the notices job, auto-merge), and every action is pinned to a commit SHA with
its tag in a trailing comment — which Dependabot's `github-actions` updates keep
current. `docker-single.yml` refuses to publish a release tag that is not on
`main`: the notices job commits to `main` on the same assumption, and `v1.1.5`
was tagged on `dev` and shipped code `main` did not have.

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

Dependabot watches `cargo` (majors included — for 0.x crates every 0.y bump is a
major, and ignoring them is how rusqlite, reqwest and the RustCrypto set fell years
behind), `npm` in `frontend/`, `github-actions`, and the literal Docker `FROM` bases
(`rust`, `ubuntu`, `node`). Auto-merge (into `dev`) takes patch and minor only, and
**never the three player libraries** — their PRs wait for a by-hand playback check.
**Dependabot cannot see the four component pins** — they are `ARG`s interpolated into
`FROM` (`FROM caddy:${CADDY_VERSION}`), which it does not resolve. That list is
**manual**; check it when you touch the stack.

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

**Player libraries** — exact versions in [`frontend/package.json`](frontend/package.json)
`devDependencies` (`ovenplayer`, `hls.js`, `livekit-client`), copied into `www/dist/vendor/`
by [`frontend/vendor.mjs`](frontend/vendor.mjs); bump with `npm install --save-exact`.
Both [`www/viewer/index.html`](www/viewer/index.html) and
[`www/admin/index.html`](www/admin/index.html) load them (livekit-client is viewer-only).
Take extra care with OvenPlayer: its DOM class names are load-bearing for the
chrome-hiding CSS in each page's `<style>` block and have drifted before. Diff the
`op-*` classes between the two tarballs (`npm pack ovenplayer@<old>` / `@<new>`) and
check each one the CSS hides still exists — 0.10.53 → 0.10.54 kept them all, and
`viewTemplate.js` differed only in line endings.

## Useful reference docs

- `README.md` — what Farbstrom is and what it does
- `docs/DEPLOYMENT.md` — architecture diagram, tech stack, ingest protocols, deployment/ops

## Tests

**Two fixtures, and the difference matters.** `common::test_app` builds only
`routes::build_router` (the `/api/*` half) over a mock transport — fine for HTTP
handlers, and left alone because most of the suite runs through it.
`common::test_app_http` builds the whole app via `app::build_app` over a real
port, which is the only way to reach the WS hub, the static/SPA routes, the
response layers, or `ConnectInfo`. A test written against the wrong one can pass
while exercising nothing: an early SPA-fallback test did exactly that, because
everything unmatched 404s in the API-only router anyway.

Existing suites, worth knowing before writing overlapping ones:
`db_integrity_test` (pool pragmas, cascades, `created_at` → epoch ms, the
display-name cleanup),
`file_visibility_test` (draft visibility, uploads — always drafts, the room
quota, closed rooms — shared-row blast radius, delete accounting, admin file
links), `room_state_test` (ended/expired gates, kicked presenters, slugs,
stream-key deletion), `robustness_test` (colour validation, OME name allowlist),
`ws_test` (the WS protocol surface — auth gate, kick, chat + history, roster
markers, presenter gating, moderation filtering, per-connection limits),
`app_routing_test` (cache headers, SPA fallback, JSON 404 for unknown `/api/*`,
Open Graph injection), `rate_limit_test` (every bucket, in one test function —
see the gotcha below).

## Gotchas

Non-obvious facts that aren't derivable from reading the code.

**WebSocket hub**
- **A rejected socket must have its close frame *flushed*, not just queued.**
  `handle_socket` queues the explanatory frame plus a 1008 close on the outbound
  channel, and a forwarding task moves them to the wire. Calling
  `send_task.abort()` straight afterwards — which it used to — usually drops both
  unsent, because the task has not been polled yet. The client then sees a
  transport reset (1006), and `frontend/viewer/ws.ts` branches specifically on
  `e.code === 1008` to decide "kicked or stale auth" — so a kicked viewer got no
  `kicked` frame and sat in the reconnect loop instead of the Removed screen.
  `reject_socket` drops the last sender and awaits the task instead. It is only
  safe *before* registration in `WS_ROOMS`, which holds a clone of the sender.
- **The hub's room maps are process-global statics** (`WS_ROOMS`,
  `WS_ROOM_FOCUS`, `WS_ROOM_DISPLAY`, and `presence::SSE_PRESENCE`). Cargo runs a
  test binary's cases on parallel threads, so every WS test must use
  `common::unique_slug`; two tests sharing a room slug will see each other's
  sockets.
- **The reconnect branch reuses the `WsParticipant`**, so a farbplay→farbplay
  reconnect cannot detect a marker it fails to re-read — the value is already
  right. `reconnect_updates_the_client_marker` reconnects with a *different*
  marker, which is what actually pins that line (gh #227).
- **The server ends a connection; it never asks.** A kick, a host revocation or
  the room ending queue the explanatory frame and a close, then cancel the
  connection's `closer` (`CancellationToken` on `WsParticipant`), which stops its
  read loop. Queueing the close and waiting for the client to answer let a
  modified client that never answered go on chatting — and a revoked presenter go
  on steering everyone's view. `a_kicked_client_that_ignores_the_close_is_cut_off`
  plays that client with `common::RawWs`, because axum-test's client answers a
  close on its own.
- **One live socket per participant.** A new authenticated socket supersedes the
  old one (close 1008 "Superseded", then its `closer`), and the disconnect timer is
  keyed to `conn_id`. Nothing pings from the server side, so after a dropped link
  the client reconnects long before the old TCP connection dies — and when it
  finally did, its timer removed the participant the new socket had taken over:
  connected, deaf to every broadcast, gone from the roster.
- **Event listeners must survive `RecvError::Lagged`.** The channels hold 64; a
  receiver that falls further behind gets `Lagged` once and then carries on. Every
  listener used to be `while let Ok(..) = rx.recv()`, which ended it for good — one
  waiting joiner opening 150 admission streams stopped every presenter seeing new
  arrivals until a restart. New listeners go through `spawn_listener`.
- **A new client message type needs a bucket in `Limits::allow`.** Unknown types
  pass free (the handler ignores them anyway), so a new one that broadcasts or
  writes would be unthrottled until it is listed. Size buckets against what the
  clients actually send — both throttle pointer moves to 33 ms — and give anything
  a client must never lose (like `pointer:hide`) a bucket no flood of something
  else can drain. Over-limit frames are dropped, not answered: a socket is never
  closed for rate, because the viewer reads a 1008 as kicked or stale auth.
- **Frames over 16 KiB end the connection** (`WS_MAX_MESSAGE`). The largest
  legitimate one is a 500-character chat message; raise it before adding a message
  that can be larger.

**OME admission webhook**
- **OME sends `closing` for every session that ends, including ones it never let
  publish** — a second encoder refused as a duplicate of the live one, and a
  session the webhook itself denied (both measured on v0.21.0). Demoting the room
  on that notification let anyone with the key blank the stream for everyone. A
  close is re-checked against OME's stream list 2 s later instead.
- **Never call OME's REST API from inside the webhook handler.** OME is blocked
  waiting for the webhook's reply, and the two stall each other until OME's 5 s
  webhook timeout (measured). Answer first; ask afterwards from a spawned task.

**Rate limiting**
- **The buckets are process-global.** `tower_governor` keeps its limiter inside
  `GovernorConfig` (built once in `finish()`), and `routes::rate_limit` caches
  each config in a `static OnceLock` — so every router in a process shares one
  bucket per layer, keyed by client IP. In production that is exactly right; in
  tests it means rate-limit cases cannot run in parallel (they all come from
  127.0.0.1 and drain each other), which is why `rate_limit_test.rs` is one test
  function in its own binary. It also cannot share a binary with anything calling
  `test_app`, which sets `STREAM_DISABLE_RATE_LIMIT` process-wide.
- **A limiter that cannot tell clients apart is worse than none.** Behind an
  external proxy every request reaches Caddy from the same Docker bridge address;
  until `TRUSTED_PROXIES` was set there, Caddy discarded the proxy's
  `X-Forwarded-For` (correctly — it did not trust the peer) and every client fell
  into one bucket, so five wrong passwords from anyone locked everyone out of
  sign-in for a minute. The `{client_ip}` the backend is handed is only as good as
  that setting.

**Database / pool**
- **`foreign_keys` is ON here by accident of the build, not by the pragma in
  `db.rs`.** libsqlite3-sys's bundled SQLite is compiled with
  `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`, and rusqlite calls
  `sqlite3_busy_timeout(db, 5000)` on open — measured, on every pooled
  connection and on a raw `Connection::open`. Dropping the `bundled` feature for
  a system SQLite would silently turn every `ON DELETE CASCADE` in `schema.sql`
  into a no-op, and four call sites depend on those cascades
  (`rooms::delete_room`, `stream_keys::delete_key`, `files::delete_room_file`,
  `admin_files::delete_files_inner`). `db.rs` restates both pragmas in
  `with_init` so the guarantee is ours; `tests/db_integrity_test.rs` pins it.
- **Pragmas set on a connection pulled from the pool configure only that
  connection.** r2d2's `min_idle` defaults to `max_size`, so all 8 exist by the
  time `build()` returns. This is not theoretical — `synchronous = NORMAL`
  reached exactly one of them for a long time while the other seven ran `FULL`.

**Wire timestamps**
- **Everything sent to a browser is epoch milliseconds** (`crate::time::now_ms`).
  `file:shared` used to send seconds from four call sites while `chat:message`
  sent milliseconds, and the viewer renders both through one `new Date(ts)` —
  so shared files showed a January 1970 clock time. Chat history converts
  `created_at` with `strftime('%s', ...) * 1000`, which reads the stored value as
  UTC; `new Date("YYYY-MM-DD HH:MM:SS")` parses as *local* time and shifted the
  whole replay by the viewer's offset.

**Admin sessions**
- **Admin JWTs carry a generation (`ver`) and are revocable.** They used to be
  pure `{admin, exp}` — nothing tied them to the password, so changing it
  revoked nothing and a stolen token stayed valid for its full expiry. A
  password change, or Settings → "Sign out other devices", bumps
  `settings.admin_token_version`; `AdminAuth` refuses any token stamped with an
  older one. That is what makes the 90-day "trust this browser" session
  defensible — without revocation it would be a 90-day liability in
  localStorage.
- The DB row is authoritative; `AppState.admin_token_version` is a cache so the
  per-request check does no I/O, reloaded at startup so revocation survives a
  restart. One process owns it, so the two cannot diverge.
- `ver` is `#[serde(default)]`, so a token minted before the claim existed reads
  as generation 0 — the same value a fresh install starts at. Upgrading does not
  sign the operator out.
- **The session token never goes in a URL.** `AdminAuth` reads the
  `Authorization` header only. The library's `<img>` thumbnails, `<video>`
  previews and downloads, which cannot send a header, carry `?ft=` instead — a
  token from `GET /api/admin/files/link-token` that opens library files and
  nothing else, lasts ten minutes, and is HMAC'd with the session generation, so
  a revocation ends it too. Those URLs used to carry the 7- or 90-day session
  JWT itself, into browser history. Downloads are an `<a download>` click, not
  `window.open`: the link is built after an await, and a popup that late is fair
  game for a blocker.
- **Both revoking endpoints return a replacement token**, and the SPA adopts it
  (`adoptReissuedToken`), so the tab that performed the revocation is not logged
  out by its own action. "Sign out other devices" is password-gated on top of
  `AdminAuth`: otherwise a stolen token could lock the real operator out while
  minting itself a fresh one.

**401 vs 403 on admin endpoints**
- **401 from an admin endpoint means exactly one thing: this session is no
  longer valid** (expired, or revoked). `apiFetch` signs the operator out on any
  401, so a handler behind `AdminAuth` that returns 401 for a *wrong submitted
  credential* logs you out on a typo — which is what "current password is wrong"
  in the change-password form used to do. Those are `AppError::Forbidden` now.
  Anything new that re-checks a password mid-session must be 403, not 401.

**Admin 2FA**
- **Teardown needs the second factor, not just the password.** The admin JWT is
  minted from the password alone, so gating `totp/disable` on a password
  re-check would gate 2FA removal on exactly the factor 2FA backstops. Recovery
  codes are accepted there too — losing the authenticator is when you need it.
- **`totp/setup` is refused while TOTP is enabled.** It rotates the secret and
  sets `totp_enabled = 0`, so re-opening the setup panel used to switch 2FA off
  silently and invalidate the authenticator.

**Files**
- **A participant upload is always a draft, and never de-duplicated.** There used
  to be an immediate-share mode that de-duplicated by content hash, handing a
  second uploader of identical bytes the *existing* row — so a guest in room B who
  uploaded room A's file got room A's filename and uploader back, and the file
  joined room B's list. No client used it (the viewer has always uploaded with
  `defer=true`, now ignored); it was removed rather than made safe. Content-hash
  dedup survives in the **admin library** upload only, where the admin sees every
  file anyway.
- **One `session_files` row can still serve several rooms** — the library assigns
  any shared file, a room's own uploads included, to other rooms through
  `room_files`. So "this file originated in my room" does not imply "only my room
  has it": the host delete path checks `room_files` for other rooms before
  hard-deleting, and otherwise only detaches. Blob unlinking recounts
  `stored_path` references first for the same reason.
- **A refused upload must be read before it is refused, or the refusal is lost.**
  When a handler answers without reading the request body, hyper closes the
  connection, and Caddy — still writing the upload upstream — fails with a broken
  pipe and answers **502 with an empty body** instead (measured: one in five 1 MB
  uploads to an ended room; straight to the backend the refusal arrived every
  time). So an authenticated refusal reads and discards the rest first
  (`uploads::drain`, `discard_rest`), up to 64 MiB: afterwards all 66 refused
  uploads measured (64 KB to 30 MB) and 6 of 6 oversized branding images got their
  real answer through Caddy. Larger bodies still race, which is why the viewer and
  the branding form check sizes before sending. Two rules: never drain for an
  unauthenticated request (it would make the server ingest data for anyone), and
  never call `next_field()` on a refused field you have not finished reading —
  multer skims the rest of it, uncounted. The rate limiter's 429 answers before
  any of this and can race the same way; for a flood that is acceptable.
- **Drafts (`is_shared = 0`) are hidden from every read path**, including
  `download_file` — which authorises on room membership, so without the filter
  any room member who knew a file id could pull someone else's unsent upload.
  The uploader still reaches their own.

**LiveKit**
- `entrypoint.sh` generates `/livekit.yaml` with a `keys:` map (`KEY: SECRET`) — the **space after the colon** is required (YAML), else LiveKit boots with no auth and only logs "Could not parse keys". The backend must mint tokens with the same `LIVEKIT_API_KEY`/`LIVEKIT_API_SECRET`. Keys are inlined into the YAML so the LiveKit process needs no key secrets in its env (`supervisord.conf` strips them with `env -u`).
- No upstream Rust SDK — `src/livekit.rs` is hand-rolled (AccessToken JWT + RoomService over `reqwest`) against `LIVEKIT_INTERNAL_URL` (HTTP, not WSS).
- **`DeleteRoom` checks `roomCreate`, not `roomAdmin`.** With only `roomAdmin` LiveKit answers `401 permissions denied`; every caller ignored the error, so ending, deleting or expiring a room never tore down its call (verified against 1.13.5). `RemoveParticipant` and `MutePublishedTrack` do take `roomAdmin`.
- Caddy `/livekit/*` block needs `header_up Host {upstream_hostport}` for WebSocket signaling to work through the proxy ([`caddy/Caddyfile`](caddy/Caddyfile)).
- **Only `/livekit/rtc*` is proxied; the rest of `/livekit/*` is a 404.** The browser client opens its socket on `/rtc/v1` (`/rtc` on older servers) and probes `…/validate` on failure (`createRtcUrl` / `createValidateUrl` in `livekit-client/src/api/utils.ts`); region lookups are LiveKit Cloud only. LiveKit's Twirp RoomService API — what the backend drives — was reachable from the internet both through this block and on a published `7880`; port 7880 is no longer published either. A new client version that needs another path has to be added here, or the call fails at connect.
- **`entrypoint.sh` refuses to start without `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET`.** They used to default to LiveKit's well-known dev pair (`devkey` / `secret`), so a deployment missing them ran a LiveKit anyone could mint tokens for, beside a backend crash-looping on the same missing variables.
- **LiveKit refuses speaker selection on every Safari-based browser** — `supportsSetSinkId()` returns `false` for Safari without checking, so `switchActiveDevice('audiooutput')` throws there, although Safari has shipped `HTMLMediaElement.setSinkId` since 18.4. `devices.ts` routes the remote `<audio>` elements LiveKit's `attach()` returns (and the broadcast's `<video>`) itself whenever `supportsAudioOutputSelection()` is false. WebKit honours `setSinkId` only inside a user gesture, so the picker starts every element's call in the same tick; an element that appears later (new participant, player remount) is best effort and logs to the diagnostics panel if refused. Whether a given Safari release actually moves WebRTC audio is unmeasured — STP 251 fixed `setSinkId()` "reporting success without switching audio output for a single-track MediaStream". The picker's **Test** button plays a tone through a MediaStream-backed element, the same path a participant's voice takes, which is how to check.
- **Changing capture constraints on a published mic means `restartTrack()`, not mute/unmute.** `setMicrophoneEnabled(true, opts)` on an existing track only calls `unmute()` and ignores `opts` (mute doesn't stop the track, so unmute never re-captures) — the noise/echo toggles silently needed a page refresh for exactly that reason. `restartTrack(opts)` falls back to `deviceId: {ideal: 'default'}` when `opts` carries none, so pass the current device or it jumps to the system default. A restart preserves the muted state.
- **Never call `getUserMedia` for a kind that is already live.** Safari mutes the earlier track when the page captures the same kind again (WebKit bug 179363, "expected behaviour"), and LiveKit pauses a muted track's upstream 5 s later — the participant goes silent with nothing on screen. The device picker used to do this on every open; `listDevices()` now only asks for unnamed kinds that aren't live.
- **A failed input switch leaves nothing capturing.** LiveKit's `restart()` stops the old track *before* `getUserMedia` for the new one (Safari requires that order), so if the new device fails the mic is dead. `switchInput()` switches back to the previous device on failure. `switchActiveDevice` also resolves `false` (no throw) when the browser captured a different device than asked — check the boolean.
- **Saved devices are requested as a bare (ideal) `deviceId`**, so an unplugged one falls back to the default rather than failing the capture. Safari's device ids are salted per origin and the salt is lost with website data, so the label is saved too and `restoreSavedInput()` (on `LocalTrackPublished`) re-finds a renamed device by name once a capture has made labels readable.
- **Remote audio can be blocked by autoplay policy.** A saved cam/mic pref connects the call on page load without a click; if the browser then refuses to play remote audio LiveKit emits `AudioPlaybackStatusChanged` and does nothing else. `room.startAudio()` must run inside a gesture, so the viewer arms a one-shot capture listener on the next click/key.

**OvenPlayer**
- `ovenplayer.js` does NOT bundle `hls.js` — load it separately or LLHLS fails silently.
- `controls: false` is a silent no-op. Hide the UI via CSS `.op-ui-container { display: none !important }`.
- Error/notification overlay sits OUTSIDE `.op-ui-container` — also hide `.op-message-container, .op-notification-container`.
- **Browser support, measured on OME v0.21.0** (WebRTC delivery, real 1080p ingests): H.264 plays in Chrome, Safari **and** Firefox; H.265 plays in Chrome and Safari, not Firefox. The H.265 tested was `hvc1.4.10.L120.bd.8` — profile 4 (Range Extensions, 4:2:2 10-bit), decoded via Apple Silicon hardware. So HEVC over WebRTC is **not** Farbplay-only, and `deliveryMode` is a genuine per-room choice rather than a codec workaround.
- LLHLS + Safari + H.265 historically failed because Safari MSE rejects the `hev1` sample entry. **v0.21.0 changed the packaging** (upstream #2258): verified that an H.265 ingest now yields `CODECS="hvc1…"` in the LL-HLS master playlist and an `hvc1`/`hvcC` sample entry in the fMP4 init segment — the form Safari requires. **Whether Safari decodes H.265 over LL-HLS is still untested** (the tests above all ran through WebRTC). Test it before relying on LLHLS for an H.265 room, and record the result here.

**WebRTC transport (OME)**
- **The two `<IceCandidates>` blocks in [`ome/origin_conf/Server.xml`](ome/origin_conf/Server.xml) are deliberately opposite, and the difference is load-bearing.** Playback (`<Publishers><WebRTC>`) **forces the built-in TURN relay** (`<TcpRelayForce>true</TcpRelayForce>` — v0.21.0's rename of the deprecated `<TcpForce>`). This looks wasteful and it is tempting to "fix"; **don't, without testing Firefox.** Direct ICE was tried on v0.21.0 (`${PublicIP}` candidates + RFC 6544 TCP ICE on `10000/tcp`, `TcpRelayForce=false`) and Chrome and Safari worked while **Firefox never nominated a candidate**, looping `Added session`/`Removed session` at OvenPlayer's 8 s `connectionTimeout` forever. Offering the relay *alongside* direct candidates (`<DefaultTransport>all</DefaultTransport>`) does not rescue it either: behind Docker NAT, OME can only advertise the relay at `${PublicIP}` and `172.x`, and neither is reachable as `localhost`. Relay-forced is the only configuration verified to work in all three browsers.
- **Ingest (`<Providers><WebRTC>`) must NOT force the relay — that breaks WHIP outright.** With it forced, OME withholds its real candidates: the SDP answer carries a single placeholder (`100.127.255.254` on a port nothing listens on) and the working path is offered *only* as `Link: <turn:…>; rel="ice-server"` response headers. OBS parses those headers and attempts direct ICE anyway ([obsproject/obs-studio#12790](https://github.com/obsproject/obs-studio/issues/12790), open), so every push died identically: HTTP `201` with a resource URL, then `PeerConnection state is now: Connecting` → `Failed` ~40 s later, no media, and a `DELETE … 404` on the way out. The handshake succeeding is what makes this read as a codec or firewall problem; it is neither. The Firefox finding above is about *playback* and does not transfer to ingest — reading it as blanket is what kept WHIP broken for a release.
- **A WHIP encoder can only use candidates it can route to, and `*` does not include loopback.** `OME_ICE_ADDRESS` (derived in [`entrypoint.sh`](entrypoint.sh) from `PUBLIC_HOST`) feeds the ingest `<IceCandidate>`: `*` on a real host, `127.0.0.1` on a localhost stack. On Docker Desktop `*` enumerates `172.x` (unroutable from macOS) and the STUN-discovered public IP (needs router hairpin *and* a port-forward back), so OBS on the same Mac has nothing to connect to.
- **A local WHIP push needs Caddy's internal CA trusted, or it dies before ICE.** `https://localhost` is served with Caddy's own CA; OBS is libcurl-based and rejects it — `Connect failed: SSL certificate problem: unable to get local issuer certificate` in the OBS log, and `curl https://localhost/` fails identically. Trust it once (`sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain data/caddy-local-root.crt` on macOS) and the URL the admin panel shows works as-is. `docker-compose.dev.yml` also publishes OME's `3333` as a TLS-free fallback (`http://localhost:3333/live/<key>?direction=whip`) — that one needs `make dev`, since a bare `docker compose up -d` uses the base file only and publishes no `3333`.
- **Adding a new ICE port means three edits**: `Server.xml`, the `EXPOSE`/`ports:` entries, and `FW_TCP`/`FW_UDP` in [`deploy.sh`](deploy.sh).
- Debugging ICE: OME logs `nominated candidate` + `DTLS peer certificate verified` on success. A repeating `Added session`/`Removed session` pair with neither line in between is ICE connectivity failure, not codec or signalling — signalling clearly succeeded if a session was created at all.
- **ffmpeg's `-f whip` muxer cannot test this server** — it offers `a=setup:passive`, and OME rejects that outright (`Offer SDP has invalid setup type - a=setup:passive`, HTTP 500) before ICE is ever reached. Not an OME bug and not the OBS failure mode; it just makes ffmpeg useless as a probe. Test with OBS, or POST a hand-written `a=setup:actpass` offer with curl and read the candidates out of the answer — that reproduces the relay-force bug in one command without an encoder.
- `OME_HOST_IP` (feeding `<Distribution>`) is derived in [`entrypoint.sh`](entrypoint.sh) from `PUBLIC_HOST`, so its export must stay *below* where `PUBLIC_HOST` is resolved. It previously read a `DOMAIN` var nothing sets and silently resolved to `localhost`.

**Codecs**
- Video is `<Bypass>true</Bypass>` — never transcoded. The ingest codec is exactly what every viewer's browser must decode, so codec choice is a product decision, not a server one. Audio *is* transcoded (AAC for LLHLS/SRT, Opus for WebRTC) with `BypassIfMatch` short-circuits.
- AV1 (OME v0.21.0) is **WHIP / enhanced-RTMP only — SRT cannot carry it**, because OME's SRT ingest is MPEG-TS and its demuxer has no AV1 support. Transcoding *to* AV1 isn't viable either: the only encoder is libaom, and upstream disabled its realtime/RC/tiling options (`#if 0`) over a crash. Two guards exist because an undecodable stream otherwise looks like a black tile: `findUnplayableCodec()` in [`frontend/viewer/player.ts`](frontend/viewer/player.ts) reads `CODECS` from the LL-HLS playlist and `MediaSource.isTypeSupported()`s each one (LL-HLS only — WebRTC negotiates codecs in SDP, so there's no equivalent pre-flight), and `browserCodecWarning()` in [`frontend/admin/dashboard.ts`](frontend/admin/dashboard.ts) flags AV1/H.265 ingests on the dashboard.

**Player sizing**
- CSS `aspect-ratio` is unreliable in flex containers — the viewer uses JS `sizeStage()` for exact 16:9 pixel dimensions. Grid view only: focus view needs no measuring since #248.
- iOS orientation change: call `sizeStage()` at 0/50/150/300/500 ms because iOS animates rotation over ~300 ms and dimensions are stale mid-transition.

**Viewer room layout — the stage is the background layer** (gh #248)
- **Nothing but `#stage` takes flow space any more.** `#top-bar` (now a transparent row of glass chips, not a bar), `#bottom-toolbar`, the chat panel and the focus-mode call panel are all absolutely positioned over it. In focus view the pinned tile fills `#stage` edge to edge with the video `object-fit: contain` inside it — so the letterbox is `--media-bg`, never a crop. A colour-review room must not crop the picture to fill the frame.
- **`--chrome-top` / `--chrome-bottom` are the safe band**, and everything that must not hide under the floating chrome insets by them: the grid's `#stage` padding, the chat panel, the call panel, and the pinned tile's own name label and pin button. Do not hardcode a clearance next to them.
- **An abspos box's containing block is its ancestor's *padding box*, so padding on the ancestor does not inset it.** The mobile-landscape side-pill clearance used to be `padding` on `#main-row` and silently stopped working the moment `#stage` became absolute — the call panel landed underneath the left pill. Inset each floating box directly instead.
- **A percentage `max-height` on an abspos box resolves against the whole containing block, not the `top`/`bottom` band it sits in.** The call panel's eight-tile cap has to spell the band out as `calc(100% - var(--chrome-top) - var(--chrome-bottom))`; with plain `100%` a long roster overflowed past the window edge instead of scrolling. Its height is `max-content` (exactly as tall as the participant count), it hangs from the top-left of the band, and it scrolls with `scrollbar-width: none`.
- **`#stage` animates its padding, and `sizeStage()` measures that padding — so every layout switch must reflow across the whole transition, never on one frame.** Use `reflowStage()`, not `requestAnimationFrame(sizeStage)`. Leaving focus view with a single frame picked the column count while the padding was still 0, i.e. for a box 118px taller than the grid actually got: tiles came out stacked and overlapping and stayed that way until the next `resize` — which is why toggling fullscreen appeared to "fix" it.
- **A tile's edge is a `box-shadow`, never a `border`, and its width is a whole multiple of 16px.** Both are what keep a 1px black line off the edge of the picture. `aspect-ratio` applies to the *border* box while `.tile-inner` insets to the *padding* box, so any border makes the video's box 2px short on each axis — a 1234px tile is 694.125 tall and hands the video 1232×692.125, i.e. 1.780:1 — and `object-fit: contain` pillarboxes a 16:9 source by ~0.8px a side. That sliver is the tile's own `--media-bg` showing through. `1fr` columns made it worse by resolving to fractional widths. A box-shadow draws the same hairline without consuming layout (and unlike `outline` it follows `border-radius` everywhere), and the multiple-of-16 width makes the height a whole number, so the video's box lands on an exact 16:9 integer. Verify by measuring the pillarbox, not by eye: it must be 0.
- **Pinned ↔ grid is one FLIP, not three separate snaps** (`morphStage()` in `layout.ts`). It measures every tile, applies the DOM mutation, measures again, inverts the difference as a transform and releases it. For the duration `body.stage-morphing` makes `#stage`'s padding change instant, so the grid does not resize *under* the travelling tiles. Reduced-motion gets the instant swap.
- **`tile-in` is a once-per-tile entrance (`.has-entered`), never something muted and restored.** Flipping `animation-name` from `none` back to `tile-in` **starts a new animation** — so suppressing it for the length of the morph and unsuppressing at the end made every tile scale and fade the instant the morph landed, and the pinned tile's own `animation: none` did the same thing on unpin. Tiles are marked on `animationend`/`animationcancel` *and* by `morphStage` itself: a tile it can measure is on screen, so it has entered, and `#tile-stream` in particular can have its entrance cancelled in the same tick it starts (shown, then pinned), which fires neither event.
- **The grid's row height is set explicitly (`grid-auto-rows`), not left `auto`.** An auto row sized against a stretched item whose height comes from `aspect-ratio` is circular: the row resolves from the item's min-content height, comes out shorter than the tile, and the tiles spill over each other. `sizeStage()` already knows the exact height, so it makes the track definite.
- **Both floating panels step aside for fullscreen and come back on exit.** Each has its own `…HiddenByFullscreen` flag that its `toggleChat()` / `toggleConf()` clears, so only a panel *we* closed is reopened: a manual close survives a fullscreen round-trip, and reopening one while in fullscreen isn't clobbered on the way out. The two are independent — closing chat by hand doesn't stop the call panel from being restored.
- **The mobile-landscape side pills split by meaning, and that is what keeps them even.** Left is what you put *into* the room (cam, mic, screen, device) plus who you are in it with (chat, participants); right is the picture (focus, pointer, scopes, play, mute, resync, fullscreen). Balance falls out of it because the buttons that come and go are one per side: the focus-only pair is participants/left and pointer/right, and the two conditional ones — screen share (hidden on iOS) and scopes (hidden with no source) — are also opposite. The pills stay within one button of each other in all eight room shapes; the old split had five of six always-present controls on the right and both focus-only ones on the left, so grid view on an iPhone showed three buttons facing eight.
- **Side-pill buttons shrink in landscape** — 38px, and 36px with tighter spacing below 360px tall — so seven still fit without scrolling on a 320px-tall iPhone SE. The pills also hide their scrollbars (`scrollbar-width: none`): a bar inside a floating glass pill reads as a rendering fault, not an affordance.
- **The call panel is the same floating top-left card at every size** — mobile portrait used to make it a full-width horizontal carousel, the last piece of the old layout still stretched edge to edge. Only `--strip-w` changes (108px on mobile, 116px in landscape); `--call-tile-h` is derived from it, so the eight-tile cap follows on its own. An empty panel is hidden outright (`:not(:has(.tile))`) — a solo participant was otherwise given a small grey stub floating on the picture.
- **A small phone in landscape matches the mobile breakpoint AND the landscape one** (an SE is 568×320, so under 700px wide and under 440px tall). The two blocks compose, and the later landscape rules only override what they name — which is why the call panel needs `right` set there as well as `left`: it inherits the mobile carousel's `width: auto`, so a left-only inset ran it under the right-hand pill.
- **The pinned view is the colour-accurate one; the grid is thumbnails.** The pinned tile is deliberately stripped of everything that could touch the image: `border: none`, `border-radius: 0` (a rounded clip is a mask, and a mask can push the video off its overlay plane the same way a backdrop-filter does), and `animation: none`. Grid tiles keep the 12px radius and the `tile-in` fade, and are downscaled — resampling changes pixel values by definition, so no grid thumbnail is ever colour-accurate regardless of CSS. Judge colour pinned.
- **What matters is contrast and luminance across the whole picture, not stray pixels.** `backdrop-filter` was a real defect because it shifted the transfer function of the entire image. A rounded corner clipping a few pixels, or a 2px ring on a tile edge, is chrome doing its job — not worth contorting the design over. Keep the distinction in mind before "fixing" anything here.
- **To check "does the UI touch the image", audit the live DOM rather than eyeballing.** Walk the ancestor chain from the `<video>` to `<html>`, then every element whose rect overlaps the picture, looking for `filter`, `opacity < 1`, `mix-blend-mode`, `backdrop-filter`, `mask`, and a translucent background or gradient painted over it — in pinned *and* grid, chrome shown *and* faded, scopes open. Enumerate *mechanisms* against the DOM; enumerating selectors by hand is what let the scopes window keep its blur through a whole "fix".
- **Chrome that floats over the picture is `--pill-bg-opaque`, not `--pill-bg`.** 72% translucent chrome blends the image into itself. `#scopes-window` is on that list too — a translucent panel blends the picture into the surface you read measurements off.
- **Nothing that floats over the picture may carry `backdrop-filter`** — it alters the image, which in a colour-grading room is the one thing the UI must never do. An element with `backdrop-filter` turns everything beneath it into an intermediate composited surface, pulling the video off its direct path and through an sRGB buffer; **Firefox shifts the transfer function doing it** (Chrome and Safari were byte-identical either side of the toggle). Measured off a SMPTE ingest, chrome visible vs hidden: 40% grey 102 → **114**, 75% white 191 → **198**, each colour bar +7 on its active channels, while 0 and 255 stayed pinned. Endpoints clamped, mid-tones lifted — a gamma change, not a brightness offset, and plainly visible as the toolbar auto-hid. New with #248 only because the chrome moved *over* the stage; before that the toolbar sat below it in flow and the top bar was a solid strip. The kill block is deliberately the **last** rule in the viewer stylesheet — `#more-sheet` and `.tile-name` set `backdrop-filter` earlier at equal specificity, so source order is what makes it stick. `.nav-pill` keeps its blur for the admin navbar, which floats over no video; `#chat-drop-overlay` and `#tour-blocker` keep theirs because they are momentary modal scrims meant to obscure the picture. Chrome legibility comes from `--pill-bg-opaque` instead, and `#top-center` uses `text-shadow` rather than `filter` (a filter is also a backdrop root).
- **The chrome fade is `opacity` + `visibility`, never `pointer-events` alone** — a faded control must not stay tabbable. `lockChrome()` returns a release function and the lock is *counted*, so overlapping holders can't un-hold each other; the tour takes one, because the tour's blocker eats the pointer events that would otherwise keep the chrome awake.
- **The active-speaker ring is `box-shadow: inset`**, not `outline` or an outer shadow: the call panel and the mobile carousel both clip, so anything drawn outside the tile box disappears along their edges.
- **Fullscreen targets `document.documentElement`**, not a tile — the pinned tile already fills the window, so the button's job is hiding the *browser's* chrome. iPhone Safari has no element fullscreen and still falls back to `webkitEnterFullscreen()` on the `<video>`.

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
- **Why it must restart OME, not hot-reload:** OME (v0.21.0) reads its SRT passphrase from `Server.xml` `${env:...}` only at process startup — `SIGHUP` reloads just `logger.xml`, and its REST API returns 403 for bind changes. So the backend can't push a new passphrase into a running OME. The bridge: `Server.xml` is unchanged (still `${env:...}`, one bind per leg); the backend writes both passphrases to `<data>/srt.env`; the `[program:ome]` command is the `ome_start.sh` wrapper that **reads `srt.env` then execs `ome_launcher.sh`**; the handler runs `supervisorctl restart ome` (`srt::restart_ome`) to re-read it. For that restart the unprivileged backend (`user=app`) needs the supervisor socket — `[unix_http_server]` is `chown=app:app chmod=0700` in `supervisord.conf`. The passphrases reach OME **only** via `srt.env` (the wrapper), never the container env.
- **`srt.env` is parsed, never sourced.** `ome_start.sh` runs as root and the file is the backend's to write, so sourcing it gave the backend user a root shell on the next OME restart — which the same user can trigger through that socket. The wrapper takes only `SRT_INGEST_PASSPHRASE`, `SRT_PLAYBACK_PASSPHRASE` (lowercase hex, what `srt::gen_passphrase` makes) and `SRT_PBKEYLEN` (16/24/32), ignoring any other line without echoing it. The backend still writes `export NAME='value'` lines, so an older image that did source the file reads the same thing after a rollback; a passphrase that is not hex would be dropped (encryption off on that leg), which nothing generates.
- OME's SRT passphrase is **bind-level (per-port), not per-stream**. Wire confidentiality only, *not* access control (still the admission webhook + SignedPolicy). Restarting OME briefly drops **every** stream (SRT + browser), and enabling a leg is a **hard cutover** (that leg's encoders / Farbplay clients must reconnect with the passphrase) — the admin UI confirms before applying.
- Reads are live: `srt-config` (admin) and `/api/watch/:slug` (playback → Farbplay) resolve from the DB (`srt::resolve`). A disabled leg writes an empty passphrase to `srt.env`, so its bind stays plaintext.
- Cold-boot ordering: the backend writes `srt.env` from the DB on startup (`srt::init_startup`, which also migrates the pre-split combined `srt_encryption_enabled` flag into the two per-leg flags); `ome_start.sh` waits up to ~10 s for the backend (priority 20) to write it before OME (priority 30) launches; fail-open to unencrypted is safe because ingest admission is fail-closed while the backend is down. Tests set `STREAM_DISABLE_OME_RESTART=1` to skip the `supervisorctl` shell-out.

**Timezones** (gh #263)
- `starts_at` / `expires_at` are stored as UTC `YYYY-MM-DD HH:MM:SS`. Rooms created before that fix may be off by the UTC offset — re-save them in admin to correct.
- **The studio timezone** (Settings → Timezone, `settings.timezone`, an IANA name; `GET`/`POST /api/admin/settings/timezone`) decides which clock admin times are *entered and shown* on: the room modal's `datetime-local` inputs, the room cards, and the invite (`12 Oct 2026, 09:00 Lisbon time (GMT+1)`). It is public through `/:slug/info`'s `timezone`, so the viewer's scheduled screen states the start in that zone and adds a "Your time:" line when the guest's offset differs at that instant. Unset, admin uses the browser's zone — byte-for-byte the old behaviour — and the viewer shows the guest's own time, labelled.
- **No tz data ships anywhere.** The backend checks only the name's shape (it has no zone database); the browser resolves it through `Intl`, and one that does not know the name falls back to its own zone (`isKnownZone`). `wallToUtc` resolves DST edges the way `new Date("YYYY-MM-DDTHH:MM")` does for local time — a repeated time is the earlier instant, a skipped one moves forward — and was checked against it every 15 minutes of a year in seven zones, Lord Howe's 30-minute and Troll's two-hour shifts included.
- **Chrome reports some zones by their legacy alias** (`Asia/Calcutta`, not `Asia/Kolkata`), so a guest's own-zone label can read "Calcutta time" there; WebKit gives the current name. Labels are the IANA name's last segment, not CLDR's display names, which say "Western European Time" where people say Lisbon.

**Docker (single container)**
- Everything is baked into the image — `docker restart` does NOT pick up code/config changes. Rebuild: `make dev` (selects `docker-compose.dev.yml`, builds from source, tagged `farbstrom:dev` — never `farbhaus/farbstrom`, so a later plain `docker compose up -d` on the same machine pulls the published image instead of running the local build under its name). `Server.xml`, `caddy/Caddyfile`, `supervisord.conf`, and the compiled `www/dist` are all baked in, so editing them on the host needs a rebuild too (except `./www` while the dev overlay's bind mount is active).
- `$` in `.env` values must be doubled (`$$`) — Compose interpolates `$VAR`.
- **The backend is compiled against a newer glibc than it runs on** (Debian bookworm 2.36 → the OME image's Ubuntu 22.04, 2.35). That works only while it needs no symbol newer than 2.35 — it needs at most 2.34 today — so the final stage runs `ldd` on the binary and fails the build on any `version … not found`, rather than shipping a backend that cannot start.
- Start order is supervisord **priority**, not `depends_on`: Valkey → backend → OME/LiveKit → Caddy, so the admission webhook (backend, `localhost:4001`) is up before OME accepts ingests. Admission is fail-closed — if the backend is down, ingests are denied (no unauthorised streaming).
- The OME admission webhook URL is `localhost:4001` (env-overridable `OME_WEBHOOK_URL` in `Server.xml`); LiveKit's Redis is `localhost:6379` (generated `livekit.yaml`) — no Docker service names resolve inside the single container.
- Privilege & secrets: Caddy/OME/LiveKit run as root (privileged ports / TURN); the backend (`user=app`) and Valkey (`user=valkey`) drop privileges. `supervisord.conf` removes the backend-only secrets (`JWT_SECRET`, `ADMIN_PASSWORD`, `LIVEKIT_API_KEY/SECRET`) from the third-party processes via `env -u` — add any new such secret to those four `-u` lists.
- Persist Caddy's `caddy_data` volume (`/root/.local/share/caddy`): it holds the internal CA + Let's Encrypt certs. Losing it re-issues certs on every recreate (Let's Encrypt rate limits) / regenerates the local CA.
- Keep the UDP RTC range narrow (50000-50100) — Docker writes one iptables rule per port; wide ranges make `compose up/down` take minutes.
- **Docker Desktop (macOS) gets a stuck UDP port and no container can bind it.** `com.docker.backend` registers the forward (`exposer.Add` lists `UDP 0.0.0.0:9999 -> 172.18.0.2:9999`, `docker port` shows it) but never creates the host socket, and **logs no error anywhere**. Every packet is dropped before it reaches the container, so an SRT ingest just never connects and OME logs its listener line and then nothing at all — `The SRT client has connected` is emitted *before* the admission webhook, so its absence proves no packet arrived rather than a rejection. **The poison is per-port and Docker-Desktop-wide, not per-container**: with farbstrom stopped, a throwaway `docker run --rm -d -p 9997:9997/udp -p 9999:9999/udp alpine sleep 60` had 9997 bound and 9999 not. **`docker compose restart` does NOT clear it, and neither does `down` + `up`** — only restarting Docker Desktop does. Diagnose with **`netstat -an -p udp | grep '\.9999 '`** (owner-independent; `lsof` without `+c 0` truncates the name to `com.docke` and a grep for `com.docker` matches nothing, so a healthy stack reads as broken). Confirm the port is genuinely free by binding it yourself — it will succeed, while a working port gives `EADDRINUSE`. Measured on Docker Desktop 4.89.0; unrelated to OME's version and to how many ports are published (reproduced with 4 published UDP ports). Linux forwards via `docker-proxy`/iptables, a different mechanism — untested there.
- Valkey is the BSD-3 fork of Redis 7.2; LiveKit talks to it as a plain RESP server, so the swap from upstream Redis is invisible.
