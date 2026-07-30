#!/usr/bin/env bash
#
# CI guard: a `get_backup_txs` read may never be swallowed into a success-shaped default.
#
# THE DEFECT CLASS THIS MAKES REGRESSION-PROOF
# --------------------------------------------
#   Three review rounds in a row found the same shape: a failure that presents as a benign EMPTY or
#   IDLE result. Each round fixed the sites it was shown; the next round found more. The sites were
#   not related by feature, they were related by SPELLING — an error turned into a default that is
#   indistinguishable from a real, calm answer:
#
#     get_backup_txs(...).await.unwrap_or_default()   -> an unreadable DB becomes "no branch
#                                                        witnesses", "no consignment", "not a
#                                                        carrier", "no exit branch to broadcast"
#     match get_backup_txs(...) { Err(_) => Ok(None) } -> an unreadable DB becomes "plain sats,
#                                                        nothing to claim"
#     get_backup_txs(...).map(..).unwrap_or(0)         -> an unreadable DB becomes spend generation
#                                                        0, which re-derives a used RGB seal
#
#   `get_backup_txs` is the single most dangerous read in the wallet to swallow, because everything
#   that protects a coin lives behind it: the exit branch, the backup transactions, the consignment
#   envelope, the carrier's spend generation, the rejection marker. Every one of those, when read as
#   EMPTY, means "there is nothing here to protect" — which is exactly what the code then does.
#
#   The read is also genuinely ambiguous, which is why it keeps trapping people:
#   `sqlite_manager::get_backup_txs` uses `fetch_one`, so "this key has no row" arrives as an `Err`
#   just like "the database could not be read". Collapsing both with `unwrap_or_default()` is the
#   easy way to handle the first and it silently swallows the second. The fix is not to propagate
#   everything — a missing branch row is legitimate — it is to SEPARATE the two, which is what
#   `tokens.rs::read_backup_rows` does (`Ok(None)` = genuinely absent, `Err` = could not read).
#
#   So: reviewing for this is not converging, and the direction of a swallow cannot be judged from a
#   grep. What CAN be judged mechanically is whether a `get_backup_txs` result was defaulted at all.
#   Route it through a helper that names the distinction, or handle the two cases explicitly.
#
# SCOPE — deliberately an explicit file list, not the whole tree
# -------------------------------------------------------------
#   Enforced only over the files whose lanes have been converted. The guard is a RATCHET: as each
#   remaining lane is fixed, add its file here and the property is locked in for good. A tree-wide
#   pattern would either fail on lanes that are still mid-conversion (a red guard teaches people to
#   ignore guards) or need an allowlist of line numbers that rots on the next edit.
#
# Usage:
#   scripts/ci/deny-swallowed-backup-reads.sh [ROOT]
#
# Exits 0 when clean, 1 when a swallowed read is found (printing every hit), 2 on a usage error.
# ROOT defaults to the repository root; it is a parameter so the guard can be self-tested.

set -uo pipefail

# The read whose failure must never be defaulted away.
READ_FN='get_backup_txs'

# Files under enforcement. ADD A LINE when a lane is converted; never remove one.
ENFORCED_FILES=(
    clients/libs/rust-sdk/src/tokens.rs
)

ROOT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

if [ ! -d "$ROOT" ]; then
    echo "deny-swallowed-backup-reads: no such directory: $ROOT" >&2
    exit 2
fi

# Report a swallow spelling that appears anywhere in the statement started by a `get_backup_txs`
# call. The statement is followed until its terminating `;` (or a blank line / closing brace, so a
# malformed chunk can never run away and scan the rest of the file).
scan_file() {
    awk -v fn="$READ_FN" '
        # Swallow spellings, applied only INSIDE a get_backup_txs statement.
        function swallows(line) {
            return (line ~ /unwrap_or_default[[:space:]]*\(/) ||
                   (line ~ /unwrap_or[[:space:]]*\(/)         ||
                   (line ~ /unwrap_or_else[[:space:]]*\(/)    ||
                   (line ~ /\.ok\(\)/)                        ||
                   (line ~ /Err\(_\)[[:space:]]*=>/)          ||
                   (line ~ /is_err[[:space:]]*\(\)/)
        }
        {
            stripped = $0
            sub(/\/\/.*$/, "", stripped)   # a mention in a comment is documentation, not a call

            if (!inside) {
                if (stripped ~ fn "[[:space:]]*\\(") { inside = 1; start = NR; span = 0; buf = "" }
                else next
            }

            span++
            buf = buf "\n  " NR ": " $0

            if (swallows(stripped)) {
                printf "%s:%d: %s result swallowed (statement starting at line %d)%s\n",
                       FILENAME, NR, fn, start, buf
                inside = 0
                next
            }

            # End of statement, or a runaway window.
            if (stripped ~ /;[[:space:]]*$/ || $0 ~ /^[[:space:]]*$/ || span > 25) { inside = 0 }
        }
    ' "$1"
}

scanned=0
hits=""
for rel in "${ENFORCED_FILES[@]}"; do
    path="$ROOT/$rel"
    [ -f "$path" ] || continue
    scanned=$((scanned + 1))
    found=$(scan_file "$path")
    if [ -n "$found" ]; then
        hits="${hits}${found}"$'\n'
    fi
done

if [ "$scanned" -eq 0 ]; then
    echo "deny-swallowed-backup-reads: none of the enforced files exist under $ROOT" >&2
    echo "  expected at least one of: ${ENFORCED_FILES[*]}" >&2
    exit 2
fi

if [ -n "$hits" ]; then
    echo "deny-swallowed-backup-reads: a $READ_FN failure is being turned into a default" >&2
    echo "$hits" | sed '/^$/d' >&2
    cat >&2 <<'EOF'

A `get_backup_txs` error must not become an empty/zero/None answer. That read backs the exit
branch, the backup transactions, the consignment envelope, the carrier's spend generation and the
rejection marker: read as EMPTY, each one means "there is nothing here to protect", and the caller
then declines to protect it. An unreadable database is not evidence that a coin is safe.

Use `tokens.rs::read_backup_rows`, which separates the two things this API conflates:
  Ok(None) -> the row genuinely does not exist (a real answer: e.g. a coin with an on-chain
              witness legitimately has no `branch-<id>` row)
  Err(_)   -> the read failed and you learned NOTHING; propagate it and fail closed.
EOF
    exit 1
fi

echo "deny-swallowed-backup-reads: OK (no swallowed $READ_FN reads in ${#ENFORCED_FILES[@]} enforced file(s))"
exit 0
