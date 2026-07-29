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
    /// Protocol version for NEW deposits (V2DEF-2). `1` = V1 (the current default during migration:
    /// a deposit is byte-identical to today, so V1 sig-count tests are unaffected). `2` = TES-R
    /// native: `claim()` auto-establishes + persists a tier ladder for each fresh confirmed coin, so
    /// it transfers via the R′ path. Seeded from env `UTEXO_PROTOCOL_DEFAULT` if set.
    pub deposit_protocol_version: u32,
}

/// Default protocol version for new deposits, from env `UTEXO_PROTOCOL_DEFAULT`.
///
/// **DEFAULT = `2` (TES-R / Utexo V2).** The B1 theft vector that reverted this to `1` twice — a retained
/// no-timelock trigger `T` voiding a naive split of a laddered coin — is CLOSED by the **in-ladder split**
/// (V2-DESIGN §5.4): a split is now a STATE tier `SP` spending `X_m.out[0]`, a DESCENDANT of `T` rather
/// than a rival for `F`, so a retained trigger has nothing to race. Landed + attack-proven:
///   - sdk58: `verify_child_bundle` accepts a real split child; 9 adversarial cases REJECT (decoy
///     aggregates, hidden parent/child state, Model-A violation, parent/child non-terminality).
///   - sdk59: an end-to-end in-ladder split PAYMENT over the SDK (`transfer()` → `in_ladder_pay`),
///     receiver adopts via `verify_child_bundle` and unilaterally exits; funds land at the receiver.
/// Plus S1/S2 (sdk54/sdk55). See `docs/utexo/V2-SPLIT-FINDINGS.md`.
///
/// V2 SEMANTIC: a RECEIVED non-exact (split-child) payment is FIRST-CLASS. Its claim completes the
/// standard SE key handover, so the receiver co-owns `A_child` (invariant across the rotation, which is
/// what keeps the pre-signed exit chain valid) and the sender is locked out. It can be paid onward
/// off-chain, whole, via `child_retransfer` — each hop co-signs a fresh lower-CSV state and discloses
/// the one it replaces for the receiver's census (`docs/utexo/V2-CHILD-FIRSTCLASS.md`, sdk60).
/// Two limits remain: its funding `SP.out[j]` is un-broadcast, so a COOPERATIVE withdrawal to an
/// arbitrary address still routes to the unilateral exit; and a child cannot itself be split in-ladder
/// yet, so it is spendable only whole. (The rejected alternative — "SE handover + budget-reopen" — was
/// adversarially unsound; the sound design conveys the handover instead of reopening a budget.)
fn deposit_protocol_default() -> u32 {
    std::env::var("UTEXO_PROTOCOL_DEFAULT").ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(2)
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
            auto_exit_margin_blocks: 288,
            deposit_protocol_version: deposit_protocol_default(),
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
            deposit_protocol_version: deposit_protocol_default(),
        }
    }
}
