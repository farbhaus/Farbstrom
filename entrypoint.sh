#!/bin/bash
set -euo pipefail

# Exported so the COPY'd /etc/caddy/Caddyfile ({$SITE_ADDRESS:localhost}) and the
# derived URLs below all resolve from one knob.
export SITE_ADDRESS="${SITE_ADDRESS:-localhost}"

mkdir -p /data /var/log/supervisor

# The backend runs unprivileged (supervisord `user=app`) and owns the SQLite DB
# + uploads under /data. /data is a bind mount/volume, so fix ownership here at
# runtime — a build-time chown wouldn't survive the mount.
chown -R app:app /data

# Generate LiveKit config pointing at local Valkey. Keys are inlined into the
# YAML here (entrypoint still has the full env), so the livekit process itself
# needs no key secrets in its environment — supervisord blanks them for it.
LIVEKIT_API_KEY="${LIVEKIT_API_KEY:-devkey}"
LIVEKIT_API_SECRET="${LIVEKIT_API_SECRET:-secret}"
cat > /livekit.yaml <<EOF
port: 7880
rtc:
  tcp_port: 7881
  port_range_start: 50000
  port_range_end: 50100
  use_external_ip: true
redis:
  address: localhost:6379
keys:
  ${LIVEKIT_API_KEY}: ${LIVEKIT_API_SECRET}
logging:
  level: info
EOF

# Browser-facing URLs. In the standalone model Caddy is this container's own TLS
# edge, so SITE_ADDRESS (the Caddy site address, e.g. stream.example.com) is also
# the public host — PUBLIC_HOST defaults to it and the single-knob deploy needs
# nothing more. When the container instead runs behind an external TLS reverse
# proxy, SITE_ADDRESS is just an internal listen address (e.g. ":80") and is NOT
# a usable hostname; set PUBLIC_HOST to the real browser-facing domain in that
# case. Either way the derived values override any stale per-flow .env value
# (e.g. the bare `cargo run` dev default of http://localhost:4001).
export PUBLIC_HOST="${PUBLIC_HOST:-$SITE_ADDRESS}"
export PUBLIC_ORIGIN="https://${PUBLIC_HOST}"
export LIVEKIT_URL="wss://${PUBLIC_HOST}/livekit"

# OME's <Distribution> host. Must come after PUBLIC_HOST is resolved above — it
# used to read a `DOMAIN` var that this codebase never sets, so it silently
# resolved to "localhost" everywhere.
export OME_HOST_IP="${PUBLIC_HOST}"

exec /usr/bin/supervisord -c /etc/supervisor/conf.d/supervisord.conf
