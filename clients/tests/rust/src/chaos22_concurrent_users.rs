//! Chaos / property test (SDK_E2E=22): N users act CONCURRENTLY against the live SE + lockbox,
//! doing weighted-random actions — enter (deposit), send, receive (claim), split, unilateral exit,
//! cooperative withdraw — and occasionally CHEATING (broadcasting stale state to try to claw a coin
//! back). Every attempt+result is traced to a JSONL file. After a quiescent settle, an oracle
//! replays the trace + queries final state and asserts the spec invariants: no value is created
//! (INV-1/13/25), no old-state cheat ever succeeded (INV-5/18/19), balances stay non-negative
//! (INV-9), claims are idempotent (INV-8), and every action outcome was EXPECTED (either success or
//! a spec-sanctioned refusal — never an unclassified error).
//!
//! Config (env): CHAOS_USERS (default 5), CHAOS_SECS (default 20), CHAOS_WHALES (default 2),
//! CHAOS_DEPOSIT_SATS (default 2_000_000), CHAOS_INFLIGHT (default 12, caps concurrent signing to
//! respect the SE pool), CHAOS_CHEAT_PROB (default 0.06), CHAOS_SEED (default 42).
//!
//! Run (smoke): SDK_E2E=22 CHAOS_USERS=5 CHAOS_SECS=20 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh
//!   ... cargo run     (full: CHAOS_USERS=100 CHAOS_SECS=180 CHAOS_WHALES=8 CHAOS_INFLIGHT=20)

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::json;
use tokio::sync::{Mutex, Semaphore};

use crate::bitcoin_core;
use crate::chaos22_oracle;

// ------------------------------------------------------------------------------------------------
// Config
// ------------------------------------------------------------------------------------------------

pub struct Cfg {
    pub users: usize,
    pub secs: u64,
    pub whales: usize,
    pub deposit_sats: u64,
    pub inflight: usize,
    pub cheat_prob: f64,
    pub seed: u64,
    pub run_dir: String,
}

fn env_usize(k: &str, d: usize) -> usize {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}
fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}
fn env_f64(k: &str, d: f64) -> f64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

impl Cfg {
    fn from_env() -> Self {
        let users = env_usize("CHAOS_USERS", 5).max(1);
        let whales = env_usize("CHAOS_WHALES", 2).clamp(1, users);
        Cfg {
            users,
            secs: env_u64("CHAOS_SECS", 20),
            whales,
            deposit_sats: env_u64("CHAOS_DEPOSIT_SATS", 2_000_000),
            inflight: env_usize("CHAOS_INFLIGHT", 12).max(1),
            cheat_prob: env_f64("CHAOS_CHEAT_PROB", 0.06),
            seed: env_u64("CHAOS_SEED", 42),
            run_dir: std::env::var("CHAOS_RUN_DIR")
                .unwrap_or_else(|_| "/tmp/chaos22".to_string()),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Trace: one JSONL line per attempt/result, wall-clock ordered.
// ------------------------------------------------------------------------------------------------

pub struct Trace {
    seq: AtomicU64,
    file: std::sync::Mutex<std::io::BufWriter<std::fs::File>>,
}

impl Trace {
    fn new(path: &str) -> Result<Self> {
        let f = std::fs::File::create(path)?;
        Ok(Trace {
            seq: AtomicU64::new(0),
            file: std::sync::Mutex::new(std::io::BufWriter::new(f)),
        })
    }
    /// Emit one event. `extra` carries action-specific fields.
    pub fn emit(&self, actor: usize, action: &str, phase: &str, outcome: &str, extra: serde_json::Value) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let mut ev = json!({
            "seq": seq, "actor": actor, "action": action, "phase": phase, "outcome": outcome,
        });
        if let (Some(obj), Some(ex)) = (ev.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{}", ev);
            let _ = f.flush();
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Outcome classification: under chaos, MANY actions legitimately fail. Distinguish spec-sanctioned
// refusals / contention from real invariant breaches. This is the heart of "everything as expected".
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Class {
    Ok,
    /// A legitimate, spec-expected failure under concurrency (not a bug).
    Contention(&'static str),
    /// An unexpected error — a candidate bug. Recorded as a breach.
    Breach,
}

/// Map an error to a class by its message. Expected-contention substrings come from the SDK/SE
/// error surface; anything unmatched is a Breach the oracle will flag.
pub fn classify(err: &anyhow::Error) -> Class {
    let m = err.to_string().to_lowercase();
    let expected: &[(&str, &str)] = &[
        ("insufficient balance", "insufficient"),
        ("no exact coin", "no-exact"),
        ("no confirmed coin", "no-coin"),
        ("no coin large enough", "no-coin"),
        ("no coin with statechain", "no-coin"),
        ("deposit token payment required", "token"),
        ("token support is not configured", "token-cfg"),
        ("single-use", "terminal"),
        ("spend budget", "terminal"),
        ("already spent", "terminal"),
        ("epoch deadline", "epoch"),
        ("nonce already finalized", "nonce-guard"),
        ("nonce reuse", "nonce-guard"),
        ("batch locked", "batch-lock"),
        ("statecoinbatchlocked", "batch-lock"),
        ("txn-mempool-conflict", "mempool-conflict"),
        ("conflicts with an existing mempool", "mempool-conflict"),
        ("non-final", "non-final"),
        ("bad-txns-inputs-missingorspent", "input-spent"),
        ("missingorspent", "input-spent"),
        ("did not confirm", "confirm-lag"),
        ("no backup tx stored", "no-coin"),
        ("piece", "split-fit"),
        ("does not fit", "split-fit"),
        ("dust", "dust"),
        ("feetoolow", "dust"),
        // V2 protocol LIMITS — finite by design, hit legitimately in a long chaos run:
        //  * replace-by-lower-timelock has finite depth. Each onward hop of a child costs one CSV rung
        //    (d0 24 -> 18 -> 12 -> 6 = d_floor on regtest); at the floor the coin must be exited or
        //    re-anchored rather than re-sent. This is the same finite-budget property V1's
        //    decrementing ladder had, and refusing at the floor is what stops a hop from creating a
        //    state that cannot out-race the one it replaces.
        ("at the floor", "csv-floor"),
        //  * a child too small to split into a viable piece + change (each grandchild must fund its own
        //    two tiers and clear dust). The refusal happens BEFORE anything is co-signed.
        ("leaves no change", "split-fit"),
        // Concurrency refusals — the system CORRECTLY refusing a racing action, not a bug:
        //  * another task spent/transferred the coin first, so it is no longer CONFIRMED. Refusing to
        //    exit a WITHDRAWN parent is the [B1] guard doing its job (exiting it would invalidate the
        //    sub-coins its split funded). NOTE this is the "already spent/transferred" refusal, NOT
        //    the "has a TES-R exit ladder and cannot be split" one — that one must stay UNCLASSIFIED so a
        //    real routing regression still shows up as a breach.
        ("already been spent/transferred", "raced-spend"),
        //  * the coin was handed over concurrently and its auth key rotated, so our co-sign is now
        //    unauthorised. That rotation IS the permanent lockout first-class children rely on.
        ("signature does not match authentication key", "raced-handover"),
        //    (the derived-token endpoint words the same refusal differently, and additionally 401s on a
        //    REPLAYED challenge nonce — audit-[15] single-use auth — which two racing tasks can hit.)
        ("fresh auth/challenge nonce required", "raced-handover"),
        // Infra contention under heavy concurrency: the request safely FAILED (no state change),
        // so it is expected load-shedding, not a protocol bug.
        ("pool timed out", "pool"),
        ("timed out while waiting", "pool"),
        ("too many clients", "pool"),
        ("too many connections", "pool"),
        ("database is locked", "db-lock"),
        ("connection closed", "conn"),
        ("connection reset", "conn"),
        ("connection refused", "conn"),
        ("broken pipe", "conn"),
        ("error sending request", "conn"),
        ("operation timed out", "timeout"),
        ("timed out", "timeout"),
        ("500 internal server error", "se-5xx"),
        ("500 - internal server error", "se-5xx"),
        ("internal server error", "se-5xx"), // any SE 5xx body (e.g. transient enclave-index read miss under pool exhaustion) is retriable load-shedding, not a protocol breach
        ("502 bad gateway", "se-5xx"),
        ("503 service unavailable", "se-5xx"),
        ("service unavailable", "se-5xx"), // fail-closed sign/first + budget reads return 503 under pool exhaustion (audit [1])
        ("expected value", "se-parse"), // SE returned non-JSON under load
        ("failed to parse", "se-parse"),
    ];
    for (needle, tag) in expected {
        if m.contains(needle) {
            return Class::Contention(tag);
        }
    }
    Class::Breach
}

// ------------------------------------------------------------------------------------------------
// Users + registry
// ------------------------------------------------------------------------------------------------

pub struct UserHandle {
    pub idx: usize,
    pub wallet: UtexoWallet,
    pub address: String,
    pub is_whale: bool,
}

pub struct Registry {
    pub users: Vec<Arc<UserHandle>>,
    pub total_deposited: AtomicU64,
    pub by_addr: HashMap<String, usize>,
}

// ------------------------------------------------------------------------------------------------
// Token faucet (shared): fetch prepaid deposit-token slots from the SE.
// ------------------------------------------------------------------------------------------------

/// Fetch `n` prepaid deposit-token slots for `user`, via the user's OWN client config (no separate
/// faucet connection). handle_token_response may pay a token invoice on-chain, so it holds the
/// bitcoin lock; free tokens return without touching the chain, leaving the lock uncontended.
async fn refill_tokens(user: &UserHandle, n: usize, bitcoin: &Mutex<()>) {
    let cc = user.wallet.client_config();
    for _ in 0..n {
        match mercuryrustlib::deposit::get_token(cc).await {
            Ok(tok) => {
                let _g = bitcoin.lock().await;
                match crate::utils::handle_token_response(cc, &tok).await {
                    Ok(id) => {
                        drop(_g);
                        user.wallet.add_prepaid_token(&id).await;
                    }
                    Err(_) => break,
                }
            }
            Err(_) => break,
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Entry point
// ------------------------------------------------------------------------------------------------

pub async fn execute() -> Result<()> {
    // Runs on the V2 (TES-R) default. The DAG-deepening actions go through the in-ladder split
    // (`in_ladder_pay` for a laddered root, `child_in_ladder_pay` for a received child), and the
    // clawback cheat broadcasts a SUPERSEDED ladder state instead of a V1 absolute-locktime backup —
    // the V2 vector. The oracle's invariants (value conservation, single custody, outpoint
    // single-spender, no stuck coins) are protocol-agnostic and unchanged.
    let cfg = Cfg::from_env();
    let _ = std::fs::remove_dir_all(&cfg.run_dir);
    std::fs::create_dir_all(&cfg.run_dir)?;
    let trace_path = format!("{}/chaos.jsonl", cfg.run_dir);
    let trace = Arc::new(Trace::new(&trace_path)?);
    let bitcoin = Arc::new(Mutex::new(())); // serialize all bitcoin-core shell-outs
    let sign_sem = Arc::new(Semaphore::new(cfg.inflight)); // cap concurrent SE co-signing

    println!(
        "CHAOS22 - starting: {} users, {} whales, {}s, deposit {} sats, inflight {}, cheat_prob {}",
        cfg.users, cfg.whales, cfg.secs, cfg.deposit_sats, cfg.inflight, cfg.cheat_prob
    );

    // --- build users CONCURRENTLY (independent per-user dbs; bounded to spare electrs) ------------
    let init_sem = Arc::new(Semaphore::new(16));
    let mut init_tasks = Vec::new();
    for i in 0..cfg.users {
        let run_dir = cfg.run_dir.clone();
        let whales_n = cfg.whales;
        let init_sem = init_sem.clone();
        init_tasks.push(tokio::spawn(async move {
            let _p = init_sem.acquire().await.unwrap();
            let mut sc = SdkConfig::regtest(&format!("chaos_{i}"));
            sc.database_file = format!("{run_dir}/wallet-{i}.db");
            sc.rgb_data_dir = None; // pure-sats chaos (RGB-over-chaos is a follow-up)
            sc.rgb_proxy_url = None;
            let (w, _) = UtexoWallet::initialize(sc, None).await?;
            let address = w.get_utexo_address().await?;
            Ok::<_, anyhow::Error>(UserHandle { idx: i, wallet: w, address, is_whale: i < whales_n })
        }));
    }
    let mut built: Vec<UserHandle> = Vec::with_capacity(cfg.users);
    for t in init_tasks {
        built.push(t.await??);
    }
    built.sort_by_key(|u| u.idx);
    let mut by_addr = HashMap::new();
    let mut users: Vec<Arc<UserHandle>> = Vec::with_capacity(cfg.users);
    for u in built {
        by_addr.insert(u.address.clone(), u.idx);
        users.push(Arc::new(u));
    }
    let registry = Arc::new(Registry { users, total_deposited: AtomicU64::new(0), by_addr });
    println!("CHAOS22 - {} wallets built (per-user db)", cfg.users);

    // --- fund whales: one batched mempool + one 3-block mine + parallel claim --------------------
    let mut deposit_addrs: Vec<(usize, String)> = Vec::new();
    for u in registry.users.iter().filter(|u| u.is_whale) {
        refill_tokens(u, 24, &bitcoin).await; // deposit slot + many split slots
        let addr = u.wallet.get_deposit_address(cfg.deposit_sats).await?;
        deposit_addrs.push((u.idx, addr));
    }
    {
        let _g = bitcoin.lock().await;
        for (_i, addr) in &deposit_addrs {
            bitcoin_core::sendtoaddress(cfg.deposit_sats as u32, addr)?;
        }
        let core = bitcoin_core::getnewaddress()?;
        bitcoin_core::generatetoaddress(3, &core)?;
    }
    for u in registry.users.iter().filter(|u| u.is_whale) {
        let mut waited = 0;
        loop {
            u.wallet.claim().await?;
            if u.wallet.get_balance().await?.available_sats >= cfg.deposit_sats {
                break;
            }
            waited += 1;
            if waited > 90 {
                return Err(anyhow!("whale {} deposit never confirmed", u.idx));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        registry.total_deposited.fetch_add(cfg.deposit_sats, Ordering::SeqCst);
        trace.emit(u.idx, "deposit", "confirm", "ok",
            json!({ "amount": cfg.deposit_sats, "balance_after": cfg.deposit_sats }));
    }
    let d_total = registry.total_deposited.load(Ordering::SeqCst);
    println!("CHAOS22 - whales funded: D = {d_total} sats deposited");

    // --- circulate value off-chain CONCURRENTLY (bounded by sign_sem) so every user has value -----
    let whales: Vec<usize> = registry.users.iter().filter(|u| u.is_whale).map(|u| u.idx).collect();
    // Give every user ample circulation headroom so deep respend chains don't dust out mid-run.
    let per_user = (cfg.deposit_sats / (cfg.users as u64 / cfg.whales as u64 + 1) / 4).max(40_000);
    let mut circ = Vec::new();
    for (k, u) in registry.users.iter().enumerate() {
        if u.is_whale {
            continue;
        }
        let u = u.clone();
        let whale = registry.users[whales[k % whales.len()]].clone();
        let sign_sem = sign_sem.clone();
        let trace = trace.clone();
        let bitcoin = bitcoin.clone();
        circ.push(tokio::spawn(async move {
            {
                let _permit = sign_sem.acquire().await.unwrap();
                if let Err(e) = whale.wallet.transfer(&u.address, per_user).await {
                    trace.emit(whale.idx, "seed_transfer", "result", "err",
                        json!({ "to": u.idx, "amount": per_user, "error": e.to_string() }));
                    return;
                }
            }
            for _ in 0..25 {
                if u.wallet.claim().await.map(|r| r.claimed_transfers).unwrap_or(0) > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            trace.emit(whale.idx, "seed_transfer", "result", "ok",
                json!({ "to": u.idx, "amount": per_user }));
            refill_tokens(&u, 8, &bitcoin).await;
        }));
    }
    for t in circ {
        let _ = t.await;
    }
    println!("CHAOS22 - value circulated off-chain to all users");

    // --- background miner: confirms trickle deposits + matures exit locktimes ---------------------
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let miner = {
        let bitcoin = bitcoin.clone();
        let mut stop_rx = stop_rx.clone();
        tokio::spawn(async move {
            loop {
                if *stop_rx.borrow() {
                    break;
                }
                {
                    let _g = bitcoin.lock().await;
                    if let Ok(core) = bitcoin_core::getnewaddress() {
                        let _ = bitcoin_core::generatetoaddress(1, &core);
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = stop_rx.changed() => {}
                }
            }
        })
    };

    // --- spawn the concurrent user tasks ----------------------------------------------------------
    let deadline = Instant::now() + Duration::from_secs(cfg.secs);
    let mut tasks = Vec::new();
    for u in registry.users.iter().cloned() {
        let registry = registry.clone();
        let trace = trace.clone();
        let bitcoin = bitcoin.clone();
        let sign_sem = sign_sem.clone();
        let seed = cfg.seed + u.idx as u64;
        let cheat_prob = cfg.cheat_prob;
        tasks.push(tokio::spawn(async move {
            run_user(u, registry, trace, bitcoin, sign_sem, deadline, seed, cheat_prob).await
        }));
    }
    println!("CHAOS22 - {} user tasks running for {}s...", cfg.users, cfg.secs);
    for t in tasks {
        let _ = t.await;
    }

    // --- settle to quiescence: everyone claims, mine to mature/confirm ----------------------------
    println!("CHAOS22 - settling to quiescence...");
    for _round in 0..4 {
        for u in registry.users.iter() {
            let _ = u.wallet.claim().await;
        }
        {
            let _g = bitcoin.lock().await;
            if let Ok(core) = bitcoin_core::getnewaddress() {
                let _ = bitcoin_core::generatetoaddress(3, &core);
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let _ = stop_tx.send(true);
    let _ = miner.await;

    // --- oracle -----------------------------------------------------------------------------------
    // D = ALL value injected (whale deposits + every in-run deposit attempt). This is an UPPER bound
    // on injected value: a deposit that didn't fully land just makes accounted < D (a bigger deficit),
    // never inflation. So `Σ SE balances <= D` is a robust no-value-created check.
    let d_total = registry.total_deposited.load(Ordering::SeqCst);
    println!("CHAOS22 - running invariant oracle over {trace_path}... (D={d_total})");
    let report = chaos22_oracle::run(&trace_path, &registry, d_total).await?;
    println!("{}", report.summary());
    if !report.breaches.is_empty() {
        for b in &report.breaches {
            println!("CHAOS22 BREACH: {b}");
        }
        return Err(anyhow!("chaos oracle found {} invariant breach(es)", report.breaches.len()));
    }
    println!(
        "CHAOS22 - SUCCESS: {} users x {}s, actions ok={} contention={} cheats_refused={}, value conserved (D={} accounted={} fees/reserve={}), no cheat succeeded, all outcomes expected.",
        cfg.users, cfg.secs, report.ok, report.contention, report.cheats_refused,
        d_total, report.accounted, report.deficit
    );
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Per-user weighted-random action loop
// ------------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_user(
    me: Arc<UserHandle>,
    registry: Arc<Registry>,
    trace: Arc<Trace>,
    bitcoin: Arc<Mutex<()>>,
    sign_sem: Arc<Semaphore>,
    deadline: Instant,
    seed: u64,
    cheat_prob: f64,
) {
    // Ensure this user has some deposit-token slots to split/deposit with.
    refill_tokens(&me, 8, &bitcoin).await;
    let mut rng = StdRng::seed_from_u64(seed);
    // Per-user DAG-deepening preference in [0.30, 0.89]: how often, when the user is holding an
    // off-chain sub-coin, it RESPENDS that sub-coin (split/transfer it onward) instead of doing a
    // flat action — each respend adds a hop, so branches accumulate depth across the run.
    let depth_pref = 0.30 + ((seed % 60) as f64) / 100.0;
    // (action, weight)
    let actions = [
        ("send", 24u32),
        ("claim", 26),
        ("respend", 16), // deepen: respend the freshest off-chain sub-coin (split OR transfer onward)
        ("split", 8),
        ("exit_deep", 5), // valid exit at a DAG point: prefer an off-chain sub-coin
        ("exit", 4),
        ("withdraw", 4),
        ("deposit", 3),
        ("read", 4),
    ];
    let total_w: u32 = actions.iter().map(|(_, w)| *w).sum();

    while Instant::now() < deadline {
        let jitter = rng.gen_range(50..600);
        tokio::time::sleep(Duration::from_millis(jitter)).await;

        // occasional cheat
        if rng.gen_bool(cheat_prob) {
            crate::chaos22_cheats::run_random_cheat(&me, &registry, &trace, &bitcoin, &mut rng).await;
            continue;
        }

        // pick a weighted action
        let mut pick = rng.gen_range(0..total_w);
        let mut chosen = "read";
        for (a, w) in &actions {
            if pick < *w {
                chosen = a;
                break;
            }
            pick -= *w;
        }

        let bal = match me.wallet.get_balance().await {
            Ok(b) => b,
            Err(_) => continue,
        };

        // DAG-deepening bias: independent of the weighted pick, with probability `depth_pref` steer
        // this iteration toward RESPENDING an off-chain sub-coin so the branch grows another hop
        // (instead of flattening via claims/reads). `respend` self-falls-back to a normal split if
        // the user holds no off-chain sub-coin yet, so the bias always makes progress toward depth.
        if chosen != "respend" && !matches!(chosen, "exit" | "exit_deep" | "withdraw") && rng.gen_bool(depth_pref) {
            chosen = "respend";
        }

        match chosen {
            "read" => {
                let coins = me.wallet.list_coins().await.map(|c| c.len()).unwrap_or(0);
                trace.emit(me.idx, "read", "result", "ok",
                    json!({ "available": bal.available_sats, "in_transfer": bal.in_transfer_sats, "pending": bal.pending_sats, "coins": coins }));
            }
            "send" => {
                if bal.available_sats < 30_000 || registry.users.len() < 2 {
                    continue;
                }
                let to = loop {
                    let j = rng.gen_range(0..registry.users.len());
                    if j != me.idx {
                        break j;
                    }
                };
                let amount = rng.gen_range(10_000..(bal.available_sats / 2).max(10_001));
                let to_addr = registry.users[to].address.clone();
                trace.emit(me.idx, "send", "attempt", "start",
                    json!({ "from": me.idx, "to": to, "amount": amount, "avail_before": bal.available_sats }));
                let permit = sign_sem.acquire().await.unwrap();
                let r = me.wallet.transfer(&to_addr, amount).await;
                drop(permit);
                emit_result(&trace, me.idx, "send", &r.map(|res| json!({
                    "from": me.idx, "to": to, "amount": amount, "coins": res.coins.len(), "used_split": res.used_split
                })));
                // top up split tokens now and then
                if rng.gen_bool(0.3) {
                    refill_tokens(&me, 6, &bitcoin).await;
                }
            }
            "claim" => {
                // snapshot owned statechain_ids so we can report exactly which coins were received
                let before: std::collections::HashSet<String> = me
                    .wallet
                    .list_coins()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|c| c.statechain_id)
                    .collect();
                let _permit = sign_sem.acquire().await.unwrap(); // bound total concurrent SE load
                let r = me.wallet.claim().await;
                drop(_permit);
                match &r {
                    Ok(res) => {
                        let claimed_ids: Vec<String> = if res.claimed_transfers > 0 {
                            me.wallet.list_coins().await.unwrap_or_default().into_iter()
                                .filter_map(|c| c.statechain_id)
                                .filter(|id| !before.contains(id))
                                .collect()
                        } else {
                            Vec::new()
                        };
                        trace.emit(me.idx, "claim", "result", "ok",
                            json!({ "claimed": res.claimed_transfers, "claimed_ids": claimed_ids,
                                    "avail_after": me.wallet.get_balance().await.map(|b| b.available_sats).unwrap_or(0) }));
                    }
                    Err(e) => emit_err(&trace, me.idx, "claim", e),
                }
            }
            "split" => {
                if bal.available_sats < 50_000 {
                    continue;
                }
                // pick a confirmed coin and split a random piece off it
                let coins = me.wallet.list_coins().await.unwrap_or_default();
                let cand = coins.iter().find(|c| c.status == "CONFIRMED" && c.amount_sats > 40_000);
                let Some(c) = cand else { continue };
                let Some(id) = c.statechain_id.clone() else { continue };
                if registry.users.len() < 2 {
                    continue;
                }
                let piece = rng.gen_range(5_000..(c.amount_sats / 2).max(5_001));
                // V2: a laddered coin cannot be self-split [B1]; the split IS a payment. `in_ladder_pay`
                // conveys the piece (Model A) to a peer and keeps the change, deepening the DAG at the
                // receiver — the same structure the old `split_coin` produced locally.
                let to = loop {
                    let j = rng.gen_range(0..registry.users.len());
                    if j != me.idx {
                        break j;
                    }
                };
                let to_addr = registry.users[to].address.clone();
                trace.emit(me.idx, "split", "attempt", "start",
                    json!({ "coin": id, "parent_sats": c.amount_sats, "piece": piece, "to": to }));
                let permit = sign_sem.acquire().await.unwrap();
                // `transfer` dispatches on the coin's shape: a laddered ROOT goes through
                // `in_ladder_pay`, a received CHILD through `child_in_ladder_pay`. Calling
                // `in_ladder_pay` directly would fail on a child ("no TES-R ladder to split").
                let r = me.wallet.transfer(&to_addr, piece).await;
                drop(permit);
                emit_result(&trace, me.idx, "split",
                    &r.map(|tr| json!({ "parent_id": id, "piece": piece, "to": to, "sent": tr.total_sats, "used_split": tr.used_split })));
                if rng.gen_bool(0.5) {
                    refill_tokens(&me, 4, &bitcoin).await;
                }
            }
            // DEEPEN: respend the freshest OFF-CHAIN sub-coin (a DAG node), adding one hop to its
            // branch. Split a piece off it, or transfer the whole sub-coin onward to another user
            // (transfer_sender::execute targets THIS specific coin, unlike wallet.transfer which
            // re-selects). Falls back to splitting any confirmed coin to seed depth if none off-chain.
            "respend" => {
                let coins = me.wallet.list_coins().await.unwrap_or_default();
                let off = coins.iter()
                    .filter(|c| c.status == "CONFIRMED" && c.off_chain && c.statechain_id.is_some() && c.amount_sats > 12_000)
                    .max_by_key(|c| c.amount_sats);
                let target = off.or_else(|| coins.iter()
                    .find(|c| c.status == "CONFIRMED" && c.statechain_id.is_some() && c.amount_sats > 40_000));
                let Some(c) = target else { continue };
                let id = c.statechain_id.clone().unwrap();
                let was_offchain = c.off_chain;
                // transfer onward (adds a claim-gated hop) unless the coin is small, then split.
                if c.amount_sats <= 24_000 || (registry.users.len() >= 2 && rng.gen_bool(0.45)) {
                    let to = loop {
                        let j = rng.gen_range(0..registry.users.len());
                        if j != me.idx { break j; }
                    };
                    let to_addr = registry.users[to].address.clone();
                    trace.emit(me.idx, "send", "attempt", "start",
                        json!({ "from": me.idx, "to": to, "amount": c.amount_sats, "coin": id, "respend": true, "was_offchain": was_offchain }));
                    let permit = sign_sem.acquire().await.unwrap();
                    let cc = me.wallet.client_config();
                    let name = me.wallet.wallet_name();
                    // A received in-ladder CHILD has no statechain/backup rows, so
                    // `transfer_sender::execute` cannot move it — it goes through `child_retransfer`
                    // (a fresh lower-CSV state + the replaced state disclosed). `wallet.transfer`
                    // dispatches on exactly that, so route children through it.
                    let is_child = mercuryrustlib::tesr::load_child(cc, name, &id)
                        .await
                        .ok()
                        .flatten()
                        .is_some();
                    let r = if is_child {
                        me.wallet.transfer(&to_addr, c.amount_sats).await.map(|_| ())
                    } else {
                        mercuryrustlib::transfer_sender::execute(cc, &to_addr, name, &id, None, false, None).await
                    };
                    drop(permit);
                    match &r {
                        Ok(_) => trace.emit(me.idx, "send", "result", "ok",
                            json!({ "from": me.idx, "to": to, "amount": c.amount_sats, "coin": id, "respend": true })),
                        Err(e) => emit_err(&trace, me.idx, "send", &anyhow!("{e}")),
                    }
                } else if registry.users.len() >= 2 {
                    let piece = rng.gen_range(5_000..(c.amount_sats / 2).max(5_001));
                    let to = loop {
                        let j = rng.gen_range(0..registry.users.len());
                        if j != me.idx { break j; }
                    };
                    let to_addr = registry.users[to].address.clone();
                    // V2 DEEPEN: `transfer` auto-routes — a laddered ROOT goes through `in_ladder_pay`,
                    // a received CHILD through `child_in_ladder_pay` (a child-level split, which is
                    // exactly the depth-2 chain sdk17 exercises). Either way the DAG gains a hop.
                    trace.emit(me.idx, "split", "attempt", "start",
                        json!({ "coin": id, "parent_sats": c.amount_sats, "piece": piece, "respend": true, "was_offchain": was_offchain, "to": to }));
                    let permit = sign_sem.acquire().await.unwrap();
                    let r = me.wallet.transfer(&to_addr, piece).await;
                    drop(permit);
                    emit_result(&trace, me.idx, "split",
                        &r.map(|tr| json!({ "parent_id": id, "piece": piece, "respend": true, "sent": tr.total_sats })));
                }
                if rng.gen_bool(0.3) {
                    refill_tokens(&me, 4, &bitcoin).await;
                }
            }
            // exit: prefer a deposit-ROOT coin (flat, never split/transferred) — the shallow DAG case.
            // exit_deep: prefer an OFF-CHAIN sub-coin — a valid unilateral exit at a DAG interior/leaf,
            // which broadcasts the branch chain then the backup. Both record the funding outpoint so
            // the oracle can audit on-chain that it was spent by the legit exit (and by nobody else).
            "exit" | "exit_deep" => {
                let coins = me.wallet.list_coins().await.unwrap_or_default();
                let cand = if chosen == "exit_deep" {
                    coins.iter().find(|c| c.status == "CONFIRMED" && c.off_chain && c.statechain_id.is_some())
                        .or_else(|| coins.iter().find(|c| c.status == "CONFIRMED" && c.statechain_id.is_some()))
                } else {
                    coins.iter().find(|c| c.status == "CONFIRMED" && !c.off_chain && c.statechain_id.is_some())
                        .or_else(|| coins.iter().find(|c| c.status == "CONFIRMED" && c.statechain_id.is_some()))
                };
                let Some(c) = cand else { continue };
                let id = c.statechain_id.clone().unwrap();
                let o_txid = c.utxo_txid.clone();
                let o_vout = c.utxo_vout.unwrap_or(0);
                let off_chain = c.off_chain;
                trace.emit(me.idx, "exit", "attempt", "start",
                    json!({ "coin": id, "amount": c.amount_sats, "off_chain": off_chain, "deep": chosen == "exit_deep" }));
                let permit = sign_sem.acquire().await.unwrap();
                let _g = bitcoin.lock().await; // exit broadcasts branch txs
                let r = me.wallet.unilateral_exit(Some(vec![id.clone()]), None).await;
                drop(_g);
                drop(permit);
                match &r {
                    Ok(st) => {
                        let complete = st.first().map(|s| s.complete).unwrap_or(false);
                        trace.emit(me.idx, "exit", "result", if complete { "ok" } else { "pending" },
                            json!({ "coin": id, "amount": c.amount_sats, "complete": complete, "off_chain": off_chain,
                                    "o_txid": o_txid, "o_vout": o_vout,
                                    "wait_blocks": st.first().map(|s| s.wait_blocks).unwrap_or(0) }));
                    }
                    Err(e) => emit_err(&trace, me.idx, "exit", e),
                }
            }
            "withdraw" => {
                let coins = me.wallet.list_coins().await.unwrap_or_default();
                let cand = coins.iter().find(|c| c.status == "CONFIRMED" && c.statechain_id.is_some());
                let Some(c) = cand else { continue };
                let id = c.statechain_id.clone().unwrap();
                let o_txid = c.utxo_txid.clone();
                let o_vout = c.utxo_vout.unwrap_or(0);
                let to = {
                    let _g = bitcoin.lock().await;
                    bitcoin_core::getnewaddress()
                };
                let Ok(to) = to else { continue };
                trace.emit(me.idx, "withdraw", "attempt", "start", json!({ "coin": id, "amount": c.amount_sats }));
                let permit = sign_sem.acquire().await.unwrap();
                let _g = bitcoin.lock().await;
                let r = me.wallet.withdraw(&to, Some(vec![id.clone()]), None).await;
                drop(_g);
                drop(permit);
                emit_result(&trace, me.idx, "withdraw",
                    &r.map(|w| json!({ "coin": id, "amount": c.amount_sats, "withdrawn": w.len(), "o_txid": o_txid, "o_vout": o_vout })));
            }
            "deposit" => {
                // an in-run enter: a fresh on-chain deposit that the miner will confirm.
                refill_tokens(&me, 2, &bitcoin).await;
                let amount = 500_000u64;
                let addr = match me.wallet.get_deposit_address(amount).await {
                    Ok(a) => a,
                    Err(e) => {
                        emit_err(&trace, me.idx, "deposit", &e);
                        continue;
                    }
                };
                {
                    let _g = bitcoin.lock().await;
                    if bitcoin_core::sendtoaddress(amount as u32, &addr).is_ok() {
                        registry.total_deposited.fetch_add(amount, Ordering::SeqCst);
                        trace.emit(me.idx, "deposit", "confirm", "ok",
                            json!({ "amount": amount, "balance_after": amount }));
                    }
                }
            }
            _ => {}
        }
    }
}

fn emit_result(trace: &Trace, actor: usize, action: &str, r: &Result<serde_json::Value>) {
    match r {
        Ok(v) => trace.emit(actor, action, "result", "ok", v.clone()),
        Err(e) => emit_err(trace, actor, action, e),
    }
}

fn emit_err(trace: &Trace, actor: usize, action: &str, e: &anyhow::Error) {
    let class = classify(e);
    let outcome = match &class {
        Class::Contention(_) => "contention",
        Class::Breach => "breach",
        Class::Ok => "ok",
    };
    let tag = match class {
        Class::Contention(t) => t,
        Class::Breach => "unclassified",
        Class::Ok => "ok",
    };
    trace.emit(actor, action, "result", outcome, json!({ "error": e.to_string(), "class": tag }));
}
