#!/usr/bin/env bash
#
# deploy.sh — one-click production deployment for Farbstrom.
#
# From a clean checkout to a running TLS deployment in one command:
#   ./deploy.sh stream.yourdomain.com      (run with bash, not sh)
#
# Designed for a CLEAN VPS where ONLY Farbstrom runs. It installs missing
# prerequisites (Docker + Compose, openssl) on apt-based hosts,
# generates secrets into .env, opens the firewall, and brings the stack up by
# pulling the published single-container image (which bakes in the frontend, so
# no Node/build toolchain is needed on the host). The containerized Caddy
# provisions Let's Encrypt and owns ALL routing — app, /live/* (OME), and
# LiveKit (proxied same-origin at /livekit/*). One domain, one cert, no host
# web server to configure.
#
# Usage:
#   ./deploy.sh stream.yourdomain.com           standalone deploy (container does TLS)
#   ./deploy.sh --update                         pull the newest image + recreate
#   ./deploy.sh --behind-proxy lk.example.com    deploy behind an external TLS proxy
#   ./deploy.sh --init-env stream.example.com    write .env only, don't start anything
#
# Flags: --regenerate          start a fresh .env, rotating secrets
#        --yes / -y            skip confirmation prompts
#        --update              reuse .env, pull newest image, recreate (no secret
#                              rotation, so live sessions survive); rollback by
#                              pinning FARBSTROM_TAG=sha-<short> in .env first
#        --behind-proxy HOST   container serves plain HTTP on 127.0.0.1:HTTP_PORT;
#                              an external proxy (e.g. host Caddy) terminates TLS
#                              and forwards HOST → it. Skips firewall + the 80/443
#                              free-port check. HOST is the browser-facing domain.
#        --init-env            generate/refresh .env and exit (no image, no start)
#
# Re-running is safe: an existing .env is reused as-is (secrets are NOT rotated,
# so live sessions survive a redeploy). Use --regenerate to start fresh.
#
set -euo pipefail
umask 077          # secrets written to .env must not be world-readable

# --- locate repo root -------------------------------------------------------
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if [[ ! -f .env.example ]]; then
  echo "FATAL: .env.example not found next to deploy.sh — run from the repo checkout." >&2
  exit 1
fi

# --- args -------------------------------------------------------------------
REGENERATE=0
ASSUME_YES=0
UPDATE=0           # --update: pull newest image + recreate, reuse .env
INIT_ENV_ONLY=0    # --init-env: write .env and stop (no image, no start)
BEHIND_PROXY=""    # --behind-proxy HOST: external TLS proxy fronts the container
DOMAIN=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --regenerate) REGENERATE=1 ;;
    --yes|-y) ASSUME_YES=1 ;;
    --update) UPDATE=1 ;;
    --init-env) INIT_ENV_ONLY=1 ;;
    --behind-proxy)
      shift
      [[ -n "${1:-}" && "$1" != -* ]] || { echo "FATAL: --behind-proxy needs the public host, e.g. --behind-proxy stream.example.com" >&2; exit 1; }
      BEHIND_PROXY="$1" ;;
    --behind-proxy=*) BEHIND_PROXY="${1#*=}" ;;
    -*) echo "FATAL: unknown option: $1" >&2; exit 1 ;;
    *) DOMAIN="$1" ;;
  esac
  shift
done

# In behind-proxy mode the public host comes from --behind-proxy, so a positional
# domain is optional; mirror it into DOMAIN so the shared .env logic has one name.
[[ -n "$BEHIND_PROXY" && -z "$DOMAIN" ]] && DOMAIN="$BEHIND_PROXY"

# Privilege prefix for host-level changes (installing packages, firewall).
SUDO=""
[[ ${EUID:-$(id -u)} -ne 0 ]] && SUDO="sudo"

# --- helpers ----------------------------------------------------------------
die()  { echo "FATAL: $*" >&2; exit 1; }
info() { echo "==> $*"; }

# All openssl-based: a `tr </dev/urandom | head` pipeline trips SIGPIPE, which
# under `set -o pipefail` + `set -e` silently aborts the whole script.
gen_secret()   { openssl rand -hex 32; }   # 64 chars, satisfies the >=32 rule in backend/src/config.rs
gen_password() { openssl rand -hex 16; }   # 32 chars, > the 12-char ADMIN_PASSWORD minimum
gen_token()    { openssl rand -hex 4; }    # 8 chars, LIVEKIT_API_KEY suffix

# set_env KEY VALUE — replace the `KEY=...` line in .env in place, preserving
# all comments / blank lines / ordering. Portable (no GNU-vs-BSD `sed -i`).
set_env() {
  local key="$1" value="$2" tmp
  tmp="$(mktemp)"
  local found=0
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" == "${key}="* ]]; then
      printf '%s=%s\n' "$key" "$value" >>"$tmp"
      found=1
    else
      printf '%s\n' "$line" >>"$tmp"
    fi
  done <.env
  [[ $found -eq 1 ]] || printf '%s=%s\n' "$key" "$value" >>"$tmp"
  mv "$tmp" .env
}

have_apt() { command -v apt-get >/dev/null 2>&1; }

# install_apt PKG... — install Debian/Ubuntu packages.
install_apt() {
  info "Installing $* (apt)"
  $SUDO apt-get update
  $SUDO apt-get install -y "$@"
}

# install_docker — Docker Engine + Compose v2 plugin via the official script.
install_docker() {
  have_apt || die "Docker not found and auto-install only supports apt-based distros. Install Docker Engine + the Compose v2 plugin: https://docs.docker.com/engine/install/"
  info "Installing Docker Engine + Compose plugin (get.docker.com)"
  curl -fsSL https://get.docker.com | $SUDO sh
  $SUDO systemctl enable --now docker
  # Let the invoking user run docker without sudo on future runs (takes effect
  # after re-login; this run still falls back to sudo via the DOCKER resolver).
  local u="${SUDO_USER:-$USER}"
  [[ -n "$SUDO" && -n "$u" ]] && $SUDO usermod -aG docker "$u" || true
}

# http_ports_busy — true if something is already listening on :80 or :443
# (an existing web server / reverse proxy). This one-click script can't work then.
# Uses ss's port filter (no `-H`, which old iproute2 lacks) and keys on the
# LISTEN state so the header line can't false-match.
http_ports_busy() {
  if command -v ss >/dev/null 2>&1; then
    ss -tln 'sport = :80 or sport = :443' 2>/dev/null | grep -q LISTEN
  elif command -v netstat >/dev/null 2>&1; then
    netstat -tln 2>/dev/null \
      | grep -qE '[[:space:]][0-9.:*[]+:(80|443)[[:space:]].*LISTEN'
  else
    return 1   # can't tell — don't block
  fi
}

# stack_running — true if our own compose stack is already up (so :80/:443
# being held by our container is expected, not a foreign-service conflict).
stack_running() {
  $DOCKER compose -f docker-compose.yml ps --status running --services 2>/dev/null \
    | grep -qx farbstrom
}

# ensure_image — make the single-container image available locally under the ref
# the compose file expects. Tries the published image first; if the registry has
# no build for this host's platform (published image is linux/amd64 only, so
# arm64 hosts get "no matching manifest") or the tag is missing, falls back to
# building from the repo root. The Dockerfile builds the backend AND frontend
# internally, so the build needs no host toolchain either way.
ensure_image() {
  if $DOCKER compose -f docker-compose.yml pull 2>/dev/null; then
    return
  fi
  # `|| true` is load-bearing: FARBSTROM_TAG is usually commented out, so grep
  # finds nothing and exits non-zero — under `set -euo pipefail` that would
  # silently abort the script on this assignment.
  local tag image
  tag="$( { grep -E '^FARBSTROM_TAG=' .env 2>/dev/null || true; } | cut -d= -f2-)"
  image="farbhaus/farbstrom:${tag:-latest}"
  info "No published image for this platform/tag — building from source ($image)"
  $DOCKER build -t "$image" .
}

# wait_healthy [timeout_s] — block until the container's Docker healthcheck
# reports `healthy`, so the script only claims success once the web tier is
# actually serving. On timeout, dump recent logs and fail (non-zero exit) rather
# than print a misleading "deployed". The compose healthcheck has a start_period
# during which the status is `starting`; we simply keep waiting through it.
wait_healthy() {
  local timeout="${1:-90}" elapsed=0 status
  info "Waiting for the container to become healthy (up to ${timeout}s)…"
  while (( elapsed < timeout )); do
    status="$($DOCKER inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' farbstrom 2>/dev/null || echo missing)"
    case "$status" in
      healthy) info "Container is healthy."; return 0 ;;
      none)    info "Container has no healthcheck — skipping health wait."; return 0 ;;
    esac
    sleep 3; elapsed=$((elapsed + 3))
  done
  echo "FATAL: container did not become healthy within ${timeout}s. Recent logs:" >&2
  $DOCKER compose -f docker-compose.yml logs --tail 40 farbstrom >&2 2>/dev/null || true
  return 1
}

# Ports the stack needs reachable from the internet. Single source of truth
# for both open_firewall and the printed summary.
FW_TCP=(80 443 1935 3478 7881)
FW_UDP=(443 9999 9998 10000:10009 50000:50100)

# open_firewall — add allow rules to whatever firewall is ALREADY active.
# Deliberately does NOT enable an inactive firewall (that risks an SSH
# lockout and is unnecessary — if nothing's filtering, the ports are open).
open_firewall() {
  if command -v ufw >/dev/null 2>&1 && $SUDO ufw status 2>/dev/null | grep -q "Status: active"; then
    info "Opening ports via ufw"
    $SUDO ufw allow 22/tcp >/dev/null 2>&1 || true   # never lock out SSH
    local p
    for p in "${FW_TCP[@]}"; do $SUDO ufw allow "${p/:/-}/tcp" >/dev/null 2>&1 || true; done
    for p in "${FW_UDP[@]}"; do $SUDO ufw allow "${p/:/-}/udp" >/dev/null 2>&1 || true; done
    $SUDO ufw reload >/dev/null 2>&1 || true
  elif command -v firewall-cmd >/dev/null 2>&1 && $SUDO firewall-cmd --state >/dev/null 2>&1; then
    info "Opening ports via firewalld"
    local p
    for p in "${FW_TCP[@]}"; do $SUDO firewall-cmd --permanent --add-port="${p/:/-}/tcp" >/dev/null 2>&1 || true; done
    for p in "${FW_UDP[@]}"; do $SUDO firewall-cmd --permanent --add-port="${p/:/-}/udp" >/dev/null 2>&1 || true; done
    $SUDO firewall-cmd --reload >/dev/null 2>&1 || true
  else
    info "No active host firewall (ufw/firewalld) — skipping."
    echo "    If your VPS provider has a CLOUD firewall, open these there:"
    echo "      tcp: ${FW_TCP[*]}"
    echo "      udp: ${FW_UDP[*]}"
  fi
}

# --- verify prerequisites ---------------------------------------------------
info "Checking prerequisites"
command -v curl    >/dev/null 2>&1 || install_apt curl ca-certificates
command -v openssl >/dev/null 2>&1 || install_apt openssl

# --init-env only writes .env (openssl-only); skip all Docker setup for it.
DOCKER="docker"
if [[ $INIT_ENV_ONLY -eq 0 ]]; then
  command -v docker  >/dev/null 2>&1 || install_docker

  # Docker present but no Compose v2 plugin (e.g. distro docker.io): add it.
  if command -v docker >/dev/null 2>&1 && ! docker compose version >/dev/null 2>&1; then
    have_apt && install_apt docker-compose-plugin
  fi

  # Resolve how to call docker: a freshly added docker group doesn't apply to
  # this shell, so fall back to sudo if the daemon isn't reachable unprivileged.
  if ! docker info >/dev/null 2>&1; then
    if [[ -n "$SUDO" ]] && $SUDO docker info >/dev/null 2>&1; then
      DOCKER="$SUDO docker"
      info "Using '$DOCKER' (re-login, or 'newgrp docker', to drop sudo for docker)"
    fi
  fi

  # Re-verify; fail clearly if an install didn't take.
  for c in openssl docker; do
    command -v "$c" >/dev/null 2>&1 || die "$c still missing after install attempt — install it manually and re-run."
  done
  $DOCKER compose version >/dev/null 2>&1 || die "Docker Compose v2 still unavailable — install the Compose plugin and re-run."
fi

# --- update mode ------------------------------------------------------------
# Pull the newest image (or the FARBSTROM_TAG pinned in .env) and recreate.
# Secrets are untouched, so live sessions survive. Rollback = pin a previous
# FARBSTROM_TAG=sha-<short> in .env, then re-run --update.
if [[ $UPDATE -eq 1 ]]; then
  [[ -f .env ]] || die "--update needs an existing .env (run a normal deploy first)."
  info "Pulling newest image"
  $DOCKER compose -f docker-compose.yml pull
  info "Recreating the container"
  $DOCKER compose -f docker-compose.yml up -d
  wait_healthy || die "update failed — the container is not healthy (see logs above)."
  info "Update complete."
  exit 0
fi

# --- domain -----------------------------------------------------------------
if [[ -z "$DOMAIN" ]]; then
  read -rp "Production domain (e.g. stream.example.com): " DOMAIN
fi
[[ -n "$DOMAIN" ]] || die "a production domain is required (needed for TLS)."
# DOMAIN flows into SITE_ADDRESS, PUBLIC_ORIGIN and the Caddy site address.
# Accepted: `localhost` (local dev — Caddy uses its internal CA, passkeys work
# because localhost is a valid WebAuthn RP ID) or a bare FQDN with at least one
# dot (no scheme, path, port, spaces). Bare IPs are rejected: Let's Encrypt
# won't issue for them and an IP has no domain, so build_webauthn would panic.
if [[ "$DOMAIN" == "localhost" ]]; then
  : # ok — supported local-dev value
elif [[ "$DOMAIN" == *://* || "$DOMAIN" == */* || "$DOMAIN" =~ [[:space:]] ]]; then
  die "'$DOMAIN' is not a bare hostname. Use e.g. stream.example.com — no https://, no path, no port."
elif [[ "$DOMAIN" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ || "$DOMAIN" == *:* ]]; then
  die "bare IPs aren't supported (no Let's Encrypt TLS, no passkeys). Use 'localhost' for local dev or a real domain like stream.example.com."
elif [[ ! "$DOMAIN" =~ ^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?)+$ ]]; then
  die "'$DOMAIN' is not a bare hostname. Use e.g. stream.example.com — no https://, no path, no port."
fi

# Fail fast (don't emit a cryptic Docker port-bind error) if something already
# holds 80/443 — UNLESS it's our own already-running stack (a redeploy), which
# must stay idempotent.
# Skipped when we won't bind 80/443: --init-env (writes .env only) and
# --behind-proxy (the external proxy is SUPPOSED to hold them; the container
# binds 127.0.0.1:HTTP_PORT instead).
if [[ $INIT_ENV_ONLY -eq 0 && -z "$BEHIND_PROXY" ]] && http_ports_busy && ! stack_running; then
  die "ports 80/443 are already in use — standalone deploy expects a fresh VPS where only Farbstrom runs.
       Free 80/443, or if this host already runs a reverse proxy, deploy behind it:
       ./deploy.sh --behind-proxy $DOMAIN"
fi

# --- .env handling ----------------------------------------------------------
ADMIN_PASSWORD=""

# env_complete — true if .env has all required secrets non-empty. Guards
# against reusing a half-written .env left by an aborted earlier run.
env_complete() {
  local k
  for k in JWT_SECRET OME_WEBHOOK_SECRET OME_API_TOKEN LIVEKIT_API_SECRET ADMIN_PASSWORD; do
    grep -qE "^${k}=.+" .env || return 1
  done
  return 0
}

if [[ -f .env ]] && ! env_complete; then
  info ".env exists but is missing required secrets (previous run likely aborted) — regenerating"
  rm -f .env
fi

if [[ -f .env && $REGENERATE -eq 1 ]]; then
  echo "Note: this rotates JWT/secrets (invalidates all sessions). It does NOT"
  echo "reset DB-stored credentials — a custom admin password, TOTP, and passkeys"
  echo "live in ./data/stream.db and keep working (and override the env password)."
  if [[ $ASSUME_YES -eq 1 ]]; then
    info "--regenerate --yes: overwriting .env with fresh secrets"
  else
    read -rp ".env exists — overwrite it and generate fresh secrets? [y/N] " ans
    [[ "${ans,,}" == "y" ]] || die "aborted by user."
  fi
  rm -f .env
fi

if [[ -f .env ]]; then
  info "Reusing existing .env (secrets unchanged)"
  # The reused .env keeps its own SITE_ADDRESS — the positional DOMAIN does NOT
  # overwrite it (idempotency: a redeploy must not silently change the host).
  # Warn so `./deploy.sh new.example.com` over an old .env isn't a silent no-op.
  existing_site="$( { grep -E '^SITE_ADDRESS=' .env 2>/dev/null || true; } | head -1 | cut -d= -f2-)"
  if [[ -n "$existing_site" && "$existing_site" != "$DOMAIN" ]]; then
    info "NOTE: .env already targets '$existing_site' — keeping it (ignoring '$DOMAIN')."
    info "      To switch host: edit SITE_ADDRESS/PUBLIC_ORIGIN/LIVEKIT_URL in .env, or re-run with --regenerate."
  fi
  # Backfill secrets introduced in later versions so upgrading an existing
  # deployment picks them up without a manual edit or a full secret rotation.
  # OME_SIGNED_POLICY_SECRET (Farbplay SRT room-link flow) is read by both the
  # backend (env_file) and OME (compose interpolation), so a single value in
  # .env keeps them in sync once both containers are recreated below.
  if ! grep -qE "^OME_SIGNED_POLICY_SECRET=.+" .env; then
    info "Adding OME_SIGNED_POLICY_SECRET to existing .env"
    set_env OME_SIGNED_POLICY_SECRET "$(gen_secret)"
  fi
else
  info "Generating .env for $DOMAIN"
  cp .env.example .env
  ADMIN_PASSWORD="$(gen_password)"
  set_env DOMAIN             "$DOMAIN"
  if [[ -n "$BEHIND_PROXY" ]]; then
    # Behind an external TLS proxy: the container's own Caddy serves plain HTTP
    # on loopback, the proxy terminates TLS. SITE_ADDRESS is just a listen addr
    # (":80"), so PUBLIC_HOST carries the real browser-facing host; WEB_BIND
    # keeps the plain-HTTP port off the internet (Docker bypasses ufw).
    set_env SITE_ADDRESS       ":80"
    set_env PUBLIC_HOST        "$DOMAIN"
    set_env WEB_BIND           "127.0.0.1"
    set_env HTTP_PORT          "8880"
    set_env HTTPS_PORT         "8444"
  else
    # Standalone: the container Caddy provisions Let's Encrypt for the domain.
    set_env SITE_ADDRESS       "$DOMAIN"
  fi
  # LiveKit is proxied same-origin at /livekit/*, so the browser sees one wss
  # origin — no separate subdomain, DNS record, or cert. PUBLIC_ORIGIN is the
  # WebAuthn relying party and must match the browser origin exactly. In the
  # container both are re-derived from PUBLIC_HOST/SITE_ADDRESS by entrypoint.sh;
  # these literals keep a hand-run `cargo run` / non-container use correct too.
  set_env LIVEKIT_URL        "wss://$DOMAIN/livekit"
  set_env PUBLIC_ORIGIN      "https://$DOMAIN"
  set_env LIVEKIT_API_KEY    "API$(gen_token)"
  set_env JWT_SECRET         "$(gen_secret)"
  set_env OME_WEBHOOK_SECRET "$(gen_secret)"
  set_env OME_SIGNED_POLICY_SECRET "$(gen_secret)"
  set_env OME_API_TOKEN      "$(gen_secret)"
  set_env LIVEKIT_API_SECRET "$(gen_secret)"
  set_env ADMIN_PASSWORD     "$ADMIN_PASSWORD"
  chmod 600 .env
fi

# --- init-env mode ----------------------------------------------------------
# Stop here: .env is written but nothing is pulled or started. For hosts that
# bring the stack up themselves (e.g. behind an existing proxy with their own
# orchestration).
if [[ $INIT_ENV_ONLY -eq 1 ]]; then
  info ".env ready at $(pwd)/.env"
  [[ -n "$ADMIN_PASSWORD" ]] && { echo; echo "  ADMIN PASSWORD (shown once — save it now): $ADMIN_PASSWORD"; }
  echo "  Start when ready:  docker compose -f docker-compose.yml up -d"
  exit 0
fi

# --- prepare image + data dir -----------------------------------------------
# The image bakes in www/dist (frontend built inside the Dockerfile), so there's
# no host frontend build and ./www is not bind-mounted in production.
info "Preparing ./data"
mkdir -p data
ensure_image
# No host-side chown: the container fixes /data ownership itself at startup
# (entrypoint.sh chowns it to the unprivileged backend user before the backend
# starts), which works for a root-owned bind mount on a fresh VPS.

# --- firewall ---------------------------------------------------------------
# Skipped behind a proxy: the container binds 127.0.0.1 for HTTP/HTTPS, so there
# is nothing host-public to open; the front proxy owns 80/443 exposure.
if [[ -n "$BEHIND_PROXY" ]]; then
  info "Behind a proxy — skipping host firewall (web ports bound to 127.0.0.1)."
else
  open_firewall
fi

# --- deploy -----------------------------------------------------------------
# -f docker-compose.yml selects ONLY the base file so the dev overlay
# (docker-compose.dev.yml, which builds from source) is NOT merged. The
# single-container image is already local (pulled or built by ensure_image).
info "Starting stack"
$DOCKER compose -f docker-compose.yml up -d
wait_healthy || die "deploy failed — the container is not healthy (see logs above)."

# --- summary ----------------------------------------------------------------
echo
echo "============================================================"
echo " Farbstrom deployed: https://$DOMAIN"
echo "============================================================"
if [[ -n "$ADMIN_PASSWORD" ]]; then
  echo
  echo "  ADMIN PASSWORD (shown once — save it now):"
  echo "      $ADMIN_PASSWORD"
  if [[ -f data/stream.db ]]; then
    echo
    echo "  NOTE: data/stream.db already exists. If a custom password was set in"
    echo "  the admin UI, it (and any TOTP/passkey) lives in the DB and OVERRIDES"
    echo "  this env password — the password above will NOT work until the DB"
    echo "  override is cleared (break-glass). TOTP/passkeys are unaffected."
  fi
fi
if [[ -n "$BEHIND_PROXY" ]]; then
  cat <<EOF

  Next steps (behind an external TLS proxy):
   - Point your proxy for $DOMAIN at the container's HTTP port:
       reverse_proxy 127.0.0.1:8880      (Caddy syntax)
   - The proxy terminates TLS; the container serves plain HTTP on loopback only.
   - Update later:  ./deploy.sh --update
   - Check health:  $DOCKER compose -f docker-compose.yml ps

EOF
else
  cat <<EOF

  Next steps:
   - Point DNS at this host for:
       $DOMAIN
     (The container Caddy provisions Let's Encrypt on first hit.)
   - Firewall: handled above (or listed there if no host firewall is active —
     open those on your VPS provider's cloud firewall if you have one).
   - Update later:  ./deploy.sh --update
   - Check health:
       $DOCKER compose -f docker-compose.yml ps
       $DOCKER compose -f docker-compose.yml logs -f farbstrom

EOF
fi
