#!/bin/bash
set -euo pipefail

# OME reads its SRT passphrase from ${env:SRT_*_PASSPHRASE} in Server.xml at
# process startup only — v0.20.5 can't hot-reload the config and its REST API
# rejects bind changes. So SRT encryption is DB-managed by the backend (gh #208),
# which owns the runtime value: it writes /data/srt.env and restarts *this*
# supervisor program to apply a change. Source that file so each (re)start picks
# up the current passphrase. Empty / missing => encryption off (backward compat).
SRT_ENV=/data/srt.env

# On a cold boot the backend (supervisord priority 20) writes srt.env before we
# (priority 30) need it, but priority only orders *launch*, not readiness — so
# wait briefly for the file. Fail open to unencrypted after the timeout: ingest
# admission is fail-closed while the backend is down, so no stream can flow
# unencrypted anyway.
i=0
while [ ! -f "$SRT_ENV" ] && [ "$i" -lt 20 ]; do
    sleep 0.5
    i=$((i + 1))
done

if [ -f "$SRT_ENV" ]; then
    # shellcheck disable=SC1090
    . "$SRT_ENV"
fi

exec /opt/ovenmediaengine/bin/ome_launcher.sh -c origin_conf
