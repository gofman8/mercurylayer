#!/usr/bin/env bash
#
# CI guard: the SILENT-DEGRADATION class — a failure that presents as a benign empty/idle result.
#
# ---------------------------------------------------------------------------------------------
# Why this exists
# ---------------------------------------------------------------------------------------------
#
# Three consecutive review rounds each found the same defect SHAPE in new places, and per-site
# fixing did not converge (each round fixed what it was shown; the next round found ~8 more):
#
#   * an empty carrier set read as "this wallet has no RGB carriers to protect" when the real
#     answer was "the RGB engine did not answer" (F3 — the near-deadline watcher then materialised
#     nothing and reported success while protecting nothing);
#   * an empty UTXO vector read as "that outpoint is spent" when the real answer was "the indexer
#     is down";
#   * an absent deadline read as "nothing is due" when the real answer was "I could not compute
#     one" — so `auto_exit_due` skipped the coin and a received RGB token stayed exposed to its
#     sender's clawback;
#   * an empty branch-witness set read as "this coin has no exit branch" when the real answer was
#     "the backup-tx read failed" — i.e. the code declined to broadcast the very thing that saves
#     the coin.
#
# Every one of those is one line of Rust that turns an `Err` into a default value. The value is
# always the SAFE-LOOKING one, so nothing downstream can tell the difference, and on this codebase
# "nothing to do" is the answer that loses money: coins here are protected by racing a deadline.
#
# ---------------------------------------------------------------------------------------------
# What the guard actually asserts
# ---------------------------------------------------------------------------------------------
#
# THE DIRECTION MATTERS, NOT THE SPELLING. A swallow that fails toward MORE work is safe; one that
# fails toward LESS protection is the bug. grep cannot tell those apart — so the guard does not try
# to. It pins the CURRENT, REVIEWED set of swallow sites and fails on any that is not on the
# allowlist. Adding one is then a deliberate act with a written reason, reviewed when it is written
# rather than three rounds later.
#
#   * a NEW occurrence (a site not in the allowlist)                       -> FAIL
#   * a site whose count grew (a second identical swallow in one file)     -> FAIL
#   * a site that DISAPPEARED (someone fixed it)                           -> pass, reported as info
#
# Keying on the normalised source LINE rather than the line number is deliberate: fixes elsewhere in
# a file must not churn the allowlist, and two sessions editing the same file must not collide.
#
# ---------------------------------------------------------------------------------------------
# Coverage and its limits (read this before trusting it)
# ---------------------------------------------------------------------------------------------
#
# Covered spellings, all high-signal for a swallowed `Result` (see PATTERNS below):
#   `.await.unwrap_or_default()`, `.await.unwrap_or(`, `.await.ok()`, `.ok()?`,
#   `Err(_) => continue`, `Err(_) => return Ok`, an `Err(_)` arm yielding an empty/default value or
#   an empty block, and `.is_err()` used as a classifier.
#
# Deliberately NOT covered, and why:
#   * a bare `.unwrap_or_default()` / `.unwrap_or(x)` with no `.await` in front — overwhelmingly
#     `Option`, not `Result` (`coin.amount.unwrap_or_default()`), and grep has no type information.
#     The `.await`-prefixed forms are the ones that swallow an I/O / DB / RGB error.
#     A MULTI-LINE chain (`.await` on one line, `.unwrap_or(false)` three lines down) is NOT exempt:
#     the `chain-after-await` pass below walks back up to 6 lines within the same statement and
#     flags a trailing swallow whose chain contains `.await` / `map_err(`. That form is not a
#     hypothetical — one of the round-3 HIGHs (`consignment_bearing_outpoints`, the carrier
#     enumeration that every carrier check downstream depends on) was written exactly that way and
#     a single-line grep would have missed it.
#   * a DROPPED `Result` (no `?`, no `let _ =`) — not a grep-detectable spelling. rustc already
#     catches it: `unused_must_use` is on by default and is exactly how the dropped
#     `update_unlock_transfer` in `server/src/endpoints/lightning_latch.rs` was found. Keep
#     `cargo check --workspace` free of `unused_must_use` and this spelling stays covered.
#     As of this writing exactly four remain, all the same legacy line — `conf_rs.merge(File::
#     with_name(..).required(false))` in `server/src/server_config.rs` and `token-server/src/
#     server_config.rs`. They are the only ones, so any NEW `unused_must_use` in a `cargo check
#     --workspace` run is a fresh dropped `Result` and should be treated as a hit on this guard.
#   * `let _ = expr;` — the deliberate, reviewed form of the same drop. Too common (event sends) to
#     be worth pinning; it is at least visible to a reader, which is the whole point.
#
# Lines that are pure comments, and lines containing `assert` (a test assertion or `debug_assert` is
# a CHECK, not a swallow — and it fails toward strictness), are skipped. So are Rust STRING LITERALS
# that merely quote a swallow spelling (a line carrying an escaped `\n`, or a bare quoted token) —
# the in-file guard in `clients/libs/rust-sdk/src/wallet.rs` keeps its planted-regression fixtures
# as string literals and must not trip this one.
#
# INLINE ESCAPE HATCH: a line carrying `AUDITED-SWALLOW` (on it, or within the 6 lines above it) is
# skipped. That is deliberately the SAME marker the `silent_degradation_guard` module in
# `clients/libs/rust-sdk/src/wallet.rs` uses, so there is one convention and not two: annotate in
# place with the direction argument, and both guards accept it. The allowlist here exists for the
# sites that predate the convention, and for files where an inline comment is not wanted.
#
# ---------------------------------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------------------------------
#   scripts/ci/deny-silent-degradation.sh [ROOT]
#
#   ROOT defaults to the repository root; it is a parameter so the guard can be self-tested against
#   a throwaway tree. The allowlist is $ROOT/scripts/ci/silent-degradation-allowlist.txt, or
#   $SD_ALLOWLIST if set; a missing allowlist means "allow nothing" (so a planted violation in a
#   throwaway tree is caught).
#
# Exits 0 when clean, 1 when an un-allowlisted swallow is found, 2 on a usage error.

set -uo pipefail

SCAN_DIRS=(clients/libs lib server/src)

# Each entry is  ID<TAB>ERE.  Keep them high-signal: a noisy guard gets allowlisted into
# uselessness, which is the failure mode this replaces.
PATTERNS=(
    'await-unwrap_or_default	\.await[[:space:]]*\.unwrap_or_default\(\)'
    'await-unwrap_or	\.await[[:space:]]*\.unwrap_or\('
    'await-ok	\.await[[:space:]]*\.ok\(\)'
    'ok-question	\.ok\(\)\?'
    'err-continue	Err\([[:space:]]*_[A-Za-z0-9_]*[[:space:]]*\)[[:space:]]*=>[[:space:]]*continue'
    'err-return-ok	Err\([[:space:]]*_[A-Za-z0-9_]*[[:space:]]*\)[[:space:]]*=>[[:space:]]*return[[:space:]]+Ok'
    'err-empty	Err\([[:space:]]*_[A-Za-z0-9_]*[[:space:]]*\)[[:space:]]*=>[[:space:]]*(Vec::new|vec!\[\]|Default::default|None|Some\(\)|HashSet::new|HashMap::new|BTreeSet::new|BTreeMap::new|String::new|0|false|true|\{[[:space:]]*\})'
    'is_err-classifier	\.is_err\(\)'
)

ROOT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

if [ ! -d "$ROOT" ]; then
    echo "deny-silent-degradation: no such directory: $ROOT" >&2
    exit 2
fi
ROOT="$(cd "$ROOT" && pwd)"

ALLOWLIST="${SD_ALLOWLIST:-$ROOT/scripts/ci/silent-degradation-allowlist.txt}"

scanned=0
for dir in "${SCAN_DIRS[@]}"; do
    [ -d "$ROOT/$dir" ] && scanned=$((scanned + 1))
done
if [ "$scanned" -eq 0 ]; then
    echo "deny-silent-degradation: none of (${SCAN_DIRS[*]}) exist under $ROOT" >&2
    exit 2
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

MARKER='AUDITED-SWALLOW'
MARKER_LOOKBACK=6

# `record FILE LINENO TEXT ID` — normalise, drop the non-swallows, append to $tmp/hits.
record() {
    _file="$1"; _no="$2"; _text="$3"; _id="$4"
    _norm="$(printf '%s' "$_text" | tr '\t' ' ' | sed -e 's/^ *//' -e 's/ *$//' -e 's/  */ /g')"
    case "$_norm" in
        # prose about the class is not the class
        '//'*|'/*'*|'*'*) return 0 ;;
        # a test assertion / debug_assert is a CHECK, and it fails toward strictness
        *assert*) return 0 ;;
        # a Rust string literal that merely QUOTES a swallow spelling (guard fixtures do this)
        *'\n'*) return 0 ;;
        '"'*'"'|'"'*'",') return 0 ;;
    esac
    # inline escape hatch, same marker as the in-file guard in rust-sdk/src/wallet.rs
    _from=$((_no - MARKER_LOOKBACK)); [ "$_from" -lt 1 ] && _from=1
    if sed -n "${_from},${_no}p" "$_file" 2>/dev/null | grep -q "$MARKER"; then
        return 0
    fi
    printf '%s\t%s\t%s\n' "${_file#"$ROOT"/}" "$_id" "$_norm" >> "$tmp/hits"
}

# ---- collect: one "PATH<TAB>ID<TAB>NORMALISED_LINE" per hit ------------------------------------
: > "$tmp/hits"
for entry in "${PATTERNS[@]}"; do
    id="${entry%%	*}"
    re="${entry#*	}"
    for dir in "${SCAN_DIRS[@]}"; do
        [ -d "$ROOT/$dir" ] || continue
        grep -rEnI "$re" "$ROOT/$dir" \
            --include='*.rs' \
            --exclude-dir=target \
            --exclude-dir=node_modules \
            --exclude-dir=dist \
            --exclude-dir=build \
            --exclude-dir=.git \
            --exclude-dir=venv \
            --exclude-dir=__pycache__ 2>/dev/null \
        | while IFS= read -r hit; do
            file="${hit%%:*}"
            rest="${hit#*:}"
            lineno="${rest%%:*}"
            line="${rest#*:}"
            record "$file" "$lineno" "$line" "$id"
        done
    done
done

# ---- collect (pass 2): the MULTI-LINE chain form -----------------------------------------------
# `x().await` / `.map_err(..)` on one line and the swallow several lines further down the same
# method chain. Looks back at most 6 lines and stops at a statement boundary (`;`), so it only ever
# associates a trailing `.unwrap_or*` / `.ok()` with ITS OWN chain.
: > "$tmp/files"
for dir in "${SCAN_DIRS[@]}"; do
    [ -d "$ROOT/$dir" ] || continue
    find "$ROOT/$dir" -name '*.rs' -type f \
        -not -path '*/target/*' -not -path '*/node_modules/*' \
        -not -path '*/dist/*' -not -path '*/build/*' >> "$tmp/files"
done
if [ -s "$tmp/files" ]; then
    # shellcheck disable=SC2016
    tr '\n' '\0' < "$tmp/files" | xargs -0 awk '
        FNR == 1 { delete buf }
        /^[[:space:]]*\.(unwrap_or|unwrap_or_default|unwrap_or_else|ok)\(/ {
            for (i = 1; i <= 6 && FNR - i > 0; i++) {
                prev = buf[(FNR - i) % 8]
                if (prev ~ /\.await/ || prev ~ /map_err\(/) { print FILENAME "\t" FNR "\t" $0; break }
                if (prev ~ /;[[:space:]]*$/) break
            }
        }
        { buf[FNR % 8] = $0 }
    ' 2>/dev/null | while IFS=$'\t' read -r file lineno line; do
        record "$file" "$lineno" "$line" 'chain-after-await'
    done
fi

sort "$tmp/hits" | uniq -c | sed -e 's/^ *//' -e 's/ /\t/' > "$tmp/observed"

# `SD_DUMP=1` prints the observed table in allowlist format, so refreshing the allowlist after a
# round of fixes is mechanical (the REASON comments still have to be written by a human).
if [ "${SD_DUMP:-0}" = "1" ]; then
    cat "$tmp/observed"
    exit 0
fi

# ---- load the allowlist ------------------------------------------------------------------------
: > "$tmp/allowed"
if [ -f "$ALLOWLIST" ]; then
    grep -v '^[[:space:]]*#' "$ALLOWLIST" | grep -v '^[[:space:]]*$' > "$tmp/allowed"
fi

# ---- compare -----------------------------------------------------------------------------------
: > "$tmp/violations"
: > "$tmp/stale"

while IFS=$'\t' read -r count path id line; do
    [ -n "${path:-}" ] || continue
    allowed_count="$(awk -F'\t' -v p="$path" -v i="$id" -v l="$line" \
        '$2==p && $3==i && $4==l { print $1; found=1 } END { if (!found) print "" }' "$tmp/allowed" | head -1)"
    if [ -z "$allowed_count" ]; then
        printf 'NEW      %s  [%s]  %s\n' "$path" "$id" "$line" >> "$tmp/violations"
    elif [ "$count" -gt "$allowed_count" ]; then
        printf 'GREW     %s  [%s]  %s   (allowed %s, found %s)\n' \
            "$path" "$id" "$line" "$allowed_count" "$count" >> "$tmp/violations"
    fi
done < "$tmp/observed"

# stale entries are INFORMATIONAL only: a fix that removes a swallow must never turn the guard red.
while IFS=$'\t' read -r count path id line; do
    [ -n "${path:-}" ] || continue
    if ! awk -F'\t' -v p="$path" -v i="$id" -v l="$line" \
        '$2==p && $3==i && $4==l { found=1 } END { exit !found }' "$tmp/observed"; then
        printf 'fixed    %s  [%s]  %s\n' "$path" "$id" "$line" >> "$tmp/stale"
    fi
done < "$tmp/allowed"

if [ -s "$tmp/violations" ]; then
    echo "deny-silent-degradation: UN-ALLOWLISTED SWALLOW (silent-degradation class)" >&2
    cat "$tmp/violations" >&2
    cat >&2 <<'EOF'

You wrote a line that can turn a FAILURE into a benign-looking empty / default / idle value.

On this codebase that is not a style question. Coins are protected by racing a deadline, so
"there is nothing to do" is the answer that loses money, and it is exactly the answer an
`unwrap_or_default()` on a failed lookup produces. Three review rounds in a row found this same
shape in new places; that is why it is a red test and not a review convention.

Ask the ONE question that decides it:

    if this expression fails, does the code do MORE work or LESS PROTECTION?

  * MORE work  (re-attempting an idempotent booking, quarantining a coin, attempting a broadcast,
    treating an unknown as unsafe) -> fine. Add it to
    scripts/ci/silent-degradation-allowlist.txt WITH the reason, as a `#` comment above the entry.
  * LESS protection (skipping a coin, reporting an empty carrier/witness/deadline set, un-quarantining,
    declining to broadcast, answering "Success" for work that did not happen) -> this is the bug.
    Fail CLOSED and LOUD instead: propagate the error, or route it into the WatchtowerBlind /
    retained-fault machinery (`UtexoWallet::note_watchtower_blind`, `WalletEvent::WatchtowerBlind`,
    `UtexoWallet::watchtower_faults`) so "I could not see" is distinguishable from "nothing is due".

Allowlist line format (TAB-separated), with the reason in a comment directly above it:

    # <why this one is safe: which direction it fails toward>
    <count><TAB><path from repo root><TAB><pattern-id><TAB><the normalised source line>
EOF
    exit 1
fi

n_allowed="$(wc -l < "$tmp/allowed" | tr -d ' ')"
n_obs="$(wc -l < "$tmp/observed" | tr -d ' ')"
if [ -s "$tmp/stale" ]; then
    echo "deny-silent-degradation: these allowlisted swallows are GONE — please prune the allowlist:"
    cat "$tmp/stale"
fi
echo "deny-silent-degradation: OK ($n_obs distinct swallow sites, all of $n_allowed allowlisted, under ${SCAN_DIRS[*]})"
exit 0
