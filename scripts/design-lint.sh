#!/usr/bin/env bash
#
# Design-system lint.
#
# www/shared/tokens.css is the single source of truth for every raw design value
# (see the "Design system" section of CLAUDE.md). This script enforces that:
#
#   1. No stylesheet outside tokens.css hardcodes a radius, z-index, font size,
#      font weight, letter-spacing or color.
#   2. Every var(--…) reference resolves to a token that actually exists.
#   3. Every token defined in tokens.css is actually used.
#
# It exists because the convention was already documented and had drifted anyway:
# 43 hardcoded radii had accumulated, 28 of them exact restatements of a token
# that already existed. Greps are cheap; the drift was not.
#
# Run from the repo root:  ./scripts/design-lint.sh

set -uo pipefail
cd "$(dirname "$0")/.."

# Page stylesheets + the shared component/utility layers. tokens.css is
# deliberately absent: it is the one file allowed to hold raw values.
PAGES=(www/admin/index.html www/viewer/index.html www/landing/index.html www/privacy/index.html)
SHARED=(www/shared/components.css www/shared/utils.css)
ALL=("${SHARED[@]}" "${PAGES[@]}")

fail=0

# report <description> <grep-output>
report() {
    local what="$1" hits="$2"
    if [[ -n "$hits" ]]; then
        printf '\n\033[31mFAIL\033[0m  %s\n' "$what"
        printf '%s\n' "$hits" | sed 's/^/        /'
        fail=1
    else
        printf '\033[32mok\033[0m    %s\n' "$what"
    fi
}

# --- 1. Raw values outside tokens.css ---------------------------------------

# .btn-tab deliberately resets its radius to 0 — an underlined tab has no
# corners. It is the single sanctioned exception.
report "no hardcoded border-radius" \
    "$(grep -rn 'border-radius: *[0-9]' "${ALL[@]}" | grep -v 'border-radius: 0;' || true)"

report "no hardcoded z-index" \
    "$(grep -rn 'z-index: *[0-9]' "${ALL[@]}" || true)"

report "no hardcoded font-size" \
    "$(grep -rnE 'font-size: ?[0-9]+px' "${ALL[@]}" || true)"

report "no hardcoded font-weight" \
    "$(grep -rnE 'font-weight: ?[0-9]{3}' "${ALL[@]}" || true)"

report "no hardcoded letter-spacing" \
    "$(grep -rnE 'letter-spacing: ?0\.[0-9]+em' "${ALL[@]}" || true)"

# Issue references in comments (#186, #204, #211 …) are 3-digit and would match a
# naive hex pattern, so require 6 hex digits. Data-URI SVGs legitimately carry
# %23-escaped colors and are skipped.
report "no hex/rgba colors outside tokens.css" \
    "$(grep -rnoE '#[0-9a-fA-F]{6}\b|rgba?\([0-9 ,.]+\)' "${ALL[@]}" \
        | grep -v 'svg+xml' | grep -v '%23' || true)"

# --- 1b. Nothing may blur the picture in the viewer -------------------------
#
# A backdrop-filter over the stream composites the video through an intermediate
# surface, and Firefox shifts its transfer function doing it — measured off a
# SMPTE ingest, 40% grey read 102 -> 114 with the chrome visible (#248). In a
# colour-grading room that is a defect, not a cosmetic issue. The viewer kills
# it for everything inside #app; this only guards the kill block against being
# deleted, since specificity already covers anything newly added.
report "viewer still cancels backdrop-filter over the picture" \
    "$(grep -q '^#app, #app \*:not(#chat-drop-overlay) {' www/viewer/index.html \
        || echo 'www/viewer/index.html: the #248 backdrop-filter kill block is gone')"

# --- 2 & 3. Token cross-reference ------------------------------------------

python3 - <<'PY'
import glob, re, sys

defined = set(re.findall(r'^\s*(--[a-z0-9-]+)\s*:',
                         open('www/shared/tokens.css').read(), re.M))

used: dict[str, set[str]] = {}
for path in (glob.glob('www/**/*.css', recursive=True)
             + glob.glob('www/**/*.html', recursive=True)
             + glob.glob('frontend/**/*.ts', recursive=True)):
    if '/dist/' in path:            # build output, regenerated
        continue
    for m in re.finditer(r'var\((--[a-z0-9-]+)', open(path).read()):
        used.setdefault(m.group(1), set()).add(path)

# Tokens legitimately referenced without a :root declaration — set per element
# from JS with a CSS fallback. Empty since #248 retired --focus-aspect.
UNDEFINED_OK: set[str] = set()
# Nothing is currently read from JS via getComputedStyle instead of a var(), so
# every token has to be referenced somewhere. Add a name here only if a token is
# genuinely consumed in a way this scan cannot see.
UNUSED_OK: set[str] = set()

bad = False

undefined = sorted(set(used) - defined - UNDEFINED_OK)
if undefined:
    print('\n\033[31mFAIL\033[0m  var() references a token that does not exist')
    for t in undefined:
        print(f'        {t}  <- {", ".join(sorted(used[t]))}')
    bad = True
else:
    print('\033[32mok\033[0m    every var() resolves to a defined token')

unused = sorted(defined - set(used) - UNUSED_OK)
if unused:
    print('\n\033[31mFAIL\033[0m  token defined in tokens.css but never used')
    for t in unused:
        print(f'        {t}')
    print('        Delete it, or apply it. A dead token is a decision nobody made.')
    bad = True
else:
    print('\033[32mok\033[0m    no dead tokens')

sys.exit(1 if bad else 0)
PY
[[ $? -ne 0 ]] && fail=1

# --- Result -----------------------------------------------------------------

echo
if [[ $fail -ne 0 ]]; then
    echo "design-lint: FAILED — see above. Raw values belong in www/shared/tokens.css."
    exit 1
fi
echo "design-lint: all checks passed."
