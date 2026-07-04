#!/usr/bin/env bash
#
# review_logs.sh — digest the traced full-suite logs into anomaly candidates for adversarial review.
#
# Surfaces the places the spec might not cover: errors/panics, non-2xx SE responses, retries,
# repeated identical requests (replay surface), and per-test wall-clock (delay surface). Run after
# `TRACE=1 ./run_all_suites.sh`.
#
# Usage: ./review_logs.sh [LOGDIR]   (default /tmp/spark_suite_logs)
set -u
LOGDIR="${1:-/tmp/spark_suite_logs}"
[ -d "$LOGDIR" ] || { echo "no logdir $LOGDIR"; exit 1; }

sec() { printf '\n===== %s =====\n' "$1"; }

sec "SUMMARY"
cat "$LOGDIR/summary.txt" 2>/dev/null

sec "ERRORS / PANICS / ASSERT FAILURES (test-side)"
grep -hnE "panic|assert|Error:|error\[|thread 'main'|Validation error|Processing error" "$LOGDIR"/*.log 2>/dev/null \
  | grep -viE "warning:|future-incompat|unused|SUCCESS|deliberately|REFUSED|rejecting|refuses|expected" | sort -u | head -60

sec "SE NON-2xx RESPONSES (rocket outcome != Success 2xx)"
grep -hE "rocket::server" "$LOGDIR"/*.log 2>/dev/null \
  | grep -E "Outcome|Matched" | grep -viE "Success\(2" | sort | uniq -c | sort -rn | head -40

sec "SE ENDPOINTS HIT (route frequency — replay/ordering surface)"
grep -hoE "Matched: \([a-z_]+\) (GET|POST) [^ ]+" "$LOGDIR"/*.log 2>/dev/null \
  | sed 's/Matched: //' | sort | uniq -c | sort -rn | head -40

sec "CLIENT HTTP CALLS (reqwest, TRACE=1) — repeated identical = replay candidates"
grep -hoE "reqwest.*(GET|POST) https?://[^ \"]+" "$LOGDIR"/*.log 2>/dev/null \
  | grep -oE "(GET|POST) https?://[^ \"]+" | sort | uniq -c | sort -rn | head -40

sec "RETRY / TIMEOUT / LOCKED / STALE markers"
grep -hnE "retry|retrying|timed out|timeout|locked|still locked|stale|missingorspent|non-final|already|conflict|duplicate" \
  "$LOGDIR"/*.log 2>/dev/null | grep -viE "warning:" | sort -u | head -40

sec "PER-TEST WALL-CLOCK (from summary; delay surface)"
grep -E "\-> " "$LOGDIR/summary.txt" 2>/dev/null

sec "MALFORMED-INPUT HANDLING (400/403/410/Gone/BadRequest observed)"
grep -hnE "400|403|410|Gone|BadRequest|InvalidRequest|does not match|must be|Forbidden" "$LOGDIR"/*.log 2>/dev/null \
  | grep -viE "warning:" | sort -u | head -40

echo
echo "Review questions to drive new tests:"
echo " - Any SE endpoint that acts on client-supplied ids/amounts WITHOUT re-validating server-side?"
echo " - Any message field a malicious peer could reorder/replay/omit (branch_txs, terminal_parents,"
echo "   consignment envelope, batch_id, payment_hash) that isn't independently checked by the receiver?"
echo " - Any await between 'decide' and 'act' where state could change (TOCTOU) — split budget set vs"
echo "   co-sign, latch confirm vs preimage retrieve, deposit detect vs backup create?"
echo " - Any amount/locktime/fee derived from one side that the other side trusts without recomputٍing?"
