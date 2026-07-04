#!/usr/bin/env bash
#
# run_all_suites.sh — launch the full Spark-parity test matrix with trace logging.
#
# Runs: unit tests (mercury-spark-sdk), every SDK_E2E=N and RGB_E2E=N flow discovered in the
# test dispatch, the RLN Lightning smoke (LN_SMOKE=1), and the upstream Mercury suite. Captures
# per-test stdout/stderr plus time-sliced docker logs (mercury-server, lockbox, electrs) so the
# run can be reviewed for anomalies (delays, retries, malformed/late/replayed messages).
#
# Prereqs: regtest stack (rgb-lightning-node/regtest.sh start) + Mercury lockbox stack
#          (docker compose -f docker-compose-lockbox.yml up -d) + the RLN binary built
#          (cd rgb-lightning-node && git submodule update --init && cargo build).
#
# Usage:
#   ./run_all_suites.sh                 # everything
#   ONLY="SDK_E2E=1 SDK_E2E=8" ./run_all_suites.sh   # a subset
#   TRACE=1 ./run_all_suites.sh         # RUST_LOG=info,mercury_spark_sdk=debug for the client
#   SKIP_LN=1 ./run_all_suites.sh       # skip the slow RLN-backed flows (5,6 + LN smoke)
set -u

REPO="/Users/gofman/Claude/mercurylayer"
TESTS="$REPO/clients/tests/rust"
LOGDIR="${LOGDIR:-/tmp/spark_suite_logs}"
SUMMARY="$LOGDIR/summary.txt"

source "$HOME/.cargo/env" 2>/dev/null || true
export PATH="$HOME/bin:$HOME/.local/bin:$PATH"
export DOCKER_HOST="${DOCKER_HOST:-unix:///Users/gofman/.colima/default/docker.sock}"
export COMPOSE_FILE="${COMPOSE_FILE:-/Users/gofman/Claude/rgb-lightning-node/compose.yaml}"
export COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-rgb-lightning-node}"
export ML_NETWORK=regtest
export RLN_REGTEST="${RLN_REGTEST:-/Users/gofman/Claude/rgb-lightning-node/regtest.sh}"
export RLN_BITCOIND_CONTAINER="${RLN_BITCOIND_CONTAINER:-rgb-lightning-node-bitcoind-1}"

if [ "${TRACE:-0}" = "1" ]; then
  export RUST_LOG="${RUST_LOG:-info,mercury_spark_sdk=debug,mercuryrustlib=debug,reqwest=info}"
fi

mkdir -p "$LOGDIR"
: > "$SUMMARY"
echo "run_all_suites @ $(date -u +%Y-%m-%dT%H:%M:%SZ)  LOGDIR=$LOGDIR  TRACE=${TRACE:-0}" | tee -a "$SUMMARY"

# electrs sometimes exits when it loses bitcoind; nudge it.
nc -z 127.0.0.1 50001 2>/dev/null || docker start rgb-lightning-node-electrs-1 >/dev/null 2>&1 || true

# --- discover which SDK_E2E / RGB_E2E indices the dispatch handles ---
discover() {
  grep -oE "std::result::Result::Ok\(\"$1[0-9]*\"\)" "$TESTS/src/main.rs" 2>/dev/null \
    | grep -oE '"[0-9]+"' | tr -d '"' | sort -n | uniq
}
SDK_NS=$(discover "")   # placeholder, replaced below
SDK_NS=$(grep -oE 'SDK_E2E".*Ok\("[0-9]+"\)' "$TESTS/src/main.rs" | grep -oE '"[0-9]+"' | tr -d '"' | sort -n | uniq)
RGB_NS=$(grep -oE 'RGB_E2E".*Ok\("[0-9]+"\)' "$TESTS/src/main.rs" | grep -oE '"[0-9]+"' | tr -d '"' | sort -n | uniq)

snapshot_docker() {
  local log="$1" since="$2"
  for c in mercurylayer-mercury-server-1 mercurylayer-lockbox-1 rgb-lightning-node-electrs-1; do
    echo "===== docker logs $c (since ${since}s) =====" >> "$log"
    docker logs --since "${since}s" "$c" >> "$log" 2>&1 || true
  done
}

# run <label> <env-assignment...>
run() {
  local label="$1"; shift
  if [ -n "${ONLY:-}" ] && ! grep -qw -- "$label" <<<"$ONLY"; then return; fi
  local log="$LOGDIR/${label//=/_}.log"
  local start=$(date +%s)
  ( cd "$TESTS" && env "$@" cargo +stable run > "$log" 2>&1 )
  local rc=$?
  local elapsed=$(( $(date +%s) - start ))
  snapshot_docker "$log" "$((elapsed + 5))"
  local ok="FAIL(rc=$rc)"
  if [ $rc -eq 0 ] && grep -qE "SUCCESS|completed successfully|Result as reported" "$log"; then ok="PASS"; fi
  printf "%-14s -> %-14s (%ss)\n" "$label" "$ok" "$elapsed" | tee -a "$SUMMARY"
}

# --- unit tests first (fast, no stack) ---
if [ -z "${ONLY:-}" ] || grep -qw "UNIT" <<<"${ONLY:-}"; then
  echo "== unit: mercury-spark-sdk ==" | tee -a "$SUMMARY"
  ( cd "$REPO" && cargo +stable test -p mercury-spark-sdk > "$LOGDIR/UNIT.log" 2>&1 )
  if grep -qE "test result: ok" "$LOGDIR/UNIT.log"; then
    echo "UNIT           -> PASS" | tee -a "$SUMMARY"
  else
    echo "UNIT           -> FAIL" | tee -a "$SUMMARY"
  fi
fi

# --- SDK flows ---
for n in $SDK_NS; do
  if [ "${SKIP_LN:-0}" = "1" ] && { [ "$n" = "5" ] || [ "$n" = "6" ]; }; then
    echo "SDK_E2E=$n     -> SKIPPED (SKIP_LN)" | tee -a "$SUMMARY"; continue
  fi
  run "SDK_E2E=$n" "SDK_E2E=$n"
done

# --- RGB DAG primitives ---
for n in $RGB_NS; do run "RGB_E2E=$n" "RGB_E2E=$n"; done

# --- Lightning harness smoke ---
if [ "${SKIP_LN:-0}" != "1" ]; then run "LN_SMOKE=1" "LN_SMOKE=1"; fi

# --- upstream Mercury suite (no SDK/RGB env -> default path) ---
run "UPSTREAM" "NOOP=1"

echo "=== ALL SUITES DONE $(date -u +%H:%M:%SZ) ===" | tee -a "$SUMMARY"
echo "logs in $LOGDIR" | tee -a "$SUMMARY"
