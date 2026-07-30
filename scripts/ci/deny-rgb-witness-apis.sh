#!/usr/bin/env bash
#
# CI guard: no mercury code may call rgb-lib's `update_witnesses` / `upsert_witness`.
#
# Why this is a hard deny and not a review convention:
#
#   A single `update_witnesses` call with the plain blockchain resolver used to permanently and
#   SILENTLY destroy a deliberately un-broadcast (Tentative) colored ladder. The indexer answers
#   `Unresolved` for a tx that was never broadcast, `Unresolved` maps to `WitnessOrd::Archived`,
#   and the archival then recurses into every descendant bundle. The call returns
#   `succeeded=N, failed={}` — no error — and `get_asset_balance` does not move, because the
#   balance comes from rgb-lib's sqlite tables while the destruction happens in the RGB stock.
#   Reproduced against the live regtest stack; see docs/utexo/CTESR-GATE.md section 2.3 (E7).
#
#   The fork (utexo-rgb-lib) now wraps the resolver so a stored-Tentative witness the indexer does
#   not know is served from the stash instead of being archived, and adds
#   `revalidate_offchain_bundles` as a repair primitive. That guard makes the call survivable; it
#   does not make it useful. Mercury has no reason to touch witness ords directly: colored ladder
#   rungs are consumed through `color_psbt_and_consume` and validated through the offchain
#   resolver. Anything that reaches for these two APIs is either bypassing that, or is a repair
#   that belongs behind `revalidate_offchain_bundles`.
#
# Usage:
#   scripts/ci/deny-rgb-witness-apis.sh [ROOT]
#
# Exits 0 when clean, 1 when a call site is found (printing every hit), 2 on a usage error.
# ROOT defaults to the repository root; it is a parameter so the guard can be self-tested.

set -uo pipefail

PATTERN='update_witnesses|upsert_witness'
SCAN_DIRS=(clients lib)

ROOT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

if [ ! -d "$ROOT" ]; then
    echo "deny-rgb-witness-apis: no such directory: $ROOT" >&2
    exit 2
fi

scanned=0
hits=""
for dir in "${SCAN_DIRS[@]}"; do
    [ -d "$ROOT/$dir" ] || continue
    scanned=$((scanned + 1))
    found=$(grep -rEnI "$PATTERN" "$ROOT/$dir" \
        --exclude-dir=target \
        --exclude-dir=node_modules \
        --exclude-dir=dist \
        --exclude-dir=build \
        --exclude-dir=.git \
        --exclude-dir=venv \
        --exclude-dir=__pycache__ 2>/dev/null)
    if [ -n "$found" ]; then
        hits="${hits}${found}"$'\n'
    fi
done

if [ "$scanned" -eq 0 ]; then
    echo "deny-rgb-witness-apis: none of (${SCAN_DIRS[*]}) exist under $ROOT" >&2
    exit 2
fi

if [ -n "$hits" ]; then
    echo "deny-rgb-witness-apis: FORBIDDEN rgb-lib witness API used" >&2
    echo "$hits" | sed '/^$/d' >&2
    cat >&2 <<'EOF'

`update_witnesses` / `upsert_witness` must not be called from mercury code.
They operate on RGB witness ords directly and can archive an un-broadcast (Tentative)
colored ladder; the failure is silent (no error, balance unchanged) and, without the fork's
repair primitive, irreversible without an on-chain unwind. See docs/utexo/CTESR-GATE.md 2.3.

If you need to bring an already-archived off-chain branch back, use the fork's
`Wallet::revalidate_offchain_bundles` instead, which never broadcasts anything.
EOF
    exit 1
fi

echo "deny-rgb-witness-apis: OK (no /$PATTERN/ under ${SCAN_DIRS[*]})"
exit 0
