#!/bin/bash
set -euo pipefail

# OME reads its SRT passphrase from ${env:SRT_*_PASSPHRASE} in Server.xml at
# process startup only — v0.21.0 can't hot-reload the config and its REST API
# rejects bind changes. So SRT encryption is DB-managed by the backend (gh #208),
# which owns the runtime value: it writes /data/srt.env and restarts *this*
# supervisor program to apply a change. Read that file so each (re)start picks
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

# Parsed, never sourced. This script runs as root, and the file is written by
# the unprivileged backend — so sourcing it handed the backend user a root
# shell on the next OME restart, which it can trigger itself through the
# supervisor socket it is given for exactly that. Only the three variables the
# backend writes are taken, and only in the shape it writes them (passphrases
# are lowercase hex, `srt::gen_passphrase`); every other line is ignored, and
# never echoed, since the content is not ours to print.
if [ -f "$SRT_ENV" ]; then
    hex_re="^(export[[:space:]]+)?(SRT_INGEST_PASSPHRASE|SRT_PLAYBACK_PASSPHRASE)='?([0-9a-f]*)'?$"
    keylen_re="^(export[[:space:]]+)?SRT_PBKEYLEN='?(16|24|32)'?$"
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            '' | '#'*) continue ;;
        esac
        if [[ "$line" =~ $hex_re ]]; then
            export "${BASH_REMATCH[2]}=${BASH_REMATCH[3]}"
        elif [[ "$line" =~ $keylen_re ]]; then
            export "SRT_PBKEYLEN=${BASH_REMATCH[2]}"
        else
            echo "ome_start: ignored a line in $SRT_ENV that is not in the expected form" >&2
        fi
    done < "$SRT_ENV"
fi

exec /opt/ovenmediaengine/bin/ome_launcher.sh -c origin_conf
