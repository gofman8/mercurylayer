use bitcoin::Network;

/// SDK configuration. Use the per-network constructors and override as needed.
#[derive(Clone, Debug)]
pub struct SdkConfig {
    /// Wallet name — one sqlite wallet record per name in `database_file`.
    pub wallet_name: String,
    /// Mercury statechain entity (SE) base URL.
    pub statechain_entity_url: String,
    /// Electrum server, e.g. `tcp://localhost:50001`.
    pub electrum_url: String,
    /// Electrum server type (`electrs`, `mempool`, ...).
    pub electrum_type: String,
    pub network: Network,
    /// Sqlite file holding wallets, coins and backup transactions.
    pub database_file: String,
    /// Confirmations required before a deposit is spendable.
    pub confirmation_target: u32,
    /// RGB consignment proxy URL (token transfers). None disables token operations.
    pub rgb_proxy_url: Option<String>,
    /// Directory for rgb-lib wallet data. None disables token operations.
    pub rgb_data_dir: Option<String>,
    /// Deposit-token id to use for deposits. None = request one from the SE's token server
    /// (may require payment — surfaced as `SdkError::TokenPaymentRequired`).
    pub deposit_token_id: Option<String>,
    /// Poll interval for the background claim/deposit watcher.
    pub poll_interval_secs: u64,
    /// Automatically re-anchor (refresh) a coin whose backup ladder is nearing its floor before it
    /// is spent, so an aging coin never becomes un-transferable or hands a receiver a coin already
    /// past its exit-race deadline. On by default. The re-anchor fee comes from the coin; the
    /// background watcher also refreshes proactively so the pre-spend hook rarely has to wait for the
    /// re-anchor to confirm. Disable for wallets that manage refresh explicitly.
    pub auto_refresh: bool,
    /// Ladder headroom (blocks below the current backup locktime) at or under which `auto_refresh`
    /// re-anchors a coin. Must exceed the SE `interval` so a whole-coin handover still validates;
    /// well under `initlock` so refresh triggers only late in the horizon. Default 144 (~1 day).
    pub auto_refresh_margin_blocks: u32,
    /// Whether the BACKGROUND watcher also runs a routine ladder-margin refresh (re-anchoring idle
    /// coins with no user action). Default **false**: refresh is folded into `transfer` and paid
    /// on-demand as part of the payment fee (B4 economics), so a running wallet never silently
    /// shrinks a balance in the background. Deadline safety for idle wallets is provided by
    /// `auto_exit` (which force-exits/materializes a coin only when it truly nears its exit-race
    /// deadline). Enable this only if you want proactive background re-anchoring despite the surprise.
    pub background_auto_refresh: bool,
    /// Run the `auto_exit_due` watchtower pass from the background watcher: force-exit plain
    /// off-chain sub-coins / MATERIALIZE token carriers approaching their exit-race deadline, so an
    /// idle owner cannot be clawed back by an ancestor's stale backup. On by default. Both actions
    /// broadcast only the owner's own pre-signed transactions (settling coins on-chain to the
    /// owner); disable for wallets that schedule `auto_exit_due` themselves or delegate to
    /// external watch bundles.
    pub auto_exit: bool,
    /// Deadline margin (blocks) for the background `auto_exit_due` pass. **DERIVED — see
    /// [`auto_exit_margin_blocks_for`], which is what both constructors call. [P0-5]**
    ///
    /// The old literal `288` was `k_max·interval + 144` evaluated on the DEPLOYED 1000/10 profile
    /// with `k_max = 14`, and it was wrong in two ways at once: `interval` is 100 on the mainnet
    /// profile (so 288 covers `k ≤ 1` there, not 14), and the trailing `+ 144` is a budget for ONE
    /// transaction to confirm while a coin whose exit is a TES-R walk must land `3 + 2·depth`
    /// transactions in sequence. Both are now derived per network.
    pub auto_exit_margin_blocks: u32,
    /// **[D31] The owner's fee source for unsticking a tier under the relay floor. `None` = cannot
    /// bump**, which is the default and the honest one: without it, a tier refused for fee reasons
    /// is reported as a stated limit rather than retried forever at the same committed rate.
    ///
    /// Set it and `unilateral_exit` / `defend_ladders` escalate a refused tier to a 1P1C package.
    /// The key here funds FEES ONLY — it is never a coin key, and that separation is what bounds the
    /// exposure of the optional funded-tower variant to the operator's float.
    pub fee_bump: Option<FeeBumpConfig>,
    /// **[CTES-R] Colour the ladder of an RGB carrier.** When on, `claim()` establishes a COLOURED
    /// TES-R ladder over a carrier — `T`, `X_0` and `S_0` each carrying a valid RGB state transition
    /// — instead of leaving it on the flat lane.
    ///
    /// **Default `false`** (commit 2c351c6). The retirement argument below is what makes ON *safe*;
    /// what stopped it from being the default is the measured economics of the lane it switches on
    /// — `docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md`: one coloured partial payment per carrier, ever,
    /// and a 4_284-block unilateral exit for the child it produces. Both constructors below ship
    /// `false`; the doc used to say "true" and the code has said `false` since 2c351c6.
    ///
    /// ## Why it was off, and what changed
    ///
    /// A coloured ladder and the legacy coloured-SPLIT lane are RIVAL spends of the same funding
    /// output `F`, and the ladder's `T` carries **no timelock** while the legacy split is an
    /// absolute-locktime backup that matures ~`initlock` blocks out. A carrier holding both would
    /// let its previous owner broadcast `T` the instant after conveying a split — an immediate,
    /// cost-free clawback of the sats AND the asset, against a receiver who cannot even race it.
    /// While both lanes existed, the only safe configuration was one lane per wallet, so this
    /// defaulted to off.
    ///
    /// The legacy lane is now RETIRED rather than merely interlocked: with this flag on,
    /// `UtexoWallet::refuse_legacy_colored_split_lane` refuses every route into
    /// `create_colored_split_tx` / `create_colored_combine_tx` — the single-carrier transfer, the
    /// multi-carrier combine and the N-recipient batch all reach the coloured in-ladder split
    /// instead, and `tokens::retired_split_lane_census` asserts over the source that no route
    /// escapes the gate. There is no longer a second spend of `F` for `T` to rival, which is what
    /// makes ON the safe default rather than a brave one.
    ///
    /// ## The one exception: the MIGRATION HATCH [B1]
    ///
    /// Retiring the lane outright would STRAND every carrier CTES-R cannot serve. A carrier that
    /// cannot be laddered has no coloured exit to walk, cannot be withdrawn as plain BTC (that burns
    /// the asset) and — with the legacy lane closed — cannot be paid from either: a working coin
    /// becomes an unspendable one, which is worse than the hazard the retirement avoids. The largest
    /// such class is every pre-flip 1_500-sat token piece, sitting above the coloured CHILD floor
    /// (so a split carved it) and below the coloured ROOT floor (so its receiver can never ladder
    /// it) — the trap `tokens::TOKEN_PIECE_SATS` was re-derived to close for NEW pieces.
    ///
    /// So `tokens::migration_hatch_verdict` opens the legacy RGB-aware lane for, and only for,
    /// carriers that hold no ladder AND for which no coloured ladder can be built — proved
    /// read-only, per coin, at the moment of the spend, under the same `wallet_lock` that `claim()`
    /// takes to build one. The retirement gate's premise is a rival trigger `T`; for that class no
    /// `T` exists or can be built while the lock is held, so the premise is absent and the refusal
    /// protects nothing. `UtexoWallet::unilateral_exit` opens for the same class in the same way:
    /// it MATERIALISES the RGB-aware exit branch and never the plain backup. Every carrier keeps at
    /// least one safe way out at all times (sdk78; unit `tokens::migration_hatch_is_narrow`).
    ///
    /// Setting it back to `false` re-enables the legacy lane for that wallet; the per-coin interlock
    /// (`refuse_if_colored_ladder`) still stops any single coin from carrying both.
    pub colored_ladder: bool,
}

/// THERE IS ONE PROTOCOL. Every fresh confirmed root coin is laddered (TES-R) by `claim()` — the
/// `UTEXO_PROTOCOL_DEFAULT` / `deposit_protocol_version` escape hatch that could opt a deposit back
/// into the flat pre-TES-R shape is GONE, and no test pins it any more.
///
/// The B1 theft vector that twice reverted the default — a retained no-timelock trigger `T` voiding a
/// naive split of a laddered coin — is CLOSED by the in-ladder split (PROTOCOL.md §5.4): a split is a
/// STATE tier `SP` spending `X_m.out[0]`, a DESCENDANT of `T` rather than a rival for `F`, so a
/// retained trigger has nothing to race. Attack-proven by sdk58 (11 adversarial cases REJECT) and
/// sdk59 (end-to-end split payment), plus S1/S2 (sdk54/sdk55).
///
/// A RECEIVED non-exact (split-child) payment is FIRST-CLASS: its claim completes the standard SE key
/// handover, so the receiver co-owns `A_child` (invariant across the rotation, which is what keeps the
/// pre-signed exit chain valid) and the sender is locked out. It can be paid onward off-chain — whole
/// via `child_retransfer`, or split via `child_in_ladder_pay` — each hop co-signing a fresh lower-CSV
/// state and disclosing the one it replaces for the receiver's census
/// (`docs/utexo/CHILDREN.md`; sdk60 two hops, sdk17 a partial second hop).
///
/// NOT every coin is laddered, and that is BY DESIGN — it is not a leftover of the old protocol:
///   * an **RGB carrier** must never be laddered *with a PLAIN ladder* (an uncoloured tier spend
///     would destroy the allocation). [CTES-R] `SdkConfig::colored_ladder` (default OFF — 2c351c6)
///     builds it a COLOURED ladder instead — every tier carrying a valid RGB state transition, so
///     laddering MOVES the allocation rather than destroying it. A carrier still falls back to the
///     flat signed-once backup shape when the coloured lane cannot be taken *for that coin*: its
///     allocation is not booked yet, its outpoint holds more than one allocation, or the RGB state
///     could not be read this pass (`LadderSkipReason::RgbCarrier`). Those are retried on the next
///     `claim()`, and while they last that carrier cannot be paid from at all — the legacy split
///     lane it used to fall back to is retired;
///   * a **split sub-coin** whose funding is un-broadcast cannot root a trigger [B0].
/// Those coins travel the UN-LADDERED lane. It is load-bearing for tokens, not dead code left over
/// from the pre-TES-R design.

// =================================================================================================
// [P0-5] THE EXIT MODEL — size, transaction count and LATENCY of a unilateral TES-R exit.
//
// These live here, in a non-test module, because `auto_exit_margin_blocks` is DERIVED from them and
// a default cannot be derived from a `#[cfg(test)]` model. `invalidation_model.rs` pins them against
// the schedule constants; `docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md` §2 is the measured source.
//
// The walk a depth-`d` coin must complete, leaf-ward, and what each rung costs:
//
//   F → T(csv 0) → X_m(ext_csv 0) → [ SP(state_csv 1) → ext(ext_csv 0) ] × d → state(state_csv 0)
//
// `T` carries no timelock; every other rung carries a BIP-68 RELATIVE lock, so the waits are
// SEQUENTIAL and add. `SP` is the only rung with two payload outputs (piece + change), hence the
// only one that costs `TIER_VBYTES + P2TR_OUT_VBYTES`.
// =================================================================================================

/// Expected block cadence — the unit every wall-clock statement in the docs uses, and the
/// confirmation budget the previous `auto_exit_margin_blocks` literal already spent one of.
pub const BLOCKS_PER_DAY: u32 = 144;

/// Transactions a unilateral exit must confirm, **in sequence**, for a coin at in-ladder split
/// depth `d`: `T`, `X_m` and the final state, plus one `SP` and one extension per split level.
pub const fn tesr_exit_txs(child_depth: u32) -> u32 {
    3 + 2 * child_depth
}

/// Signed vsize of that whole walk, derived from the MEASURED tier vsizes
/// (`mercurylib::tesr::TIER_VBYTES` = 125, `P2TR_OUT_VBYTES` = 43): `293·d + 375` vB.
///
/// The model this replaced said `155·d + 112` — a pre-TES-R split-branch shape that understates a
/// depth-1 exit by 1.9× and gets further out with every level.
///
/// **This is the UNCOLOURED walk.** A CTES-R coloured walk has the same shape and the same
/// transaction count — which is all [`auto_exit_margin_blocks_for`] consumes — but every tier
/// carries an `opret`, so it is dearer: `mercuryrustlib::rgb::colored_tier_vbytes` gives 168 / 211
/// per one- / two-payload tier, i.e. 883 vB at depth 1 against 668 here.
/// `invalidation_model::exit_cost_scaling_model` pins both.
pub const fn tesr_exit_vbytes(child_depth: u64) -> u64 {
    use mercurylib::tesr::{P2TR_OUT_VBYTES, TIER_VBYTES};
    // `T` + `X_m` + the final state, all one-payload tiers.
    let spine = 3 * TIER_VBYTES;
    // Per split level: `SP` (two payload outputs) + one extension (one payload output).
    let per_level = (TIER_VBYTES + P2TR_OUT_VBYTES) + TIER_VBYTES;
    spine + child_depth * per_level
}

/// **Exit LATENCY in blocks** — the sum of the walk's relative timelocks: `720·d + 2160` on the
/// mainnet schedule, `12·d + 36` on the regtest one. Derived from the schedule, never a literal.
///
/// The model this replaced reported **zero** wait for every depth. Then it reported `2124·d + 2160`,
/// which was right for split states at `D0 − δ`.
///
/// [CATS] A split state is now a SPINE tier at `SPINE_CSV` = 0, so a level costs only its extension:
/// **720 blocks instead of 2 124**, a 66% cut in the term that compounds with depth. The tail is
/// unchanged — the payee's own `[extension, state]` pair is not a spine and still waits the full
/// `E0 + D0`. Concretely, mainnet: depth 1 falls 4 284 → 2 880 blocks (29.8 → 20 days) and depth 100
/// falls 214 560 → 74 160 blocks (4.08 years → 1.41 years).
pub fn tesr_exit_wait_blocks(p: &mercurylib::tesr::TesrParams, child_depth: u32) -> u32 {
    // `T` has no timelock. `X_m` is a fresh extension (m = 0); the split state `SP` is a spine tier
    // and waits nothing; the child's own ladder is a fresh extension and a fresh state.
    let per_level = u32::from(p.ext_csv(0)) + u32::from(mercuryrustlib::tesr::SPINE_CSV);
    let tail = u32::from(p.ext_csv(0)) + u32::from(p.state_csv(0));
    child_depth * per_level + tail
}

/// Pre-split hops the audit-[17] gap is assumed to span.
///
/// **This is an ASSUMPTION, and it is the weakest term in [`auto_exit_margin_blocks_for`].** The
/// deposit-anchored deadline (`H_deposit + initlock`) is LATE by `k·interval` for a parent
/// transferred `k` times before it was split, and nothing conveys `k` to a receiver — that is
/// audit item [17], still open. The true fail-closed bound is the ladder capacity itself
/// (`initlock / interval` = 100 hops ⟹ a gap of `initlock`), which would exceed the entire deadline
/// horizon and make every off-chain coin due the moment it is claimed. A margin cannot absorb an
/// unbounded `k`; only conveying ancestor locktimes can. 14 is carried over unchanged from the
/// literal this derivation replaced, so that this change moves exactly one dimension.
pub const AUDIT_17_K_MAX: u32 = 14;

/// SE `interval` (`lh_decrement`) of the regtest profile.
///
/// [D8(f)] No longer a literal copied from a config file. `TesrParams::flat_ladder_params` is the
/// single source of truth — the client refuses any coordinator that disagrees with it, and the
/// coordinator refuses to boot if its own env disagrees — so a margin derived from a stale literal
/// here would have been sized for a ladder nobody runs.
pub const SE_INTERVAL_DEPLOYED: u32 = flat_interval_or_panic("regtest");
/// SE `interval` of the mainnet profile (testnet runs the mainnet schedule per D25).
pub const SE_INTERVAL_DEFAULT: u32 = flat_interval_or_panic("bitcoin");

/// `flat_ladder_params(net).1` in a `const` context, so the margin below is derived at compile time
/// rather than transcribed. An unknown network is a compile error, not a silent default.
const fn flat_interval_or_panic(net: &str) -> u32 {
    match mercurylib::tesr::TesrParams::flat_ladder_params_const(net) {
        Some((_, interval)) => interval,
        None => panic!("unknown network: no flat-ladder interval to derive the exit margin from"),
    }
}

/// In-ladder split depth [`auto_exit_margin_blocks_for`] sizes the confirmation budget for.
///
/// The only lane in `auto_exit_due` that walks relative timelocks at all is the COLOURED split
/// child (`wallet.rs`, the `ctesr-` loop), and a coloured child is capped at depth 1 —
/// `tokens::CTESR_CARRIER_SEND_DEPTH`, enforced by `colored_child_txids`. The other two lanes
/// broadcast a locktime-free branch, i.e. one transaction, which this formula covers as `d = 0`
/// would not: `tesr_exit_txs(0) = 3` is still a safe over-estimate for them.
/// `invalidation_model::auto_exit_margin_is_derived_from_the_walk` asserts the two constants agree,
/// so a coloured child-level split appearing later fails loudly here.
pub const AUTO_EXIT_MODELLED_DEPTH: u32 = 1;

/// **The derived `auto_exit_margin_blocks` default. [P0-5]**
///
/// ```text
/// margin = k_max · interval          the audit-[17] gap the deposit-anchored deadline misses
///        + tesr_exit_txs(d) · 144    one confirmation window per SEQUENTIAL tx of the exit walk
/// ```
///
/// The second term is the correction. The literal it replaces spent a single `+ 144` — a budget for
/// ONE transaction — on a walk that lands `3 + 2d` of them one after another, each of which must
/// confirm before the next tier's relative lock even starts counting.
///
/// It deliberately does NOT include the walk's `Σ csv` (`tesr_exit_wait_blocks`). That head start is
/// already taken per-coin, from the coin's own chain, where it belongs: `auto_exit_due` computes
/// `head_start = Σ csv` off the `ctesr-` bundle and subtracts it from the deadline before comparing
/// against this margin. Folding it in here as well would double-count it and force-exit every coin
/// `2124·d + 2160` blocks early — thousands of blocks of off-chain life and an on-chain fee, spent
/// on arithmetic already done.
pub const fn auto_exit_margin_blocks_for(k_max: u32, interval: u32, child_depth: u32) -> u32 {
    k_max * interval + tesr_exit_txs(child_depth) * BLOCKS_PER_DAY
}

/// The owner's fee float, plus how to reach a node that speaks `submitpackage`. [D31]
#[derive(Clone, Debug)]
pub struct FeeBumpConfig {
    pub core_rpc_url: String,
    pub core_rpc_user: String,
    pub core_rpc_password: String,
    /// The funding UTXO, as `txid:vout`.
    pub funding_outpoint: String,
    pub funding_value: u64,
    /// Hex secret key controlling `funding_outpoint`, which must be a P2TR key-spend output.
    /// **A fee key, never a coin key.**
    pub funding_secret_key_hex: String,
    /// Where the child's change goes — normally back to the fee wallet.
    pub change_address: String,
    /// The package feerate to lift a stuck tier to.
    pub target_fee_rate: f64,
}

impl SdkConfig {
    /// Local regtest stack defaults (matches `regtest.Settings.toml` of the repo's test harness).
    pub fn regtest(wallet_name: &str) -> Self {
        SdkConfig {
            wallet_name: wallet_name.to_string(),
            statechain_entity_url: "http://127.0.0.1:8000".to_string(),
            electrum_url: "tcp://localhost:50001".to_string(),
            electrum_type: "electrs".to_string(),
            network: Network::Regtest,
            database_file: "wallet.db".to_string(),
            confirmation_target: 2,
            rgb_proxy_url: Some("rpc://127.0.0.1:3000/json-rpc".to_string()),
            rgb_data_dir: Some(format!("./rgb-data-{}", wallet_name)),
            deposit_token_id: None,
            poll_interval_secs: 5,
            auto_refresh: true,
            auto_refresh_margin_blocks: 144,
            background_auto_refresh: false,
            auto_exit: true,
            // 14·10 + 5·144 = 860 blocks. DERIVED, not chosen — see auto_exit_margin_blocks_for.
            // [D31] Off by default: a wallet cannot bump until an owner gives it a fee source.
            fee_bump: None,
            auto_exit_margin_blocks: auto_exit_margin_blocks_for(
                AUDIT_17_K_MAX,
                SE_INTERVAL_DEPLOYED,
                AUTO_EXIT_MODELLED_DEPTH,
            ),
            colored_ladder: false,
        }
    }

    /// Mainnet skeleton — supply your SE and electrum endpoints.
    pub fn mainnet(wallet_name: &str, statechain_entity_url: &str, electrum_url: &str) -> Self {
        SdkConfig {
            wallet_name: wallet_name.to_string(),
            statechain_entity_url: statechain_entity_url.to_string(),
            electrum_url: electrum_url.to_string(),
            electrum_type: "electrs".to_string(),
            network: Network::Bitcoin,
            database_file: "wallet.db".to_string(),
            confirmation_target: 3,
            rgb_proxy_url: None,
            rgb_data_dir: None,
            deposit_token_id: None,
            poll_interval_secs: 30,
            auto_refresh: true,
            auto_refresh_margin_blocks: 144,
            background_auto_refresh: false,
            auto_exit: true,
            // 14·100 + 5·144 = 2_120 blocks. The mainnet SE `interval` is 100, not the 10 the
            // old shared literal was derived on.
            // [D31] Off by default: a wallet cannot bump until an owner gives it a fee source.
            fee_bump: None,
            auto_exit_margin_blocks: auto_exit_margin_blocks_for(
                AUDIT_17_K_MAX,
                SE_INTERVAL_DEFAULT,
                AUTO_EXIT_MODELLED_DEPTH,
            ),
            colored_ladder: false,
        }
    }
}
