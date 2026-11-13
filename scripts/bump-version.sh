#!/usr/bin/env bash
# Set the Farbstrom version in every file that carries it, or check that they
# agree. The release tag is the version; these files have to say the same.
#
#   scripts/bump-version.sh 1.2.3            write 1.2.3 into all four files
#   scripts/bump-version.sh --check v1.2.3   exit 1 unless all four say 1.2.3
#
# A leading `v` is accepted either way, so a tag name can be passed as-is.
#
# The four files: backend/Cargo.toml and backend/Cargo.lock (the backend's own
# entry), frontend/package.json and frontend/package-lock.json. Cargo.toml is
# the one that matters at runtime — the backend compiles it in
# (`CARGO_PKG_VERSION`) and the privacy page shows it — and the release
# workflow runs `--check` against the tag before publishing, so a release
# cannot ship naming the wrong version.
#
# Writes nothing to git. Bump, commit, merge to main, then tag main.
#
# Portable across GNU and BSD sed (`-i.bak`, then the backup is removed), so it
# runs the same on a Mac as in CI. Needs node and npm, which the frontend
# build needs anyway.

set -euo pipefail

cd "$(dirname "$0")/.."

CARGO_TOML=backend/Cargo.toml
CARGO_LOCK=backend/Cargo.lock
CRATE=stream-backend

usage() {
    echo "usage: $0 [--check] <X.Y.Z>" >&2
    exit 2
}

check=0
if [ "${1:-}" = "--check" ]; then
    check=1
    shift
fi
[ $# -eq 1 ] || usage

want="${1#v}"
if ! printf '%s' "$want" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "error: '$1' is not a version of the form X.Y.Z" >&2
    exit 2
fi

# --- read ------------------------------------------------------------------

cargo_toml_version() {
    # The `version =` line inside [package], not a dependency's.
    sed -n '/^\[package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$CARGO_TOML"
}

cargo_lock_version() {
    # The line right after the crate's own `name =` in its [[package]] entry.
    sed -n "/^name = \"$CRATE\"\$/{n;s/^version = \"\\(.*\\)\"\$/\\1/p;}" "$CARGO_LOCK"
}

npm_version() {
    node -p 'require("./frontend/package.json").version'
}

npm_lock_versions() {
    # npm keeps the root package's version twice in a v3 lockfile.
    node -p 'const l = require("./frontend/package-lock.json"); l.version + " " + l.packages[""].version'
}

verify() {
    local bad=0 got
    for pair in \
        "$CARGO_TOML:$(cargo_toml_version)" \
        "$CARGO_LOCK:$(cargo_lock_version)" \
        "frontend/package.json:$(npm_version)"; do
        got="${pair#*:}"
        if [ "$got" != "$want" ]; then
            echo "  ${pair%%:*} says '${got}', want '$want'" >&2
            bad=1
        fi
    done
    for got in $(npm_lock_versions); do
        if [ "$got" != "$want" ]; then
            echo "  frontend/package-lock.json says '$got', want '$want'" >&2
            bad=1
        fi
    done
    return $bad
}

if [ $check -eq 1 ]; then
    if verify; then
        echo "Version files all say $want."
        exit 0
    fi
    echo "error: version files do not match $want — run scripts/bump-version.sh $want, commit, and re-tag." >&2
    exit 1
fi

# --- write -----------------------------------------------------------------

sed -i.bak "/^\[package\]/,/^\[/s/^version = \".*\"\$/version = \"$want\"/" "$CARGO_TOML"
rm -f "$CARGO_TOML.bak"

sed -i.bak "/^name = \"$CRATE\"\$/{n;s/^version = \".*\"\$/version = \"$want\"/;}" "$CARGO_LOCK"
rm -f "$CARGO_LOCK.bak"

# npm rewrites package.json and both lockfile entries; --no-git-tag-version
# keeps it from committing or tagging on its own.
(cd frontend && npm version "$want" --no-git-tag-version --allow-same-version >/dev/null)

# Read everything back, so a sed that silently matched nothing fails here
# rather than in the release workflow.
if ! verify; then
    echo "error: a file was not updated — see above." >&2
    exit 1
fi
echo "Set version $want in $CARGO_TOML, $CARGO_LOCK, frontend/package.json, frontend/package-lock.json."
