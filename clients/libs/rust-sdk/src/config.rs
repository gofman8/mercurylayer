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
    /// Deadline margin (blocks) for the background `auto_exit_due` pass. Must absorb the audit-[17]
    /// gap (the deposit-anchored deadline is late by `k·interval` for a parent transferred `k`
    /// times pre-split) plus confirmation latency and congestion: choose `≥ k_max·interval + 144`.
    /// Default 288 (~2 days; covers k ≤ 14 pre-split hops on the deployed 1000/10 profile).
    pub auto_exit_margin_blocks: u32,
    /// **[CTES-R] Colour the ladder of an RGB carrier.** When on, `claim()` establishes a COLOURED
    /// TES-R ladder over a carrier — `T`, `X_0` and `S_0` each carrying a valid RGB state transition
    /// — instead of leaving it on the flat lane. Default **true**.
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
///     would destroy the allocation). [CTES-R] `SdkConfig::colored_ladder`, now ON by default,
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
            auto_exit_margin_blocks: 288,
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
            auto_exit_margin_blocks: 288,
            colored_ladder: false,
        }
    }
}
