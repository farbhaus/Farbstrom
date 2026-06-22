#!/usr/bin/env bash
#
# install.sh — zero-checkout bootstrap for a Farbstrom deploy host.
#
# Fetches just the deploy artifacts (no source tree) into a target directory and
# hands off to deploy.sh. Both the repo and the farbhaus/farbstrom image are
# public, so this needs no credentials.
#
#   curl -fsSL https://raw.githubusercontent.com/farbhaus/Farbstrom/main/install.sh \
#     | bash -s -- stream.yourdomain.com
#
# Everything after `--` is forwarded to deploy.sh, so all of its flags work:
#   … | bash -s -- --behind-proxy stream.yourdomain.com
#   … | bash -s -- --init-env stream.yourdomain.com
#
# Env knobs:
#   FARBSTROM_REF=main        git ref/tag to fetch the deploy files from
#   FARBSTROM_DIR=/opt/farbstrom   where to place them
#   FARBSTROM_NO_RUN=1        fetch only, don't run deploy.sh (dry run)
set -euo pipefail

REPO="farbhaus/Farbstrom"
REF="${FARBSTROM_REF:-main}"
DIR="${FARBSTROM_DIR:-/opt/farbstrom}"
RAW="https://raw.githubusercontent.com/${REPO}/${REF}"

# Only the files a deploy host needs — base compose, env template, deploy script.
FILES=(docker-compose.yml .env.example deploy.sh)

SUDO=""
[[ ${EUID:-$(id -u)} -ne 0 ]] && command -v sudo >/dev/null 2>&1 && SUDO="sudo"

echo "==> Fetching Farbstrom deploy files (${REPO}@${REF}) into ${DIR}"
$SUDO mkdir -p "$DIR"
# Make the target writable by the invoking user so deploy.sh can write .env/data
# without sudo on every step.
[[ -n "$SUDO" ]] && $SUDO chown "$(id -u):$(id -g)" "$DIR"

for f in "${FILES[@]}"; do
  echo "    - $f"
  curl -fsSL "${RAW}/${f}" -o "${DIR}/${f}"
done
chmod +x "${DIR}/deploy.sh"

if [[ "${FARBSTROM_NO_RUN:-0}" == "1" ]]; then
  echo "==> FARBSTROM_NO_RUN=1 — files fetched, not starting. Run: ${DIR}/deploy.sh <domain>"
  exit 0
fi

echo "==> Handing off to deploy.sh"
cd "$DIR"
exec ./deploy.sh "$@"
