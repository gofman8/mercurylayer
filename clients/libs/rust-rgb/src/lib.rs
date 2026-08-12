//! Mercury <-> RGB bridge.
//!
//! Thin wrapper around [`rgb_lib`] that lets the Mercury Layer statechain client issue RGB assets,
//! color the (externally built) statechain backup/withdrawal transactions and accept incoming
//! consignments - all while keeping RGB's `bitcoin`/`bdk` types isolated from Mercury's pinned
//! `bitcoin 0.30`. The only data crossing this boundary is strings: base64 PSBTs, hex txids and
//! base64 consignments.
//!
//! This bridge uses only the **public** rgb-lib API - the same special-usage methods that
//! rgb-lightning-node uses (`color_psbt_and_consume`, `post_consignment`, `accept_transfer`) - so
//! the rgb-lib dependency needs no source changes for coloring/accepting. The single statechain
//! addition in the rgb-lib fork is `fund_statechain_utxo` (deposit binding), which has no public
//! equivalent because it spends a colored UTXO to an externally-owned (statechain) address.
//!
//! Model (RGB-over-statechain): the asset stays bound to the **statechain UTXO** (the `Tx0`
//! outpoint) throughout the coin's off-chain life. Each Mercury transfer produces a *colored*
//! backup transaction whose RGB transition closes the seal on the statechain UTXO and re-assigns
//! the asset to the new owner's output. The transition only becomes on-chain-valid when a witness
//! transaction is broadcast: the cooperative withdrawal transaction, or - on a unilateral exit -
//! the latest backup transaction. This mirrors how RGB is used over Lightning commitment txs.

use std::collections::HashMap;
use std::fs;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rgb_lib::bitcoin::psbt::Psbt as RgbPsbt;
use rgb_lib::wallet::{
    rust_only::{check_indexer_url, AssetColoringInfo, ColoringInfo, IndexerProtocol},
    RgbWalletOpsOffline, RgbWalletOpsOnline, SinglesigKeys, TransportEndpoint, Wallet,
};
use rgb_lib::{
    generate_keys, restore_keys,
    keys::WitnessVersion,
    wallet::{DatabaseType, Online, OnlineOptions, WalletData},
    Assignment, AssetSchema, BitcoinNetwork, ConsignmentExt, ContractId, FileContent, RgbTransport,
};

/// Why an off-chain consignment validation did not come back valid.
///
/// This MIRRORS the taxonomy the rgb-lib fork already publishes and does not invent a new one.
/// `rgb_lib::wallet::rust_only::validate_consignment_offchain_chain` matches on rgbstd's
/// `ValidationError` and reports the arm in `ValidateConsignmentResult::error`:
///
/// * `Err(ValidationError::InvalidConsignment(_))` -> `error = Some("invalid")` — the consignment
///   itself does not validate. Re-running it a thousand times cannot change that verdict.
/// * `Err(ValidationError::ResolverError(_))`      -> `error = Some("resolver")` — validation never
///   reached a verdict because a witness could not be resolved (indexer / electrum / esplora down,
///   timing out, rate-limiting, mid-reorg). The consignment may well be perfectly good.
///
/// Both arms set `valid: false`, so a caller that reads only the boolean treats a five-second
/// network blip as proof of forgery. Anything that acts irreversibly on "invalid" — above all the
/// claim path, which UN-QUARANTINES the coin and thereby throws away its RGB protection for good —
/// must branch on this instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationVerdict {
    /// The consignment validated.
    Valid,
    /// A real, reproducible verdict of INVALID from the RGB validator. Permanent.
    PermanentlyInvalid,
    /// No verdict was reached: a witness could not be resolved. Transient — retry later.
    Unresolved,
}


/// **[P2] A distinct, DETERMINISTIC blinding per payload output.**
///
/// A seal's blinding conceals it. Sharing one across a transaction's payload outputs makes the
/// concealment collective: a recipient who learns the secret for their own leg de-conceals every
/// sibling. rgb-lib's `new_random_vout` would separate them but destroys byte-for-byte rebuild on a
/// retry, which the un-broadcast statechain lane depends on — a rebuilt tier with a different seal
/// is a different transaction, and the co-signature no longer matches.
///
/// So the per-vout value is derived from the operation's own blinding and the vout: distinct for
/// every output, identical across reruns of the same operation, and revealing nothing about a
/// sibling without the base secret.
fn per_output_blinding(base: u64, vout: u32) -> u64 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"utexo/p2/per-output-blinding/v1");
    h.update(base.to_be_bytes());
    h.update(vout.to_be_bytes());
    let d = h.finalize();
    u64::from_be_bytes(d[..8].try_into().expect("32-byte digest"))
}

impl ValidationVerdict {
    /// Map one `ValidateConsignmentResult` (`valid` + `error` tag) onto this enum.
    ///
    /// Fails CLOSED on anything unrecognised: an `error` tag this bridge does not know is treated as
    /// [`Self::Unresolved`], i.e. transient. That is the safe direction — a caller keeps retrying and
    /// keeps its quarantine — whereas guessing `PermanentlyInvalid` would irreversibly discard
    /// protection on the strength of a string this code has never seen before. If rgb-lib ever gains
    /// a third *permanent* arm, it must be added here explicitly.
    pub fn from_rgb_lib(valid: bool, error: Option<&str>) -> Self {
        if valid {
            return Self::Valid;
        }
        match error {
            Some("invalid") => Self::PermanentlyInvalid,
            _ => Self::Unresolved,
        }
    }

    /// True only for a verdict that may be acted on irreversibly.
    pub fn is_permanently_invalid(self) -> bool {
        matches!(self, Self::PermanentlyInvalid)
    }
}

/// The witness txid of every bundle carried by `consignment_base64`, in the consignment's own
/// bundle order.
///
/// This is the read side of the CTES-R **build-time collision assert** (`docs/utexo/CTESR-GATE.md`
/// §3.1). Two RGB transitions over the SAME parent outpoint with the same amounts and the same seal
/// blinding collapse to one `OpId` and one `BundleId` (`TransitionBundle::commit_encode` commits
/// only the `input_map`); rgb-lib then resolves that one BundleId to whichever rival witness has the
/// numerically smallest *internal* (little-endian) txid. The loser's own consignment therefore comes
/// back embedding the RIVAL's witness, and no branch the receiver can try will validate.
///
/// So: after colouring a tier, the caller MUST check that this list contains the tier's own txid.
/// Absence is proof that the seal blinding collided — never a receiver-side problem to discover.
pub fn consignment_witness_txids(consignment_base64: &str) -> Result<Vec<String>> {
    let bytes = STANDARD
        .decode(consignment_base64)
        .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
    let dir = unique_tmp("mercury-rgb-witnesses");
    fs::create_dir_all(&dir)?;
    let path = dir.join("consignment");
    fs::write(&path, &bytes)?;
    let consignment = rgb_lib::wallet::rust_only::load_transfer(
        path.to_str().ok_or_else(|| anyhow!("bad temp path"))?,
    )?;
    let _ = fs::remove_dir_all(&dir);
    Ok(consignment
        .bundled_witnesses()
        .map(|wb| wb.pub_witness.txid().to_string())
        .collect())
}

/// An rgb-lib wallet wired for use alongside a Mercury Layer wallet.
pub struct RgbWallet {
    wallet: Wallet,
    online: Online,
    indexer_url: String,
    /// RGB proxy transport endpoint, e.g. `rpc://127.0.0.1:3000/json-rpc`.
    proxy: String,
}

fn map_network(network: &str) -> Result<BitcoinNetwork> {
    Ok(match network.to_lowercase().as_str() {
        "mainnet" | "bitcoin" => BitcoinNetwork::Mainnet,
        "testnet" => BitcoinNetwork::Testnet,
        "signet" => BitcoinNetwork::Signet,
        "regtest" => BitcoinNetwork::Regtest,
        other => return Err(anyhow!("unsupported network: {other}")),
    })
}

fn unique_tmp(prefix: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), unique))
}

impl RgbWallet {
    /// Generate a fresh mnemonic for a new RGB wallet (taproot keychain).
    pub fn generate_mnemonic(network: &str) -> Result<String> {
        let keys = generate_keys(map_network(network)?, WitnessVersion::Taproot);
        Ok(keys.mnemonic)
    }

    /// Open (creating if needed) an rgb-lib wallet from a mnemonic and bring it online against the
    /// given electrum `indexer_url`. `proxy` is the RGB proxy transport endpoint
    /// (e.g. `rpc://127.0.0.1:3000/json-rpc`), used for consignment exchange.
    pub fn open(
        data_dir: &str,
        mnemonic: &str,
        network: &str,
        indexer_url: &str,
        proxy: &str,
    ) -> Result<Self> {
        let bitcoin_network = map_network(network)?;

        let keys = restore_keys(bitcoin_network, mnemonic.to_string(), WitnessVersion::Taproot)?;
        let wallet_keys = SinglesigKeys::from_keys(&keys, None);

        let wallet_data = WalletData {
            data_dir: data_dir.to_string(),
            bitcoin_network,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 1,
            supported_schemas: vec![
                AssetSchema::Nia,
                AssetSchema::Uda,
                AssetSchema::Cfa,
                AssetSchema::Ifa,
            ],
            reuse_addresses: false,
        };

        let mut wallet = Wallet::new(wallet_data, wallet_keys)?;

        // Validate the indexer before going online so misconfiguration fails early.
        let _: IndexerProtocol = check_indexer_url(indexer_url, bitcoin_network)?;

        let online = wallet.go_online(OnlineOptions {
            indexer_url: indexer_url.to_string(),
            skip_consistency_check: true,
            // Fast vanilla syncs only cover revealed addresses from `last_used (or last_revealed)
            // - lookback` up to `last_revealed`. Every `get_address` call reveals a NEW index, so
            // with lookback 0 an address funded BEFORE a later reveal falls out of the window and
            // its UTXO becomes invisible ("Insufficient bitcoin funds ... available '0'" from
            // create_utxos while the coins sit confirmed on-chain). A small margin keeps recent
            // funding addresses in scope; long-idle wallets still rely on callers not to reveal
            // more than this many addresses between funding and spending.
            vanilla_sync_lookback: 20,
        })?;

        Ok(Self {
            wallet,
            online,
            indexer_url: indexer_url.to_string(),
            proxy: proxy.to_string(),
        })
    }

    /// A bitcoin address of the underlying RGB (BDK) wallet, used to fund it with sats so it can
    /// create the UTXOs needed to hold allocations and pay RGB witness fees.
    pub fn get_address(&mut self) -> Result<String> {
        Ok(self.wallet.get_address()?)
    }

    /// Create `num` colorable UTXOs of `size` sats each in the RGB wallet (needed before issuance).
    /// The size must be large enough to later fund the statechain deposit from the colored UTXO.
    pub fn create_utxos(&mut self, num: u8, size: u32, fee_rate: u64) -> Result<u8> {
        Ok(self.wallet.create_utxos(
            self.online.clone(),
            false,
            Some(num),
            Some(size),
            fee_rate,
            false,
        )?)
    }

    /// Issue a NIA (Non-Inflatable Asset) and return its contract/asset id.
    pub fn issue_nia(
        &self,
        ticker: &str,
        name: &str,
        precision: u8,
        amounts: Vec<u64>,
    ) -> Result<String> {
        let asset = self
            .wallet
            .issue_asset_nia(ticker.to_string(), name.to_string(), precision, amounts)?;
        Ok(asset.asset_id)
    }

    /// Issue an IFA (Inflatable Fungible Asset): `amounts` are minted now, `inflation_amounts` are
    /// inflation-right the issuer can later realize with [`Self::inflate`]. Offline (no broadcast),
    /// like [`Self::issue_nia`]. Returns the contract/asset id.
    pub fn issue_ifa(
        &self,
        ticker: &str,
        name: &str,
        precision: u8,
        amounts: Vec<u64>,
        inflation_amounts: Vec<u64>,
    ) -> Result<String> {
        let asset = self.wallet.issue_asset_ifa(
            ticker.to_string(),
            name.to_string(),
            precision,
            amounts,
            inflation_amounts,
            None,
        )?;
        Ok(asset.asset_id)
    }

    /// Issue a UDA (Unique Digital Asset — a single-token NFT). Offline (no broadcast). `details`
    /// and media are optional; tests typically pass `None`/empty. Returns the contract/asset id.
    pub fn issue_uda(
        &self,
        ticker: &str,
        name: &str,
        details: Option<&str>,
        precision: u8,
        media_file_path: Option<&str>,
        attachments_file_paths: Vec<String>,
    ) -> Result<String> {
        let asset = self.wallet.issue_asset_uda(
            ticker.to_string(),
            name.to_string(),
            details.map(|s| s.to_string()),
            precision,
            media_file_path.map(|s| s.to_string()),
            attachments_file_paths,
        )?;
        Ok(asset.asset_id)
    }

    /// Issue a CFA (Collectible Fungible Asset). Offline (no broadcast). Like NIA but with a `name`
    /// (no ticker), optional `details` and optional media. Returns the contract/asset id.
    pub fn issue_cfa(
        &self,
        name: &str,
        details: Option<&str>,
        precision: u8,
        amounts: Vec<u64>,
        file_path: Option<&str>,
    ) -> Result<String> {
        let asset = self.wallet.issue_asset_cfa(
            name.to_string(),
            details.map(|s| s.to_string()),
            precision,
            amounts,
            file_path.map(|s| s.to_string()),
        )?;
        Ok(asset.asset_id)
    }

    /// Realize `inflation_amounts` of an IFA's inflation-right as new fungible supply in this
    /// wallet's RGB engine. **ON-CHAIN**: broadcasts a witness tx (inflation is a contract state
    /// transition — there is no off-chain variant in RGB). Returns `(txid, minted_total)`.
    pub fn inflate(
        &mut self,
        asset_id: &str,
        inflation_amounts: Vec<u64>,
        fee_rate: u64,
    ) -> Result<(String, u64)> {
        let minted: u64 = inflation_amounts.iter().sum();
        let res = self.wallet.inflate(
            self.online.clone(),
            asset_id.to_string(),
            inflation_amounts,
            fee_rate,
            1,
        )?;
        Ok((res.txid, minted))
    }

    /// Burn `amount` of an asset's FREE (engine-held) balance. **ON-CHAIN**: broadcasts a witness
    /// tx. Statechain-bound supply must be exited back into the engine before it can be burned.
    /// Returns the burn txid.
    pub fn burn(&mut self, asset_id: &str, amount: u64, fee_rate: u64) -> Result<String> {
        let res = self
            .wallet
            .burn(self.online.clone(), asset_id.to_string(), amount, fee_rate, 1)?;
        Ok(res.txid)
    }

    /// List NIA + IFA assets known to this wallet: (asset_id, ticker, name, precision).
    pub fn list_assets(&self) -> Result<Vec<(String, String, String, u8)>> {
        let assets = self.wallet.list_assets(vec![])?;
        let mut out = Vec::new();
        for a in assets.nia.unwrap_or_default() {
            out.push((a.asset_id, a.ticker, a.name, a.precision));
        }
        for a in assets.ifa.unwrap_or_default() {
            out.push((a.asset_id, a.ticker, a.name, a.precision));
        }
        for a in assets.uda.unwrap_or_default() {
            out.push((a.asset_id, a.ticker, a.name, a.precision));
        }
        for a in assets.cfa.unwrap_or_default() {
            // CFA has no ticker; expose the name in both slots so callers can match by name.
            out.push((a.asset_id, a.name.clone(), a.name, a.precision));
        }
        Ok(out)
    }

    /// Contract metadata for an asset: `(schema, ticker, name, precision, initial_supply,
    /// max_supply, details)`. `schema` is one of "Nia"/"Uda"/"Cfa"/"Ifa"; for a non-inflatable asset
    /// `max_supply == initial_supply`, and for an IFA `max_supply == initial_supply + inflation
    /// right`. `ticker`/`details` are "" when absent. Mirrors rgb-lib `get_asset_metadata`.
    pub fn asset_metadata(
        &self,
        asset_id: &str,
    ) -> Result<(String, String, String, u8, u64, u64, u64)> {
        let m = self.wallet.get_asset_metadata(asset_id.to_string())?;
        Ok((
            format!("{:?}", m.asset_schema),
            m.ticker.unwrap_or_default(),
            m.name,
            m.precision,
            m.initial_supply,
            m.max_supply,
            m.known_circulating_supply,
        ))
    }

    /// Settled balance of an asset in this wallet.
    pub fn settled_balance(&self, asset_id: &str) -> Result<u64> {
        Ok(self.wallet.get_asset_balance(asset_id.to_string())?.settled)
    }

    /// Full RGB balance of an asset: `(settled, future, spendable)`.
    pub fn balance(&self, asset_id: &str) -> Result<(u64, u64, u64)> {
        let b = self.wallet.get_asset_balance(asset_id.to_string())?;
        Ok((b.settled, b.future, b.spendable))
    }

    /// RGB allocations of an asset held on this wallet's UTXOs: `(outpoint, amount, settled)`.
    pub fn list_allocations(&mut self, asset_id: &str) -> Result<Vec<(String, u64, bool)>> {
        let unspents = self
            .wallet
            .list_unspents(Some(self.online.clone()), false, false)?;
        let mut out = vec![];
        for u in unspents {
            for a in &u.rgb_allocations {
                if a.asset_id.as_deref() == Some(asset_id) {
                    if let Assignment::Fungible(amt) = a.assignment {
                        out.push((
                            format!("{}:{}", u.utxo.outpoint.txid, u.utxo.outpoint.vout),
                            amt,
                            a.settled,
                        ));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Summary of this wallet's transfers for an asset: `(kind, status, amount, txid)`.
    pub fn transfers(&self, asset_id: &str) -> Result<Vec<(String, String, u64, String)>> {
        let transfers = self.wallet.list_transfers(Some(asset_id.to_string()))?;
        Ok(transfers
            .into_iter()
            .map(|t| {
                let amount: u64 = t
                    .assignments
                    .iter()
                    .map(|a| match a {
                        Assignment::Fungible(n) | Assignment::InflationRight(n) => *n,
                        _ => 0,
                    })
                    .sum();
                (
                    format!("{:?}", t.kind),
                    format!("{:?}", t.status),
                    amount,
                    t.txid.unwrap_or_default(),
                )
            })
            .collect())
    }

    /// Sync RGB state with the indexer (settle pending transfers, update witnesses).
    pub fn refresh(&mut self, asset_id: Option<String>) -> Result<()> {
        self.wallet
            .refresh(self.online.clone(), asset_id, vec![], false)?;
        Ok(())
    }

    /// **[CTES-R] The read-only STOCK probe of `docs/utexo/CTESR-GATE.md` §3.3.** Can `output_map`
    /// still be spent, right now, out of the outpoints `psbt_base64` spends?
    ///
    /// This is the ONLY health check the gate sanctions, and the reason is measured, not stylistic:
    /// E7 killed a coloured ladder's stock outright and `get_asset_balance` went on reporting
    /// `settled = future = spendable = 1000`, because rgb-lib's balance is computed from its sqlite
    /// tables and a stock invalidation touches neither. `list_unspents` was equally blind. A
    /// monitoring alarm built on either would never have fired.
    ///
    /// It probes the stock because `color_psbt` builds the transition from
    /// `contract_assignments_for(contract_id, prev_outputs)` — i.e. from the STASH — and refuses
    /// with `InvalidColoringInfo { "total amount in output_map (N) greater than available (0)" }`
    /// when the allocation is gone. A dead stash therefore answers `Err` where a live one answers
    /// `Ok`, which is exactly the discrimination the balance cannot make.
    ///
    /// **Read-only by construction, not by convention:** `color_psbt` (NOT
    /// `color_psbt_and_consume`) builds the fascia in memory and returns it; nothing is consumed,
    /// no transfer row is written, no witness is touched, and the caller's `psbt_base64` is
    /// untouched because the parsed copy is local. Safe to run against a live coin, including
    /// mid-exit.
    ///
    /// `Ok(())` means spendable. `Err` carries rgb-lib's own reason.
    pub fn probe_spendable(
        &self,
        psbt_base64: &str,
        contract_id: &str,
        output_map: HashMap<u32, u64>,
        blinding: u64,
    ) -> Result<()> {
        let contract = ContractId::from_str(contract_id)
            .map_err(|e| anyhow!("invalid contract id: {e}"))?;
        let mut psbt = RgbPsbt::from_str(psbt_base64).map_err(|e| anyhow!("invalid psbt: {e}"))?;
        let coloring_info = ColoringInfo {
            asset_info_map: HashMap::from([(
                contract,
                AssetColoringInfo {
                    // [P2] ONE SECRET PER PAYLOAD SEAL. `static_blinding` applies a single value to
                    // every output, so in a multi-payload transaction each recipient can de-conceal
                    // every sibling seal with the secret they legitimately learn for their own leg —
                    // reading who else was paid, and how much, out of a batch they were entitled to
                    // one leg of.
                    //
                    // Derived, not random: the transaction must rebuild byte-for-byte on a retry, so
                    // each vout's blinding is a deterministic function of the shared blinding and
                    // the vout. Distinct per output, reproducible across runs.
                    output_blinding: output_map
                        .keys()
                        .map(|vout| (*vout, per_output_blinding(blinding, *vout)))
                        .collect(),
                    output_map,
                    blinded_map: HashMap::new(),
                    static_blinding: Some(blinding),
                },
            )]),
            static_blinding: Some(blinding),
            nonce: None,
        };
        // `color_psbt` — never `color_psbt_and_consume`. The fascia is dropped on return.
        let _ = self.wallet.color_psbt(&mut psbt, coloring_info)?;
        Ok(())
    }

    /// **[D3] Why did rgb-lib refuse to colour this outpoint?** Read-only.
    ///
    /// [`Self::probe_spendable`] answers *whether*; this answers *why*. rgb-lib's own refusal is
    /// `Invalid coloring info { "total amount in output_map (N) greater than available (0)" }`,
    /// which names neither of the two filters that can produce that `0`:
    ///
    /// * `check_witness` — the allocation's witness must be in the stock's witness-ord map and must
    ///   not be `Archived`. A deliberately un-broadcast branch resolved with the PLAIN blockchain
    ///   resolver comes back `Unresolved`, and `WitnessStatus::Unresolved.witness_ord()` is
    ///   `Archived`. That is the E7 class, and it is persisted.
    /// * `check_bundle` — the allocation's bundle must not be in the persisted `invalid_bundles`
    ///   set, which archival populates recursively.
    ///
    /// Meanwhile `get_asset_balance` is computed from sqlite and does not move, so the coin looks
    /// healthy from every angle except the one that matters. Never diagnose this from a balance.
    ///
    /// Returns `(verdict, visible_amount, witness_ord, stash_holds_witness_tx,
    /// invalid_bundle_count)`. The verdict is the `Debug` rendering of rgb-lib's
    /// `AllocationVerdict` — `Spendable`, `WitnessUnknownToStock`, `WitnessArchived`,
    /// `BundleInvalidated` or `SealNotRevealed`.
    ///
    /// Nothing is consumed, no witness is resolved and no indexer is consulted (E7-safe).
    pub fn diagnose_allocation(
        &self,
        contract_id: &str,
        txid: &str,
        vout: u32,
    ) -> Result<(String, u64, Option<String>, bool, usize)> {
        use rgb_lib::wallet::Outpoint;
        let d = self.wallet.diagnose_allocation(
            contract_id.to_string(),
            Outpoint { txid: txid.to_string(), vout },
        )?;
        Ok((
            format!("{:?}", d.verdict()),
            d.visible_amount,
            d.witness_ord.clone(),
            d.stash_holds_witness_tx,
            d.invalid_bundle_count,
        ))
    }

    /// Every witness the RGB stock holds an ord for, as `(txid, ord)`. Read-only.
    ///
    /// The chain-level companion to [`Self::diagnose_allocation`]: on a CTES-R ladder every rung
    /// must read `Tentative` (it is un-broadcast by design). Any `Archived` rung is the E7 defect,
    /// and the position of the first one says which step archived it.
    pub fn witness_ords(&self) -> Result<Vec<(String, String)>> {
        Ok(self.wallet.witness_ords()?)
    }

    /// Color a Mercury-built unsigned transaction (provided as a base64 PSBT) so that the RGB asset
    /// is re-assigned to the given pre-coloring output vouts, returning the modified PSBT (base64,
    /// with the OP_RETURN opret commitment added) and the consignment (base64) to relay to the
    /// receiver.
    ///
    /// Uses the public `color_psbt_and_consume` (no rgb-lib source change). `output_map` maps
    /// *pre-coloring* output index -> asset amount. `blinding` is the deterministic seal blinding
    /// (must be shared with the receiver so it can accept).
    ///
    /// Returns `(colored_psbt_base64, consignment_base64)`.
    pub fn color(
        &self,
        psbt_base64: &str,
        contract_id: &str,
        output_map: HashMap<u32, u64>,
        blinding: u64,
    ) -> Result<(String, String)> {
        self.color_with_nonce(psbt_base64, contract_id, output_map, blinding, None)
    }

    /// [`Self::color`] plus an explicit `ColoringInfo.nonce`.
    ///
    /// The nonce lands in the RGB transition (`transition_builder.set_nonce`) and therefore
    /// independently separates the `OpId` **and** the `BundleId` of two transitions over the same
    /// parent outpoint — the belt-and-braces half of `docs/utexo/CTESR-GATE.md` §3.1. It is a
    /// *second* separation, never a substitute: rival tiers must already carry distinct seal
    /// blindings (see `mercuryrustlib::rgb::TierSeal`), because the blinding is what the receiver
    /// re-derives. Pass `None` for the historical behaviour (`u64::MAX`, rgb-lib's default).
    pub fn color_with_nonce(
        &self,
        psbt_base64: &str,
        contract_id: &str,
        output_map: HashMap<u32, u64>,
        blinding: u64,
        nonce: Option<u64>,
    ) -> Result<(String, String)> {
        let contract = ContractId::from_str(contract_id)
            .map_err(|e| anyhow!("invalid contract id: {e}"))?;
        let mut psbt = RgbPsbt::from_str(psbt_base64).map_err(|e| anyhow!("invalid psbt: {e}"))?;

        let coloring_info = ColoringInfo {
            asset_info_map: HashMap::from([(
                contract,
                AssetColoringInfo {
                    // [P2] ONE SECRET PER PAYLOAD SEAL. `static_blinding` applies a single value to
                    // every output, so in a multi-payload transaction each recipient can de-conceal
                    // every sibling seal with the secret they legitimately learn for their own leg —
                    // reading who else was paid, and how much, out of a batch they were entitled to
                    // one leg of.
                    //
                    // Derived, not random: the transaction must rebuild byte-for-byte on a retry, so
                    // each vout's blinding is a deterministic function of the shared blinding and
                    // the vout. Distinct per output, reproducible across runs.
                    output_blinding: output_map
                        .keys()
                        .map(|vout| (*vout, per_output_blinding(blinding, *vout)))
                        .collect(),
                    output_map,
                    blinded_map: HashMap::new(),
                    static_blinding: Some(blinding),
                },
            )]),
            static_blinding: Some(blinding),
            nonce,
        };

        let transfers = self.wallet.color_psbt_and_consume(&mut psbt, coloring_info)?;
        let transfer = transfers
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("color produced no consignment"))?;

        let dir = unique_tmp("mercury-rgb-color");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        transfer
            .save_file(&path)
            .map_err(|e| anyhow!("could not serialize consignment: {e}"))?;
        let bytes = fs::read(&path)?;
        let _ = fs::remove_dir_all(&dir);

        Ok((psbt.to_string(), STANDARD.encode(bytes)))
    }

    /// Color a PSBT assigning to **blinded** beneficiaries (existing outpoints) rather than witness
    /// vouts: `blinded` is a list of `(recipient_id, amount)` where each `recipient_id` is a
    /// `blind_receive` blinded seal (e.g. on a statechain UTXO), enabling a statechain->statechain
    /// transfer where the receiver receives on its own statechain UTXO and change goes to a free
    /// statechain UTXO. Uses the public `color_psbt_and_consume` + the fork's `blinded_map`.
    /// Returns `(colored_psbt_base64, consignment_base64)` - one consignment covering all beneficiaries.
    pub fn color_blinded(
        &self,
        psbt_base64: &str,
        contract_id: &str,
        blinded: Vec<(String, u64)>,
        blinding: u64,
    ) -> Result<(String, String)> {
        let contract = ContractId::from_str(contract_id)
            .map_err(|e| anyhow!("invalid contract id: {e}"))?;
        let mut psbt = RgbPsbt::from_str(psbt_base64).map_err(|e| anyhow!("invalid psbt: {e}"))?;
        let blinded_map: HashMap<String, u64> = blinded.into_iter().collect();
        let coloring_info = ColoringInfo {
            asset_info_map: HashMap::from([(
                contract,
                AssetColoringInfo {
                    output_map: HashMap::new(),
                    blinded_map,
                    static_blinding: Some(blinding),
                    // [P2] No witness-vout seals on this lane — every beneficiary is a BLINDED seal
                    // the receiver produced with `blind_receive`, already concealed under a secret
                    // only they hold. There is nothing here for a per-output blinding to separate.
                    output_blinding: HashMap::new(),
                },
            )]),
            static_blinding: Some(blinding),
            nonce: None,
        };
        let transfers = self.wallet.color_psbt_and_consume(&mut psbt, coloring_info)?;
        let transfer = transfers
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("color produced no consignment"))?;
        let dir = unique_tmp("mercury-rgb-color-blinded");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        transfer
            .save_file(&path)
            .map_err(|e| anyhow!("could not serialize consignment: {e}"))?;
        let bytes = fs::read(&path)?;
        let _ = fs::remove_dir_all(&dir);
        Ok((psbt.to_string(), STANDARD.encode(bytes)))
    }

    /// Validate a consignment (base64) **off-chain** — before its witness (exit) transaction is
    /// broadcast — using the witness bundled inside the consignment (rgb-lib's `OffchainResolver`).
    /// `txid` is the witness/exit transaction id. This is how a receiver accepts an off-chain P2P
    /// statechain transfer: it checks that the sender's *unbroadcast* exit tx commits the RGB
    /// transition assigning the asset to the receiver's seal, without anything touching Bitcoin.
    /// Returns `(valid, detail)` where `detail` is a warning summary when valid or the failure reason.
    pub fn validate_offchain(&self, consignment_base64: &str, txid: &str) -> Result<(bool, Option<String>)> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-validate");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let network = self.wallet.get_wallet_data().bitcoin_network;
        let res = rgb_lib::wallet::rust_only::validate_consignment_offchain(
            path.to_str().ok_or_else(|| anyhow!("bad temp path"))?,
            txid,
            &self.indexer_url,
            network,
        )?;
        let _ = fs::remove_dir_all(&dir);
        let detail = if res.valid {
            res.warnings.map(|w| w.join("; "))
        } else {
            res.details.or(res.error)
        };
        Ok((res.valid, detail))
    }

    /// Validate a consignment **off-chain** whose witness is a CHAIN of un-broadcast transactions —
    /// a multi-level off-chain split (Stage 2 of `docs/rgb_offchain_split_spilman.md`). `txids` is the
    /// branch from the on-chain root down to the leaf (e.g. `[S1, S2]`); every one is resolved from
    /// the consignment's bundled witnesses, so an un-broadcast *parent* need not be on-chain (unlike
    /// [`Self::validate_offchain`], which only treats the single terminal txid as offchain).
    /// Import the asset (contract) carried by a consignment into this wallet's stash, validating
    /// against a chain of un-broadcast witnesses. Required before a receiver can register/spend an
    /// allocation of a contract it has never seen.
    pub fn import_asset_offchain(
        &self,
        consignment_base64: &str,
        txids: &[String],
    ) -> Result<()> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-import");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let consignment = rgb_lib::wallet::rust_only::load_transfer(
            path.to_str().ok_or_else(|| anyhow!("bad temp path"))?,
        )?;
        let _ = fs::remove_dir_all(&dir);
        self.wallet.save_new_asset(consignment, txids)?;
        Ok(())
    }

    /// Off-chain validate `consignment_base64` against the un-broadcast branch `txids` and return
    /// the fungible amount the consignment assigns to the receiver's OWN witness outpoint
    /// (`witness_txid`:`vout`). The consignment-derived amount — a receiver books this instead of
    /// trusting a sender-supplied value.
    pub fn accept_offchain_amount(
        &self,
        consignment_base64: &str,
        txids: &[String],
        witness_txid: &str,
        vout: u32,
    ) -> Result<u64> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-offchain-amount");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let consignment = rgb_lib::wallet::rust_only::load_transfer(
            path.to_str().ok_or_else(|| anyhow!("bad temp path"))?,
        )?;
        let _ = fs::remove_dir_all(&dir);
        Ok(self
            .wallet
            .offchain_assigned_amount(consignment, txids, witness_txid, vout)?)
    }

    /// Structured off-chain chain validation for SDK receivers: like
    /// [`Self::validate_offchain_chain`] but also returning the consignment's verified contract
    /// id (only meaningful when valid) AND — critically — *why* a non-valid verdict came back.
    ///
    /// The boolean alone is not enough for the receiver: rgb-lib reports `valid == false` both for a
    /// consignment that is cryptographically broken (permanent) and for one it simply could not
    /// resolve because the indexer/electrum backend was unreachable (transient). Collapsing the two
    /// makes a network blip look exactly like a griefer's garbage. See [`ValidationVerdict`].
    pub fn validate_offchain_chain_info(
        &self,
        consignment_base64: &str,
        txids: &[String],
    ) -> Result<(ValidationVerdict, Option<String>, Option<String>)> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-validate-chain-info");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let network = self.wallet.get_wallet_data().bitcoin_network;
        let res = rgb_lib::wallet::rust_only::validate_consignment_offchain_chain(
            path.to_str().ok_or_else(|| anyhow!("bad temp path"))?,
            txids,
            &self.indexer_url,
            network,
        )?;
        let _ = fs::remove_dir_all(&dir);
        let detail = if res.valid {
            res.warnings.map(|w| w.join("; "))
        } else {
            res.details.or_else(|| res.error.clone())
        };
        Ok((
            ValidationVerdict::from_rgb_lib(res.valid, res.error.as_deref()),
            detail,
            res.contract_id,
        ))
    }

    pub fn validate_offchain_chain(
        &self,
        consignment_base64: &str,
        txids: &[String],
    ) -> Result<(bool, Option<String>)> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-validate-chain");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let network = self.wallet.get_wallet_data().bitcoin_network;
        let res = rgb_lib::wallet::rust_only::validate_consignment_offchain_chain(
            path.to_str().ok_or_else(|| anyhow!("bad temp path"))?,
            txids,
            &self.indexer_url,
            network,
        )?;
        let _ = fs::remove_dir_all(&dir);
        let detail = if res.valid {
            res.warnings.map(|w| w.join("; "))
        } else {
            res.details.or(res.error)
        };
        Ok((res.valid, detail))
    }

    /// Validate and accept an incoming consignment (base64) relayed in-band by the Mercury transfer
    /// message. The bytes are re-posted to the local RGB proxy and validated via the public
    /// `accept_transfer` (no rgb-lib source change). `txid`/`vout` identify the witness-tx output
    /// that will hold the asset and `blinding` is the seal blinding used by the sender.
    ///
    /// Returns the total fungible amount assigned to this wallet by the consignment.
    pub fn accept(
        &mut self,
        consignment_base64: &str,
        txid: &str,
        vout: u32,
        blinding: u64,
    ) -> Result<u64> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;

        let dir = unique_tmp("mercury-rgb-accept");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;

        // Re-post the in-band consignment to the proxy under recipient_id = txid, so the public
        // accept_transfer (which fetches by txid) validates exactly these bytes.
        let http_endpoint = TransportEndpoint::new(self.proxy.clone())?.endpoint;
        self.wallet.post_consignment(
            &http_endpoint,
            txid.to_string(),
            &path,
            txid.to_string(),
            Some(vout),
        )?;
        let _ = fs::remove_dir_all(&dir);

        let transport =
            RgbTransport::from_str(&self.proxy).map_err(|e| anyhow!("invalid proxy endpoint: {e}"))?;
        let (_transfer, assignments) =
            self.wallet
                .accept_transfer(txid.to_string(), vout, transport, blinding)?;

        // Audit [22] / INV-26: count only spendable Fungible amount as received value. An
        // InflationRight is NOT spendable balance (it moves solely via the mint path); booking it as
        // received would inflate the receiver's balance out of nothing. Mirrors offchain_assigned_amount.
        let received: u64 = assignments
            .into_iter()
            .map(|a| match a {
                Assignment::Fungible(amt) => amt,
                Assignment::InflationRight(_) | Assignment::NonFungible | Assignment::Any => 0,
            })
            .sum();

        Ok(received)
    }

    /// **CTES-R.** Accept a colored TES-R ladder: validate the LEAF consignment against the whole
    /// UN-BROADCAST tier chain and reveal EVERY tier seal, so the receiver both holds the allocation
    /// and can spend the extension's payload output on its next hop.
    ///
    /// `txids` is the ladder in exit order (`T, X_m, S_k`); `seals` is `(txid, vout, blinding)` per
    /// tier, in the same order. The blindings are DERIVED by the caller from the conveyed bundle
    /// (`mercuryrustlib::tesr::colored_tier_seals`) — never taken from a sender-supplied field.
    ///
    /// Returns the fungible amount the LEAF seal (the receiver's own exit output) receives.
    ///
    /// Distinct from [`Self::accept`] in the two ways that matter: `accept` resolves exactly one
    /// off-chain witness and reveals exactly one seal, so it can neither validate a multi-tier
    /// un-broadcast chain nor leave the receiver able to continue it.
    ///
    /// # [D3] This is also the missing call on the LEGACY lane
    ///
    /// The legacy token receive path books a piece with [`Self::import_asset_offchain`] +
    /// [`Self::register_statechain`] only. `import_asset_offchain` is rgb-lib's `save_new_asset`,
    /// which imports the **contract** and never `accept_transfer`s the transfer; `register_statechain`
    /// writes a **sqlite** row. So the receiver's stock ends up holding no witness ord for the
    /// split/combine txid and no revealed seal at the piece's outpoint, and every later
    /// `color_psbt` over that outpoint answers
    /// `Invalid coloring info: total amount in output_map (N) greater than available (0)` — while
    /// `get_asset_balance` reports the full amount, because that comes from sqlite.
    ///
    /// Measured, with the control, by `RGB_E2E=16`: the SAME consignment and the SAME outpoint go
    /// from `WitnessUnknownToStock` to `Spendable` when this call is added, and nothing else
    /// changes. A legacy piece can therefore be repaired by accepting it here with
    /// `txids = the branch witnesses` and `seals = [(funding_txid, vout, the sender's blinding)]` —
    /// it is a one-seal ladder. Doing so is an SDK receive-path change and is deliberately NOT
    /// done inside this bridge; see `RGB_E2E=16`'s module docs for the migration consequence of
    /// leaving it undone.
    pub fn accept_ladder(
        &mut self,
        consignment_base64: &str,
        txids: &[String],
        seals: &[(String, u32, u64)],
    ) -> Result<u64> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-accept-ladder");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let consignment = rgb_lib::wallet::rust_only::load_transfer(
            path.to_str().ok_or_else(|| anyhow!("bad temp path"))?,
        )?;
        let _ = fs::remove_dir_all(&dir);

        let assignments = self
            .wallet
            .accept_offchain_ladder(consignment, txids, seals)?;
        // INV-26, as in `accept`: only SPENDABLE fungible state counts as received. An
        // InflationRight is the right to mint, not balance.
        Ok(assignments
            .into_iter()
            .map(|a| match a {
                Assignment::Fungible(amt) => amt,
                Assignment::InflationRight(_) | Assignment::NonFungible | Assignment::Any => 0,
            })
            .sum())
    }

    /// Create a statechain UTXO of `size_sat` sats and deposit the wallet's **free allocation** of
    /// `contract_id` onto it - the statechain analogue of rgb-lib's `create_utxos` (which creates
    /// colorable UTXOs sized in sats). The deposited asset amount is the wallet's spendable balance
    /// (not a passed rgb-amount); after this, `get_asset_balance(contract_id)` drops to 0.
    ///
    /// Returns `(txid, vout)` of the statechain UTXO.
    pub fn create_statechain_utxo(
        &mut self,
        statechain_address: &str,
        size_sat: u64,
        contract_id: &str,
        fee_rate: u64,
        blinding: u64,
    ) -> Result<(String, u32)> {
        let free = self.wallet.get_asset_balance(contract_id.to_string())?.spendable;
        if free == 0 {
            return Err(anyhow!("no free allocation of {contract_id} to deposit"));
        }
        // Deposit the full free allocation onto a statechain UTXO of `size_sat` sats.
        self.deposit_via_send(statechain_address, size_sat, contract_id, free, fee_rate, blinding)
    }

    /// Color-based deposit that keeps the statechain UTXO **re-colorable** by the owner (so it can
    /// later transfer/exit), unlike `create_statechain_utxo`/`deposit_via_send` which mark the asset
    /// as sent away. Returns `(txid, vout, consignment_base64, signed_tx_hex)`; the caller broadcasts
    /// `signed_tx_hex`.
    pub fn fund_statechain(
        &mut self,
        address: &str,
        amount_sat: u64,
        contract_id: &str,
        rgb_amount: u64,
        fee_rate: u64,
        blinding: u64,
    ) -> Result<(String, u32, String, String)> {
        let _ = self
            .wallet
            .list_unspents(Some(self.online.clone()), false, false)?;
        let (txid, vout, consignment, signed_tx_hex) = self.wallet.fund_statechain_utxo(
            address.to_string(),
            amount_sat,
            contract_id.to_string(),
            rgb_amount,
            fee_rate,
            blinding,
        )?;
        Ok((txid, vout, STANDARD.encode(consignment), signed_tx_hex))
    }

    /// Register a Mercury statechain UTXO as a wallet-owned colorable UTXO, so standard rgb-lib
    /// methods (`get_asset_balance`, `list_unspents`, `blind_receive`) treat it exactly like an
    /// on-chain UTXO (design-doc item 1). `rgb_amount == 0` registers a *free* coin (onboarding: a
    /// statechain UTXO to receive on). Any allocation must already be in the stash (e.g. it was put
    /// there by `fund_statechain` on this wallet). Returns the txo idx.
    pub fn register_statechain(
        &self,
        txid: &str,
        vout: u32,
        btc_amount: u64,
        contract_id: &str,
        rgb_amount: u64,
        spend_outpoints: &[String],
    ) -> Result<i32> {
        use rgb_lib::wallet::Outpoint;
        let spend = spend_outpoints
            .iter()
            .filter_map(|s| {
                let (t, v) = s.rsplit_once(':')?;
                Some(Outpoint { txid: t.to_string(), vout: v.parse().ok()? })
            })
            .collect();
        Ok(self.wallet.register_statechain_utxo(
            txid.to_string(),
            vout,
            btc_amount,
            contract_id.to_string(),
            rgb_amount,
            spend,
        )?)
    }

    /// Mark registered statechain UTXO(s) (`txid:vout`) as spent in the DB - call after a transfer
    /// consumes a statechain UTXO so `get_asset_balance` drops to just the change.
    pub fn mark_spent(&self, outpoints: &[String]) -> Result<()> {
        use rgb_lib::wallet::Outpoint;
        let ops = outpoints
            .iter()
            .filter_map(|s| {
                let (t, v) = s.rsplit_once(':')?;
                Some(Outpoint { txid: t.to_string(), vout: v.parse().ok()? })
            })
            .collect();
        Ok(self.wallet.mark_utxos_spent(ops)?)
    }

    /// Standard rgb-lib **blinded receive** invoice for `amount`. The seal is one of this wallet's
    /// free colorable UTXOs - which, after `register_statechain`, can be a statechain UTXO. This is
    /// the user's "receiver creates an rgb invoice whose recipient_id references its statechain UTXO".
    /// Returns the `recipient_id` (the blinded seal the sender assigns to).
    pub fn blind_receive(&mut self, asset_id: Option<&str>, amount: u64) -> Result<String> {
        let rd = self.wallet.blind_receive(
            asset_id.map(|s| s.to_string()),
            Assignment::Fungible(amount),
            None,
            vec![self.proxy.clone()],
            0,
        )?;
        Ok(rd.recipient_id)
    }

    /// Dump this wallet's unspents as `(outpoint, btc_amount, colorable, exists, n_allocations,
    /// pending_blinded)` - so a test can show that a registered statechain UTXO is treated as a
    /// colorable on-chain UTXO (and, after `blind_receive`, carries a pending blinded seal).
    pub fn unspents_dump(&mut self) -> Result<Vec<(String, u64, bool, bool, usize, u32)>> {
        let unspents = self
            .wallet
            .list_unspents(Some(self.online.clone()), false, false)?;
        Ok(unspents
            .into_iter()
            .map(|u| {
                (
                    format!("{}:{}", u.utxo.outpoint.txid, u.utxo.outpoint.vout),
                    u.utxo.btc_amount,
                    u.utxo.colorable,
                    u.utxo.exists,
                    u.rgb_allocations.len(),
                    u.pending_blinded,
                )
            })
            .collect())
    }

    /// Deposit using rgb-lib's **standard** `send`: pay `amount_sat` to the statechain `address` as
    /// an RGB *witness* recipient assigned `rgb_amount` of `contract_id`. Because this goes through
    /// rgb-lib's normal transfer flow, all standard methods reflect it afterwards: the issuer's
    /// `balance` drops, `transfers` shows a `Send`, and `list_unspents` shows the spent colored UTXO
    /// plus a new change UTXO (carrying any asset change). The witness output that becomes the
    /// statechain UTXO is at vout 1 (rgb-lib places the OP_RETURN commitment at vout 0). `send`
    /// broadcasts the funding tx itself. Returns `(txid, vout)`.
    pub fn deposit_via_send(
        &mut self,
        address: &str,
        amount_sat: u64,
        contract_id: &str,
        rgb_amount: u64,
        fee_rate: u64,
        blinding: u64,
    ) -> Result<(String, u32)> {
        use rgb_lib::bitcoin::Address as BdkAddress;
        use rgb_lib::utils::recipient_id_from_script_buf;
        use rgb_lib::wallet::{Recipient, WitnessData};

        let network = self.wallet.get_wallet_data().bitcoin_network;
        let addr = BdkAddress::from_str(address)
            .map_err(|e| anyhow!("invalid statechain address: {e}"))?
            .assume_checked();
        let recipient_id = recipient_id_from_script_buf(addr.script_pubkey(), network);

        let recipient = Recipient {
            recipient_id,
            witness_data: Some(WitnessData {
                amount_sat,
                blinding: Some(blinding),
            }),
            assignment: Assignment::Fungible(rgb_amount),
            transport_endpoints: vec![self.proxy.clone()],
        };
        let recipient_map = HashMap::from([(contract_id.to_string(), vec![recipient])]);

        let res = self.wallet.send(
            self.online.clone(),
            recipient_map,
            true, // donation: broadcast without recipient ACK (the statechain UTXO is external)
            fee_rate,
            1,
            None,
            None,
        )?;
        Ok((res.txid, 1))
    }

    /// Generate a standard rgb-lib **witness receive** invoice for `amount` (of any asset - the
    /// contract is imported from the consignment on refresh). Registers a pending incoming transfer
    /// and reserves the wallet's next address; returns the `recipient_id` (which encodes that
    /// address). Used for the cooperative exit so the receiver settles via the normal `refresh` flow.
    pub fn witness_receive(&mut self, amount: u64) -> Result<String> {
        let rd = self.wallet.witness_receive(
            None,
            Assignment::Fungible(amount),
            None,
            vec![self.proxy.clone()],
            1,
        )?;
        Ok(rd.recipient_id)
    }

    /// The bitcoin address that a witness `recipient_id` will be paid at (so the externally-built
    /// colored withdrawal tx can pay it).
    pub fn address_from_recipient_id(&self, recipient_id: &str) -> Result<String> {
        use rgb_lib::bitcoin::{Address, Network};
        use rgb_lib::utils::script_buf_from_recipient_id;

        let script = script_buf_from_recipient_id(recipient_id.to_string())?
            .ok_or_else(|| anyhow!("recipient_id is not a witness recipient"))?;
        let net = match self.wallet.get_wallet_data().bitcoin_network {
            BitcoinNetwork::Mainnet => Network::Bitcoin,
            BitcoinNetwork::Regtest => Network::Regtest,
            BitcoinNetwork::Signet | BitcoinNetwork::SignetCustom => Network::Signet,
            _ => Network::Testnet, // Testnet/Testnet4 share the testnet address format
        };
        let addr = Address::from_script(&script, net)
            .map_err(|e| anyhow!("could not derive address from recipient_id: {e}"))?;
        Ok(addr.to_string())
    }

    /// Post a consignment (base64) to the RGB proxy under `recipient_id`, so the receiver's standard
    /// `refresh` can fetch and settle it. `txid`/`vout` identify the witness output.
    pub fn post_consignment(
        &self,
        recipient_id: &str,
        consignment_base64: &str,
        txid: &str,
        vout: u32,
    ) -> Result<()> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-post");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let http_endpoint = TransportEndpoint::new(self.proxy.clone())?.endpoint;
        self.wallet.post_consignment(
            &http_endpoint,
            recipient_id.to_string(),
            &path,
            txid.to_string(),
            Some(vout),
        )?;
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    pub fn indexer_url(&self) -> &str {
        &self.indexer_url
    }
}
