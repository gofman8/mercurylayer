//! Client-side driver for TES-R tier co-signing against the live blind SE.
//!
//! The SE is unchanged: it blind-co-signs whatever sighash the client presents (`/sign/first` +
//! `/sign/second`), so a tier tx (v3, relative-timelock, P2A anchor) round-trips through exactly the
//! same MuSig2 flow as an un-laddered coin's backup tx. This module wires [`mercurylib::tesr::cosign_tier_request`] into
//! that round-trip and returns the fully-signed, broadcast-ready tier tx.

use std::str::FromStr;

use anyhow::Result;
use electrum_client::ElectrumApi;
use mercurylib::{
    tesr::cosign_tier_request,
    transaction::{create_signature, new_backup_transaction},
    wallet::Coin,
};
use serde::{Deserialize, Serialize};

use crate::{
    client_config::ClientConfig,
    transaction::{sign_first, sign_second},
};

const TESR_BUNDLE_VERSION: u32 = 1;

/// One pre-signed, broadcast-ready TES-R tier tx as persisted in a coin's bundle.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TesrTier {
    pub txid: String,
    pub signed_tx: String,
    /// The value this tier pays forward (its payload output) — the prevout the child tier spends.
    pub out_value: u64,
    /// The relative-timelock (BIP-68 blocks) on this tier's input; `None` for the trigger.
    pub csv: Option<u16>,
    /// **Payload-vout accessor** — the output index at which this tier's PAYLOAD (value-carrying)
    /// outputs begin. `mercurylib::tesr::UNCOLORED_PAYLOAD_VOUT` (= 0) today; a coloured CTES-R tier
    /// carries the opret at 0 and shifts every payload by one.
    ///
    /// ⚠️ In a CONVEYED bundle this field is ATTACKER-SUPPLIED, exactly like every other field. It is
    /// never trusted on its own: every site that reads it cross-checks the index against transaction
    /// CONTENT (`spk == agg_spk`, `taproot_key_hex` on the spk, or the prevout amount feeding
    /// `verify_tier_cosigned`'s sighash), so a wrong value fails CLOSED with a named verification
    /// error rather than mis-chaining. `serde(default)` keeps every already-persisted `tesr-*` /
    /// `ctesr-*` row and every in-flight mailbox message deserializing byte-identically.
    #[serde(default)]
    pub payload_vout: u32,
}

impl TesrTier {
    /// This tier's payload output within `tx`. Fails CLOSED if `payload_vout` is out of range — a
    /// conveyed bundle can claim any index, and an out-of-range one must be a rejection, never a
    /// panic and never a silent fallback to `output[0]`.
    fn payload_out<'a>(
        &self,
        tx: &'a electrum_client::bitcoin::Transaction,
        what: &str,
    ) -> Result<&'a electrum_client::bitcoin::TxOut> {
        tx.output.get(self.payload_vout as usize).ok_or_else(|| {
            anyhow::anyhow!(
                "{what}: declared payload_vout {} is out of range ({} outputs)",
                self.payload_vout,
                tx.output.len()
            )
        })
    }
}

/// One depth LEVEL of the ladder: an extension and the state hanging off it. For a non-final level
/// the state is a SELF-SPLIT paying the aggregate `A` (its output hosts the next level — this is the
/// off-chain rollover); the final level's state is the exit leg to the owner.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TesrLevel {
    pub extension: TesrTier,
    pub state: TesrTier,
}

/// A coin's persisted TES-R ladder — everything an owner or a keyless tower needs to renew, roll over
/// or exit the coin, held on the owner's disk (the SE never sees it). Persisted under `tesr-<id>`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TesrBundle {
    pub version: u32,
    pub statechain_id: String,
    pub network: String,
    pub fee_rate: f64,
    /// P2TR(A) — the aggregate address every tier pays (invariant across renewals and transfers).
    pub agg_address: String,
    /// Where the final state pays on a unilateral exit.
    pub owner_exit_address: String,
    pub f_txid: String,
    pub f_vout: u32,
    pub f_value: u64,
    pub trigger: TesrTier,
    /// One entry per rollover depth level; the LAST level's state is the exit leg to the owner.
    pub levels: Vec<TesrLevel>,
    /// Renewal counter at the current (deepest) level.
    pub m: u32,
    /// SUPERSEDED states — every owner-paying state the SE co-signed that a later renewal/rollover/
    /// transfer replaced. Kept for FULL-DISCLOSURE counting (history/MIGRATION.md): the SE counts their
    /// co-signs, so verify_bundle must see them or num_sigs won't balance; and the current state must
    /// be at a strictly LOWER CSV than every one of these (it matures first, so a retained stale state
    /// loses the race). Not part of the exit chain.
    #[serde(default)]
    pub superseded_states: Vec<TesrTier>,
    /// SUPERSEDED extensions (from renewals) — kept only so their co-signs are counted.
    #[serde(default)]
    pub superseded_extensions: Vec<TesrTier>,
    /// The relative-timelock schedule this coin runs on (mainnet/regtest preset). Drives the
    /// param-based cadence in [`renew_auto`] / [`rollover_auto`]; defaults to mainnet for bundles
    /// persisted before this field existed.
    #[serde(default)]
    pub params: mercurylib::tesr::TesrParams,
}

/// [in-ladder split] A split child's exit bundle. It spans TWO aggregates: the ancestor segment
/// (`T, X_m, SP`) under the PARENT's aggregate `A_parent` (rooted at the on-chain funding `F`), and the
/// child's own headless ladder (`ext_child, state_child`) under `A_child` = the key `SP.out[sp_vout]`
/// pays. `verify_child_bundle` (the 8-check Stage-2 predicate, ruling wqvoxvusg) proves the child is
/// safe from a hidden parent state rivalling `SP` over `X_m.out[0]` WITHOUT any SGX change.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChildTesrBundle {
    /// The PARENT segment as an ordinary TES-R bundle: exit chain `T -> X_m -> SP`, with `SP` the
    /// current (terminal) state paying the children+P2A, and the parent's old owner state `S_0`
    /// disclosed in `superseded_states` (it rivals `SP` over `X_m.out[0]` and must lose the race).
    pub parent: TesrBundle,
    pub parent_statechain_id: String,
    /// Which `SP` output funds this child (`j`).
    pub sp_vout: u32,
    pub child_statechain_id: String,
    /// The child receiver's own exit key (Model A: the final child state must pay THIS).
    pub child_owner_exit_address: String,
    /// Child ladder: `child_extension` spends `SP.out[sp_vout]`; `child_state` spends its `out[0]`.
    pub child_extension: TesrTier,
    pub child_state: TesrTier,
    #[serde(default)]
    pub child_superseded_states: Vec<TesrTier>,
    #[serde(default)]
    pub child_superseded_extensions: Vec<TesrTier>,
    /// INTERMEDIATE child segments, root→leaf, when this child descends from another child (a
    /// child-level in-ladder split). Empty for a depth-1 child, which is why it is `serde(default)`:
    /// every already-persisted `ctesr-*` row and every in-flight mailbox message keeps deserializing.
    ///
    /// `sp_vout` is always relative to the IMMEDIATELY PRECEDING segment — `parent.current().state`
    /// when this is empty, otherwise `ancestors.last().state`.
    #[serde(default)]
    pub ancestors: Vec<ChildSegment>,
}

/// One INTERMEDIATE child segment of a multi-level child chain: the split state that funds the next
/// level, plus the ladder that hangs off it. Its `state` is the split (`SP`-like) tier whose
/// `out[next_vout]` funds the segment below.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChildSegment {
    /// This segment's statechain id (an ancestor of the leaf child; must be TERMINAL at the SE).
    pub statechain_id: String,
    /// Which output of the PRECEDING segment's `state` funds THIS segment (the preceding segment is
    /// `parent.current().state` for `ancestors[0]`, else `ancestors[i-1].state`).
    pub funding_vout: u32,
    /// The segment's own ladder: `extension` spends its funding outpoint, `state` spends ext.out[0].
    pub extension: TesrTier,
    pub state: TesrTier,
    #[serde(default)]
    pub superseded_states: Vec<TesrTier>,
    #[serde(default)]
    pub superseded_extensions: Vec<TesrTier>,
}

/// The SE-authoritative facts a verifier needs about ONE intermediate child segment. Supplied by the
/// caller (fetched from `/info/statechain` + `/statechain/spend_budget`), never taken from the bundle.
#[derive(Debug, Clone)]
pub struct AncestorFacts {
    pub num_sigs: u32,
    pub aggregate_pubkey: Option<String>,
    pub terminal: bool,
}

impl TesrBundle {
    /// The current (deepest) level.
    pub fn current(&self) -> &TesrLevel {
        self.levels.last().expect("a bundle always has >= 1 level")
    }
    /// Rollover depth (0 = a single level).
    pub fn level(&self) -> u32 {
        (self.levels.len() as u32).saturating_sub(1)
    }
    /// The prevout (txid, value) the current level's extension spends: the trigger at level 0, else
    /// the previous level's self-split state.
    fn current_parent(&self) -> (String, u64) {
        let n = self.levels.len();
        if n <= 1 {
            (self.trigger.txid.clone(), self.trigger.out_value)
        } else {
            (self.levels[n - 2].state.txid.clone(), self.levels[n - 2].state.out_value)
        }
    }
    /// All tier txs in unilateral-exit order: trigger, then (extension, state) for each level.
    pub fn exit_tiers(&self) -> Vec<&TesrTier> {
        let mut v = vec![&self.trigger];
        for l in &self.levels {
            v.push(&l.extension);
            v.push(&l.state);
        }
        v
    }
}

/// Establish a confirmed coin's TES-R ladder: build + blind-co-sign T → X_0 → S_0 over the funding
/// UTXO `F`. `coin` must be CONFIRMED with utxo/amount/aggregated_address populated.
pub async fn establish(
    cc: &ClientConfig,
    coin: &mut Coin,
    owner_exit_address: &str,
    csv_e: u16,
    csv_d: u16,
    fee_rate: f64,
    network: &str,
) -> Result<TesrBundle> {
    let statechain_id = coin.statechain_id.clone().ok_or_else(|| anyhow::anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or_else(|| anyhow::anyhow!("no utxo_txid"))?;
    let f_vout = coin.utxo_vout.ok_or_else(|| anyhow::anyhow!("no utxo_vout"))?;
    let f_value = coin.amount.ok_or_else(|| anyhow::anyhow!("no amount"))? as u64;
    let agg = coin.aggregated_address.clone().ok_or_else(|| anyhow::anyhow!("no aggregated_address"))?;

    let t = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &agg, network, fee_rate)?;
    let t_signed = cosign_tier(cc, coin, t.tx_hex.clone(), f_value, network).await?;
    let x = mercurylib::tesr::build_extension(&t.txid, t.out_value, &agg, network, csv_e, fee_rate)?;
    let x_signed = cosign_tier(cc, coin, x.tx_hex.clone(), t.out_value, network).await?;
    let s = mercurylib::tesr::build_state(&x.txid, x.out_value, owner_exit_address, network, csv_d, fee_rate)?;
    let s_signed = cosign_tier(cc, coin, s.tx_hex.clone(), x.out_value, network).await?;

    Ok(TesrBundle {
        version: TESR_BUNDLE_VERSION,
        statechain_id,
        network: network.to_string(),
        fee_rate,
        agg_address: agg,
        owner_exit_address: owner_exit_address.to_string(),
        f_txid,
        f_vout,
        f_value,
        trigger: TesrTier { txid: t.txid, signed_tx: t_signed, out_value: t.out_value, csv: None, payload_vout: t.payload_vout },
        levels: vec![TesrLevel {
            extension: TesrTier { txid: x.txid, signed_tx: x_signed, out_value: x.out_value, csv: Some(csv_e), payload_vout: x.payload_vout },
            state: TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(csv_d), payload_vout: s.payload_vout },
        }],
        m: 0,
        superseded_states: Vec::new(),
        superseded_extensions: Vec::new(),
        params: mercurylib::tesr::TesrParams::for_network(network),
    })
}

/// Establish a coin's ladder using its network's canonical [`TesrParams`] schedule (initial extension
/// `E0` and state `D0`), instead of hand-picked CSVs — the production entry point.
pub async fn establish_auto(
    cc: &ClientConfig,
    coin: &mut Coin,
    owner_exit_address: &str,
    network: &str,
) -> Result<TesrBundle> {
    let p = mercurylib::tesr::TesrParams::for_network(network);
    establish(cc, coin, owner_exit_address, p.ext_csv(0), p.state_csv(0), p.committed_fee_rate, network).await
}

/// [in-ladder split] A split child's headless ladder: extension spends `SP.out[j]`, state spends the
/// extension's `out[0]` and pays the owner. No trigger — `SP` is un-broadcast, so the child's clock
/// only starts once `SP` confirms on a unilateral exit. Both tiers are co-signed under the CHILD's
/// aggregate (`child_coin.aggregated_address` == `SP.out[j]`'s scriptPubKey).
pub struct ChildLadder {
    pub extension: TesrTier,
    pub state: TesrTier,
}

/// [in-ladder split] Co-sign a split child's headless ladder (extension + owner state) rooting at
/// `SP.out[sp_vout]`. The child coin is a fresh statechain node whose aggregate is `SP.out[j]`'s key;
/// this is the child-segment analogue of [`establish`] with no trigger. Persist/convey afterwards.
pub async fn establish_child(
    cc: &ClientConfig,
    child_coin: &mut Coin,
    sp_txid: &str,
    sp_vout: u32,
    sp_out_value: u64,
    owner_exit_address: &str,
    csv_e: u16,
    csv_d: u16,
    fee_rate: f64,
    network: &str,
) -> Result<ChildLadder> {
    let agg = child_coin
        .aggregated_address
        .clone()
        .ok_or_else(|| anyhow::anyhow!("child coin has no aggregated_address"))?;

    // Extension roots at the assigned SP output, under the child's aggregate.
    let x = mercurylib::tesr::build_extension_from(sp_txid, sp_vout, sp_out_value, &agg, network, csv_e, fee_rate)?;
    let x_signed = cosign_tier(cc, child_coin, x.tx_hex.clone(), sp_out_value, network).await?;

    // Owner state spends the extension's PAYLOAD output, pays the owner-exit key.
    let s = mercurylib::tesr::build_state_from(
        &x.txid, x.payload_vout, x.out_value, owner_exit_address, network, csv_d, fee_rate,
    )?;
    let s_signed = cosign_tier(cc, child_coin, s.tx_hex.clone(), x.out_value, network).await?;

    Ok(ChildLadder {
        extension: TesrTier { txid: x.txid, signed_tx: x_signed, out_value: x.out_value, csv: Some(csv_e), payload_vout: x.payload_vout },
        state: TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(csv_d), payload_vout: s.payload_vout },
    })
}

/// [in-ladder split] The production sender for an in-ladder split (B1 fix, PROTOCOL.md §5.4). Builds
/// `SP` — a STATE tier spending `X_m.out[0]` (a DESCENDANT of the trigger, NOT a rival for `F`), paying
/// each child statechain coin's aggregate — terminalizes the parent (budget 1, consumed by `SP`),
/// co-signs `SP` under `A_parent`, discloses the old owner state `S_0` as superseded (out-raced by `SP`
/// one rung lower), and establishes each child's headless ladder off `SP.out[j]` paying its recipient.
/// Returns one [`ChildTesrBundle`] per child for conveyance; the receiver checks each with
/// [`verify_child_bundle`]. The child coins must already be SE-registered (their aggregate is what `SP`
/// pays); `children` is `(child_coin, recipient_owner_exit_address, value_sats)` and is mutated as each
/// child ladder is co-signed. Value is conserved: `Σ value == tier_out_total(X_m.out[0], N)`.
pub async fn in_ladder_split(
    cc: &ClientConfig,
    wallet_name: &str,
    parent_coin: &mut Coin,
    bundle: &TesrBundle,
    children: &mut [(Coin, String, u64)],
) -> Result<Vec<ChildTesrBundle>> {
    let p = bundle.params;
    let x_m = bundle.current().extension.clone();
    let s0_csv = bundle
        .current()
        .state
        .csv
        .ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;
    // SP must OUT-RACE S_0 over X_m.out[0]: one rung lower, floored (consumes a state rung).
    let sp_csv = s0_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| anyhow::anyhow!("state CSV at the floor — renew/rollover before splitting"))?;

    let n = children.len();
    if n == 0 {
        return Err(anyhow::anyhow!("in-ladder split needs at least one child"));
    }
    // Value conservation — no mint, no burn (build_split_state re-checks, but fail early with context).
    let total = mercurylib::tesr::tier_out_total(x_m.out_value, n, bundle.fee_rate)
        .ok_or_else(|| anyhow::anyhow!("committed fee too high for {n} children"))?;
    let sum: u64 = children.iter().map(|(_, _, v)| *v).sum();
    if sum != total {
        return Err(anyhow::anyhow!(
            "child values sum to {sum} but must equal {total} (= X_m.out[0] − committed fee)"
        ));
    }

    // SP pays each child's aggregate address, in order (SP.out[j] == children[j]).
    let payees: Vec<(String, u64)> = children
        .iter()
        .map(|(c, _, v)| {
            c.aggregated_address
                .clone()
                .ok_or_else(|| anyhow::anyhow!("child coin has no aggregated_address"))
                .map(|a| (a, *v))
        })
        .collect::<Result<_>>()?;
    let sp = mercurylib::tesr::build_split_state(
        &x_m.txid, x_m.out_value, &payees, &bundle.network, sp_csv, bundle.fee_rate,
    )?;

    // Terminalize the parent (SP consumes the last budget slot) and co-sign SP under A_parent.
    let parent_sid = parent_coin
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("parent coin has no statechain_id"))?;
    crate::lightning_latch::set_spend_budget(cc, wallet_name, &parent_sid, 1).await?;
    let sp_signed = cosign_tier(cc, parent_coin, sp.tx_hex.clone(), x_m.out_value, &bundle.network).await?;

    // Parent segment shared by every child bundle: SP is the current (terminal) state; S_0 superseded.
    let mut parent_seg = bundle.clone();
    let last = parent_seg.levels.len() - 1;
    parent_seg.superseded_states.push(parent_seg.levels[last].state.clone());
    parent_seg.levels[last].state = TesrTier {
        txid: sp.txid.clone(),
        signed_tx: sp_signed,
        out_value: total,
        csv: Some(sp_csv),
        payload_vout: sp.payload_vout,
    };

    // Each child: headless ladder off SP.out[j], paying its recipient (Model A).
    let mut bundles = Vec::with_capacity(n);
    for (j, (child_coin, recipient, value)) in children.iter_mut().enumerate() {
        // Child `j` lives at SP's j-th PAYLOAD output, not positional `j` (a coloured SP carries the
        // opret at index 0 and shifts every child by one).
        let child_vout = sp.payload_vout + j as u32;
        let ladder = establish_child(
            cc, child_coin, &sp.txid, child_vout, *value, recipient,
            p.ext_csv(0), p.state_csv(0), bundle.fee_rate, &bundle.network,
        )
        .await?;
        let child_sid = child_coin
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("child coin has no statechain_id"))?;
        // [F1] The child is deliberately left NON-terminal, so the receiver can complete the standard
        // key handover and hold a first-class, re-transferable coin (CHILDREN.md). A child
        // is not defenceless without terminality — two mechanisms cover the two windows:
        //   * PRE-conveyance: a rival the sender co-signed over SP.out[j] before conveying is caught by
        //     the exact-equality census (`child_num_sigs == CHILD_V2_BASELINE + tiers + superseded`),
        //     which the receiver checks against the SE's authoritative count.
        //   * POST-conveyance: `convey_child_bundle`'s `get_new_x1` opens a transfer on the child, and
        //     the coordinator's pending-transfer lock then refuses every further sender co-sign until
        //     the transfer completes — at which point the receiver's auth rotation makes the lockout
        //     PERMANENT. (The temporary lock is what bridges census → key_updated.)
        // The LATCHED lane is the exception and re-applies terminality in `in_ladder_pay`: there the
        // piece sits unclaimed until an LN preimage lands, i.e. deliberately past the lock's window.
        bundles.push(ChildTesrBundle {
            parent: parent_seg.clone(),
            parent_statechain_id: parent_sid.clone(),
            sp_vout: child_vout,
            child_statechain_id: child_sid,
            child_owner_exit_address: recipient.clone(),
            child_extension: ladder.extension,
            child_state: ladder.state,
            child_superseded_states: vec![],
            child_superseded_extensions: vec![],
            ancestors: vec![],
        });
    }
    Ok(bundles)
}

/// Off-chain renewal at the schedule cadence: the new extension takes CSV `E0 − (m+1)·δE` and the
/// fresh state `D0`, both from the bundle's [`TesrParams`]. Returns `true` if a rollover is due
/// afterwards (`m` reached `m_max`), so the caller rolls over before the extension floor.
pub async fn renew_auto(cc: &ClientConfig, coin: &mut Coin, bundle: &mut TesrBundle) -> Result<bool> {
    let p = bundle.params;
    let next_m = (bundle.m + 1) as u16;
    renew(cc, coin, bundle, p.ext_csv(next_m), p.state_csv(0)).await?;
    Ok(p.needs_rollover(bundle.m as u16))
}

/// Off-chain rollover at the schedule cadence: a fresh level with extension `E0` and state `D0`.
pub async fn rollover_auto(cc: &ClientConfig, coin: &mut Coin, bundle: &mut TesrBundle) -> Result<()> {
    let p = bundle.params;
    rollover(cc, coin, bundle, p.ext_csv(0), p.state_csv(0)).await
}

/// Persist a bundle under `tesr-<statechain_id>` (replaces any prior bundle for the coin).
pub async fn persist(cc: &ClientConfig, wallet_name: &str, bundle: &TesrBundle) -> Result<()> {
    let json = serde_json::to_string(bundle)?;
    crate::sqlite_manager::insert_raw_backup_txs(
        &cc.pool,
        wallet_name,
        &format!("tesr-{}", bundle.statechain_id),
        &json,
    )
    .await
}

/// Load a coin's persisted TES-R bundle from the wallet DB, if any.
pub async fn load(cc: &ClientConfig, wallet_name: &str, statechain_id: &str) -> Result<Option<TesrBundle>> {
    let key = format!("tesr-{statechain_id}");
    for (k, json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if k == key {
            return Ok(Some(serde_json::from_str(&json)?));
        }
    }
    Ok(None)
}

/// Flat-backup baseline of a natively-laddered **on-chain PARENT** coin. `sig_count` starts at 0
/// (`generated_public_key DEFAULT 0`); the coin's on-chain deposit confirmation co-signs exactly ONE
/// signed-once backup tx (`coin_status::check_deposit` → `create_tx1` for a non-single-use coin) before
/// the ladder is established. So such a parent has `num_sigs == 1 + <established tiers>`. A split-child
/// receiver — who cannot observe the parent's pre-establish history — relies on this constant to run
/// `verify_child_bundle`'s exact-equality census independently of the sender.
pub const PARENT_V2_BASELINE: u32 = 1;

/// Flat-backup baseline of a split **CHILD** slot: it is an SE-registered key that is NEVER funded
/// on-chain (its funding is the un-broadcast `SP.out[j]`), so `check_deposit`/`create_tx1` never runs
/// for it and `num_sigs` counts ONLY the two child tiers co-signed at split time. Baseline `0`.
pub const CHILD_V2_BASELINE: u32 = 0;

/// Persist a split child bundle under `ctesr-<child_statechain_id>` (replaces any prior).
pub async fn persist_child(cc: &ClientConfig, wallet_name: &str, cb: &ChildTesrBundle) -> Result<()> {
    let json = serde_json::to_string(cb)?;
    crate::sqlite_manager::insert_raw_backup_txs(
        &cc.pool,
        wallet_name,
        &format!("ctesr-{}", cb.child_statechain_id),
        &json,
    )
    .await
}

/// Load a coin's persisted split child bundle from the wallet DB, if any.
pub async fn load_child(cc: &ClientConfig, wallet_name: &str, child_statechain_id: &str) -> Result<Option<ChildTesrBundle>> {
    let key = format!("ctesr-{child_statechain_id}");
    for (k, json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if k == key {
            return Ok(Some(serde_json::from_str(&json)?));
        }
    }
    Ok(None)
}

/// Statechain ids of all coins backed by a RECEIVED in-ladder split child bundle (a persisted
/// `ctesr-<id>`). These are FIRST-CLASS coins — the claim completed the SE key handover, so they can be
/// paid onward off-chain via [`child_retransfer`] — but their funding `SP.out[j]` is un-broadcast, so
/// callers use this set to treat them specially: spendable only WHOLE (never chosen as the coin to
/// split), and withdrawn by unilateral exit rather than a cooperative on-chain withdrawal.
/// One wallet-DB read.
pub async fn child_claim_sids(cc: &ClientConfig, wallet_name: &str) -> Result<std::collections::HashSet<String>> {
    let mut set = std::collections::HashSet::new();
    for (k, _json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if let Some(sid) = k.strip_prefix("ctesr-") {
            set.insert(sid.to_string());
        }
    }
    Ok(set)
}

/// The full unilateral-exit chain of a split child, in broadcast order:
/// `T -> X_m -> SP` (parent segment) then `ext_child -> state_child`. Each entry is
/// `(signed_tx_hex, relative_csv)` — the trigger has no CSV.
pub fn child_exit_chain(cb: &ChildTesrBundle) -> Vec<(String, Option<u16>)> {
    let mut chain: Vec<(String, Option<u16>)> =
        cb.parent.exit_tiers().iter().map(|t| (t.signed_tx.clone(), t.csv)).collect();
    // Splice EVERY intermediate segment, root→leaf, before the leaf's own tiers. Omitting these is
    // not a mere verification gap — the leaf's funding outpoint would never be created on-chain, so
    // the exit would stall forever and the value would be UNRECOVERABLE. Order is the broadcast
    // order: each segment's extension, then its state (which funds the next level down).
    for seg in cb.ancestors.iter() {
        chain.push((seg.extension.signed_tx.clone(), seg.extension.csv));
        chain.push((seg.state.signed_tx.clone(), seg.state.csv));
    }
    chain.push((cb.child_extension.signed_tx.clone(), cb.child_extension.csv));
    chain.push((cb.child_state.signed_tx.clone(), cb.child_state.csv));
    chain
}

/// **Owner-initiated unilateral exit of a split CHILD coin.** Broadcasts the child's full pre-co-signed
/// chain (`T -> X_m -> SP -> ext_child -> state_child`) in order, each tier once its relative-CSV is met,
/// stopping at the first not-yet-mature tier. Keyless (the receiver never co-signs — every tx is already
/// signed and `state_child` pays the receiver's own key). Idempotent: call once per block; already-known
/// tiers are skipped. Returns a typed [`ExitProgress`]; **`Err` = blind** (external review F4) — a
/// backend that could not be read, or stored child material that could not be decoded, is never
/// reported as an ordinary "nothing broadcast this pass, keep waiting".
pub fn exit_child_pass(electrum: &electrum_client::Client, cb: &ChildTesrBundle) -> Result<ExitProgress> {
    let mut broadcast = Vec::new();
    let mut stalled = None;
    for (signed, _csv) in child_exit_chain(cb) {
        let raw = hex::decode(&signed)
            .map_err(|e| anyhow::anyhow!("child exit chain carries unusable signed tx hex: {e}"))?;
        // Derive the txid to skip already-known tiers without re-broadcasting.
        let txid = {
            use electrum_client::bitcoin::{consensus::deserialize, Transaction};
            deserialize::<Transaction>(&raw)
                .map_err(|e| anyhow::anyhow!("child exit chain tx did not deserialize: {e}"))?
                .txid()
                .to_string()
        };
        if tx_known(electrum, &txid)? {
            continue;
        }
        match electrum.transaction_broadcast_raw(&raw) {
            Ok(_) => broadcast.push(txid),
            Err(e) => {
                // CSV not met / parent unconfirmed — retry next pass, but SAY SO.
                stalled = Some(format!("{txid}: {e}"));
                break;
            }
        }
    }
    let complete = tx_known(electrum, &cb.child_state.txid)?;
    Ok(ExitProgress { broadcast, complete, stalled })
}

/// The relative-CSV of the first child-exit tier not yet on-chain (a wait-time hint), or `Ok(None)`
/// once the child exit is complete. Mirrors [`next_exit_tier`] for a split child chain, including
/// its fail-closed contract: **`Err` = blind**, never a fabricated wait time.
pub fn next_child_exit_tier(electrum: &electrum_client::Client, cb: &ChildTesrBundle) -> Result<Option<u16>> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    for (signed, csv) in child_exit_chain(cb) {
        let raw = hex::decode(&signed)
            .map_err(|e| anyhow::anyhow!("child exit chain carries unusable signed tx hex: {e}"))?;
        let txid = deserialize::<Transaction>(&raw)
            .map_err(|e| anyhow::anyhow!("child exit chain tx did not deserialize: {e}"))?
            .txid()
            .to_string();
        if !tx_known(electrum, &txid)? {
            return Ok(Some(csv.unwrap_or(0)));
        }
    }
    Ok(None)
}

/// Fetch the authoritative inputs a split-child receiver needs and run [`verify_child_bundle`] — the
/// verify-ONLY core (no persistence). Reads `F.spk` from chain, the parent+child
/// `num_sigs`/`aggregate_pubkey` from `/info/statechain`, and both terminality flags from
/// `/statechain/spend_budget` (fail-closed), and checks the child pays `receiver_backup_address`
/// (Model A). Returns the child's exit value on success. Used by claim()'s validation pass.
pub async fn verify_conveyed_child(
    cc: &ClientConfig,
    receiver_backup_address: &str,
    cb: &ChildTesrBundle,
) -> Result<u64> {
    use electrum_client::bitcoin::Txid;
    // F.spk from chain (also proves F is known to the chain; unspent/confirmed is enforced by the
    // terminality of the parent — a terminal parent's only live spend is the disclosed T).
    let f_txid = Txid::from_str(&cb.parent.f_txid)
        .map_err(|_| anyhow::anyhow!("bad parent F txid"))?;
    let f_tx = cc
        .electrum_client
        .transaction_get(&f_txid)
        .map_err(|_| anyhow::anyhow!("parent F {} not found on chain", cb.parent.f_txid))?;
    let f_out = f_tx
        .output
        .get(cb.parent.f_vout as usize)
        .ok_or_else(|| anyhow::anyhow!("parent F has no output {}", cb.parent.f_vout))?;
    let f_spk_hex = hex::encode(f_out.script_pubkey.as_bytes());

    let p_info = crate::utils::get_statechain_info(&cb.parent_statechain_id, cc)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no statechain info for parent sid"))?;
    let c_info = crate::utils::get_statechain_info(&cb.child_statechain_id, cc)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no statechain info for child sid"))?;

    // Only the PARENT's terminality is fetched: the child's census is made durable by the handover the
    // receiver completes in this same claim, not by terminality (see verify_child_bundle's [F2]).
    let (_, _, parent_terminal) =
        crate::lightning_latch::get_spend_budget(cc, &cb.parent_statechain_id).await?;

    // Each INTERMEDIATE segment is an ancestor (not handed over here), so it must be terminal and its
    // census must balance — fetch the SE's authoritative facts for each, in root→leaf order.
    let mut ancestor_facts: Vec<AncestorFacts> = Vec::with_capacity(cb.ancestors.len());
    for seg in cb.ancestors.iter() {
        let info = crate::utils::get_statechain_info(&seg.statechain_id, cc)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no statechain info for ancestor {}", seg.statechain_id))?;
        let (_, _, terminal) = crate::lightning_latch::get_spend_budget(cc, &seg.statechain_id).await?;
        ancestor_facts.push(AncestorFacts {
            num_sigs: info.num_sigs,
            aggregate_pubkey: info.aggregate_pubkey.clone(),
            terminal,
        });
    }

    verify_child_bundle(
        cb,
        &f_spk_hex,
        p_info.num_sigs,
        PARENT_V2_BASELINE,
        p_info.aggregate_pubkey.as_deref(),
        parent_terminal,
        c_info.num_sigs,
        CHILD_V2_BASELINE,
        c_info.aggregate_pubkey.as_deref(),
        &ancestor_facts,
        receiver_backup_address,
    )?;
    Ok(cb.child_state.out_value)
}

/// [`verify_conveyed_child`] + persist the bundle so the receiver can [`exit_child_pass`] it. The
/// RECEIVER side of an in-ladder split payment (the analogue of adopting a conveyed `tesr_ladder`).
pub async fn adopt_child_bundle(
    cc: &ClientConfig,
    wallet_name: &str,
    receiver_backup_address: &str,
    cb: &ChildTesrBundle,
) -> Result<u64> {
    let value = verify_conveyed_child(cc, receiver_backup_address, cb).await?;
    persist_child(cc, wallet_name, cb).await?;
    Ok(value)
}

/// CHILD-LEVEL IN-LADDER SPLIT — pay a NON-EXACT amount out of a received child.
///
/// The child analogue of `in_ladder_split`. The child's own state is replaced by a SPLIT state `CSP`
/// (one δ lower, so it out-races the state it replaces over `ext_child.out[0]`) paying N grandchildren;
/// each grandchild then gets its own headless ladder off `CSP.out[j]`. The child itself becomes an
/// INTERMEDIATE segment in each grandchild's bundle: it is terminalized (it is an ancestor now, not a
/// coin being handed over), and the receiver's `verify_child_bundle` walks it via `cb.ancestors`.
///
/// Returns one bundle per grandchild, in `children` order.
pub async fn child_in_ladder_split(
    cc: &ClientConfig,
    wallet_name: &str,
    child_coin: &mut Coin,
    cb: &ChildTesrBundle,
    children: &mut [(Coin, String, u64)],
) -> Result<Vec<ChildTesrBundle>> {
    let p = cb.parent.params;
    let n = children.len();
    if n == 0 {
        return Err(anyhow::anyhow!("a child split needs at least one grandchild"));
    }
    let old_csv = cb
        .child_state
        .csv
        .ok_or_else(|| anyhow::anyhow!("child state has no CSV — cannot split"))?;
    // CSP must OUT-RACE the state it replaces over ext_child.out[0]: one rung lower, floored.
    let csp_csv = old_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| anyhow::anyhow!(
            "child state CSV {old_csv} is at the floor ({}) — exit or re-anchor instead of splitting",
            p.d_floor
        ))?;

    // Value conservation: the grandchildren share the split total exactly (no mint, no burn).
    let total = mercurylib::tesr::tier_out_total(cb.child_extension.out_value, n, cb.parent.fee_rate)
        .ok_or_else(|| anyhow::anyhow!("committed fee too high to split this child into {n}"))?;
    let sum: u64 = children.iter().map(|(_, _, v)| *v).sum();
    if sum != total {
        return Err(anyhow::anyhow!(
            "grandchild values sum to {sum} but must equal {total} (= ext_child.out[0] − committed fee)"
        ));
    }

    // CSP pays each grandchild's aggregate address, in order (CSP.out[j] == children[j]).
    let payees: Vec<(String, u64)> = children
        .iter()
        .map(|(c, _, v)| {
            c.aggregated_address
                .clone()
                .ok_or_else(|| anyhow::anyhow!("grandchild coin has no aggregated_address"))
                .map(|a| (a, *v))
        })
        .collect::<Result<_>>()?;
    let csp = mercurylib::tesr::build_split_state(
        &cb.child_extension.txid,
        cb.child_extension.out_value,
        &payees,
        &cb.parent.network,
        csp_csv,
        cb.parent.fee_rate,
    )?;

    // Terminalize the CHILD (it becomes an ancestor segment) and co-sign CSP under A_child.
    let child_sid = child_coin
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("child coin has no statechain_id"))?;
    crate::lightning_latch::set_spend_budget(cc, wallet_name, &child_sid, 1).await?;
    let csp_signed = cosign_tier(
        cc,
        child_coin,
        csp.tx_hex.clone(),
        cb.child_extension.out_value,
        &cb.parent.network,
    )
    .await?;

    // The child segment as the grandchildren will see it: CSP is its current state, and the state CSP
    // replaced is disclosed as superseded (it loses the race for ext_child.out[0]).
    let mut seg_superseded = cb.child_superseded_states.clone();
    seg_superseded.push(cb.child_state.clone());
    let child_segment = ChildSegment {
        statechain_id: child_sid.clone(),
        funding_vout: cb.sp_vout,
        extension: cb.child_extension.clone(),
        state: TesrTier {
            txid: csp.txid.clone(),
            signed_tx: csp_signed,
            out_value: total,
            csv: Some(csp_csv),
            payload_vout: csp.payload_vout,
        },
        superseded_states: seg_superseded,
        superseded_extensions: cb.child_superseded_extensions.clone(),
    };

    let mut bundles = Vec::with_capacity(n);
    for (j, (gc_coin, recipient, value)) in children.iter_mut().enumerate() {
        let gc_vout = csp.payload_vout + j as u32;
        let ladder = establish_child(
            cc, gc_coin, &csp.txid, gc_vout, *value, recipient,
            p.ext_csv(0), p.state_csv(0), cb.parent.fee_rate, &cb.parent.network,
        )
        .await?;
        let gc_sid = gc_coin
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("grandchild coin has no statechain_id"))?;
        // Grandchildren are left NON-terminal for the same reason children are: the receiver completes
        // the key handover and takes first-class ownership (see in_ladder_split's [F1]).
        let mut ancestors = cb.ancestors.clone();
        ancestors.push(child_segment.clone());
        bundles.push(ChildTesrBundle {
            parent: cb.parent.clone(),
            parent_statechain_id: cb.parent_statechain_id.clone(),
            sp_vout: gc_vout,
            child_statechain_id: gc_sid,
            child_owner_exit_address: recipient.clone(),
            child_extension: ladder.extension,
            child_state: ladder.state,
            child_superseded_states: vec![],
            child_superseded_extensions: vec![],
            ancestors,
        });
    }
    Ok(bundles)
}

/// ONWARD HOP — re-transfer a whole received CHILD off-chain to a new owner (Spark parity).
///
/// A received child cannot go through `transfer_sender::execute`: it has no `tesr-` bundle (only
/// `ctesr-`), so that path would fall through to the B1-unsafe plain split, and it has no un-laddered
/// backup chain to hand over. This is the child's own Model-A transfer instead:
///
///   * build a NEW child state over `ext_child.out[0]` one δ lower (replace-by-lower-timelock), paying
///     the NEW recipient's exit key — so the fresh state matures BEFORE the one it replaces and wins
///     the race for that outpoint;
///   * co-sign it under `A_child` — possible only because the receiver completed the key handover when
///     it adopted the child (Commit A); the sender of THIS hop is the current owner;
///   * disclose the state it replaced in `child_superseded_states` (full-disclosure counting), which the
///     receiver's census then counts and proves non-confirmable;
///   * convey the updated bundle with the standard handover, exactly like the first hop.
///
/// Net effect per hop: EXACTLY +1 `num_sigs` and +1 superseded entry, which is what
/// `verify_child_bundle`'s child census expects (`baseline + 2 + superseded`).
pub async fn child_retransfer(
    cc: &ClientConfig,
    wallet_name: &str,
    child_coin: &mut Coin,
    cb: &ChildTesrBundle,
    recipient_address: &str,
) -> Result<ChildTesrBundle> {
    let p = cb.parent.params;
    // Replace-by-lower-timelock: the new state must mature strictly before the one it supersedes, and
    // must not sink below the schedule floor (at the floor the coin must be re-anchored, not re-sent).
    let old_csv = cb
        .child_state
        .csv
        .ok_or_else(|| anyhow::anyhow!("child state has no CSV — cannot re-transfer"))?;
    let new_csv = old_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| anyhow::anyhow!(
            "child state CSV {old_csv} is at the floor ({}) — exit or re-anchor it instead of re-sending",
            p.d_floor
        ))?;

    // The new state spends the SAME outpoint as the one it replaces: ext_child.out[0].
    let payee = mercurylib::tesr::payee_address(recipient_address, &cb.parent.network)?;
    let st = mercurylib::tesr::build_state_from(
        &cb.child_extension.txid,
        cb.child_extension.payload_vout,
        cb.child_extension.out_value,
        &payee,
        &cb.parent.network,
        new_csv,
        cb.parent.fee_rate,
    )?;
    let signed = cosign_tier(
        cc,
        child_coin,
        st.tx_hex.clone(),
        cb.child_extension.out_value,
        &cb.parent.network,
    )
    .await?;

    let mut next = cb.clone();
    // Full disclosure: the state we just replaced is now superseded (it loses the race for
    // ext_child.out[0] to the lower-CSV state above) and must be counted by the receiver's census.
    next.child_superseded_states.push(next.child_state.clone());
    next.child_state = TesrTier {
        txid: st.txid.clone(),
        signed_tx: signed,
        out_value: st.out_value,
        csv: Some(new_csv),
        payload_vout: st.payload_vout,
    };
    next.child_owner_exit_address = payee;

    convey_child_bundle(cc, recipient_address, child_coin, &next, None).await?;
    // Book the hop locally: the child has left this wallet.
    persist_child(cc, wallet_name, &next).await?;
    Ok(next)
}

/// Conveys a split **child** bundle to `recipient_address` by posting an encrypted mailbox message
/// (`child_tesr_bundle` set, `protocol_version = 4`) via `transfer/update_msg`, together with the
/// STANDARD key-handover material (`transfer_signature` + blinded `t1`). The `child_coin` is the
/// sender-owned piece slot whose `signed_statechain_id` authorises the post. The receiver picks it up
/// in claim(), runs [`verify_child_bundle`] and then COMPLETES the handover, so the child becomes a
/// first-class coin and the sender is locked out (`docs/utexo/CHILDREN.md`).
pub async fn convey_child_bundle(
    cc: &ClientConfig,
    recipient_address: &str,
    child_coin: &Coin,
    cb: &ChildTesrBundle,
    batch_id: Option<String>,
) -> Result<()> {
    let statechain_id = child_coin
        .statechain_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("piece child coin has no statechain_id"))?;
    let signed_statechain_id = child_coin
        .signed_statechain_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("piece child coin has no signed_statechain_id"))?;
    let (_, _, recipient_auth_pubkey) =
        mercurylib::decode_transfer_address(recipient_address)?;
    // The bundle must describe the very slot we are conveying, or the receiver would census one coin
    // and complete the handover on another.
    if cb.child_statechain_id != *statechain_id {
        return Err(anyhow::anyhow!(
            "child bundle statechain id {} does not match the conveyed slot {statechain_id}",
            cb.child_statechain_id
        ));
    }

    // The child's funding outpoint is `SP.out[sp_vout]` of the UN-BROADCAST split state — the child
    // slot's own `utxo_txid`/`utxo_vout` are None (a derived slot is never funded on-chain), so derive
    // it from the bundle. This is the outpoint the transfer signature commits to, and the same one the
    // receiver reconstructs when it validates.
    let sp_txid = {
        use bitcoin::consensus::encode::deserialize;
        // The child's funding tx is the segment DIRECTLY above it: the parent's SP for a depth-1
        // child, else the deepest intermediate segment's state. This must match exactly what the
        // receiver reconstructs when it verifies the transfer signature.
        let funding_hex = cb
            .ancestors
            .last()
            .map(|a| a.state.signed_tx.clone())
            .unwrap_or_else(|| cb.parent.current().state.signed_tx.clone());
        let sp: bitcoin::Transaction = deserialize(&hex::decode(&funding_hex)?)?;
        sp.txid().to_string()
    };

    // Sender-side binding, built BEFORE the transfer is opened: once `get_new_x1` runs, the coordinator's
    // pending-transfer lock refuses further co-signs on this slot, so every sender-side signature must
    // already exist (the same discipline `transfer_sender::execute` follows).
    let transfer_signature = mercurylib::transfer::sender::create_transfer_signature(
        recipient_address,
        &sp_txid,
        cb.sp_vout,
        &child_coin.user_privkey,
    )?;

    // `transfer/update_msg` is an UPDATE keyed on (statechain_id, new_user_auth_key); the row is
    // created by this `transfer/sender` init call — without it the update_msg silently no-ops (0 rows)
    // and the receiver never sees the message. It also returns `x1`, the SE's blinding factor for the
    // key handover, and ARMS the coordinator's pending-transfer lock on this slot (which is what closes
    // the post-conveyance rival window for the child).
    //
    // [non-exact LN latch, LIGHTNING.md Step 1] `batch_id = Some` makes the child mailbox row born
    // batch-locked: `insert_new_transfer` sets locked=true and locked2=is_lightning_latch, so the
    // receiver (SSP) must not adopt the piece until the LN preimage flips locked2 via
    // `unlock_by_preimage`. The piece sid must already carry an external-hash latch (registered by the
    // latched `in_ladder_pay`) for the server to mark the row a lightning latch.
    let x1 = crate::transfer_sender::get_new_x1(
        cc,
        statechain_id,
        signed_statechain_id,
        &recipient_auth_pubkey.to_string(),
        batch_id,
    )
    .await?;

    let json = serde_json::to_string(cb)?;
    let payload = mercurylib::transfer::sender::create_child_conveyance_update_msg(
        &x1,
        recipient_address,
        child_coin,
        &transfer_signature,
        &json,
    )?;
    let endpoint = cc.statechain_entity.clone();
    let client = cc.get_reqwest_client()?;
    let status = client
        .post(&format!("{}/transfer/update_msg", endpoint))
        .json(&payload)
        .send()
        .await?
        .status();
    if !status.is_success() {
        return Err(anyhow::anyhow!("failed to convey child bundle (update_msg {status})"));
    }
    Ok(())
}

/// Off-chain RENEWAL: co-sign a new extension `X_{m+1}` with a strictly LOWER CSV
/// (Decker-Wattenhofer replace-by-lower-timelock) plus a fresh state `S'_0`, replacing the current
/// tiers in `bundle`. Zero on-chain bytes; once the trigger is broadcast, the superseded extension
/// can never win the race for `T.out[0]`. Persist the updated bundle afterwards.
pub async fn renew(
    cc: &ClientConfig,
    coin: &mut Coin,
    bundle: &mut TesrBundle,
    csv_e_new: u16,
    csv_d: u16,
) -> Result<()> {
    let (parent_txid, parent_val) = bundle.current_parent();
    let x = mercurylib::tesr::build_extension(&parent_txid, parent_val, &bundle.agg_address, &bundle.network, csv_e_new, bundle.fee_rate)?;
    let x_signed = cosign_tier(cc, coin, x.tx_hex.clone(), parent_val, &bundle.network).await?;
    let s = mercurylib::tesr::build_state(&x.txid, x.out_value, &bundle.owner_exit_address, &bundle.network, csv_d, bundle.fee_rate)?;
    let s_signed = cosign_tier(cc, coin, s.tx_hex.clone(), x.out_value, &bundle.network).await?;

    let last = bundle.levels.len() - 1;
    // Full-disclosure: the replaced extension + state were co-signed; keep them so verify_bundle counts them.
    bundle.superseded_extensions.push(bundle.levels[last].extension.clone());
    bundle.superseded_states.push(bundle.levels[last].state.clone());
    bundle.levels[last] = TesrLevel {
        extension: TesrTier { txid: x.txid, signed_tx: x_signed, out_value: x.out_value, csv: Some(csv_e_new), payload_vout: x.payload_vout },
        state: TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(csv_d), payload_vout: s.payload_vout },
    };
    bundle.m += 1;
    Ok(())
}

/// Off-chain ROLLOVER at epoch exhaustion: instead of an on-chain compaction, convert the current
/// level's state into a SELF-SPLIT paying `A` and hang a FRESH level (extension + owner state) off it
/// — a new renewal budget, unbounded off-chain depth, ZERO on-chain bytes. Persist afterwards. The
/// only cost is +2 pre-signed txs (~248 vB) of *contingent* exit weight and +1 unilateral-exit level.
pub async fn rollover(
    cc: &ClientConfig,
    coin: &mut Coin,
    bundle: &mut TesrBundle,
    csv_e: u16,
    csv_d: u16,
) -> Result<()> {
    let p = bundle.params;
    let cur_ext = bundle.current().extension.clone();
    let cur_state_csv = bundle
        .current()
        .state
        .csv
        .ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;

    // 1. Re-sign the current level's state as a self-split paying A (so a child level can hang off it).
    //
    // [S3] The self-split spends the SAME prevout as the current owner-paying state (both spend X_L:0),
    // so it is a RIVAL of that state, not an independent tier. It can only supersede it by maturing
    // FIRST — i.e. at a strictly LOWER CSV — exactly as presign_receiver_state does for S'. Building it
    // at the current (undecremented) CSV made it an EQUAL-CSV twin of the retained old state, which
    // verify_bundle's per-prevout race check rejects (`(csv as u32) <= live_csv`), so every rolled-over
    // coin failed verification. It went unnoticed because sdk43 never called verify_bundle. Since
    // rollover is mandatory at m_max, that was the terminal state of every long-lived coin.
    //
    // Consequence: rollover now CONSUMES one state rung (see PROTOCOL.md footprint note). If the state
    // CSV is already at the floor, a self-split rollover is impossible and the coin must exit or
    // on-chain re-anchor.
    let roll_csv = cur_state_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| anyhow::anyhow!(
            "state CSV at the floor — cannot self-split rollover; exit or on-chain re-anchor"
        ))?;
    let s_roll = mercurylib::tesr::build_state(&cur_ext.txid, cur_ext.out_value, &bundle.agg_address, &bundle.network, roll_csv, bundle.fee_rate)?;
    let s_roll_signed = cosign_tier(cc, coin, s_roll.tx_hex.clone(), cur_ext.out_value, &bundle.network).await?;

    // 2. Fresh level off the self-split output: new extension + owner-exit state.
    let x2 = mercurylib::tesr::build_extension(&s_roll.txid, s_roll.out_value, &bundle.agg_address, &bundle.network, csv_e, bundle.fee_rate)?;
    let x2_signed = cosign_tier(cc, coin, x2.tx_hex.clone(), s_roll.out_value, &bundle.network).await?;
    let s2 = mercurylib::tesr::build_state(&x2.txid, x2.out_value, &bundle.owner_exit_address, &bundle.network, csv_d, bundle.fee_rate)?;
    let s2_signed = cosign_tier(cc, coin, s2.tx_hex.clone(), x2.out_value, &bundle.network).await?;

    let last = bundle.levels.len() - 1;
    // Full-disclosure: the old owner-paying state is replaced by the self-split; keep it for counting.
    bundle.superseded_states.push(bundle.levels[last].state.clone());
    bundle.levels[last].state = TesrTier { txid: s_roll.txid, signed_tx: s_roll_signed, out_value: s_roll.out_value, csv: Some(roll_csv), payload_vout: s_roll.payload_vout };
    bundle.levels.push(TesrLevel {
        extension: TesrTier { txid: x2.txid, signed_tx: x2_signed, out_value: x2.out_value, csv: Some(csv_e), payload_vout: x2.payload_vout },
        state: TesrTier { txid: s2.txid, signed_tx: s2_signed, out_value: s2.out_value, csv: Some(csv_d), payload_vout: s2.payload_vout },
    });
    bundle.m = 0; // fresh renewal budget at the new level
    Ok(())
}

/// Model A (history/MIGRATION.md §"receiver ladder adoption"): while still owning the coin, pre-sign the
/// RECEIVER-paying state `S'` so the receiver gets a complete, verifiable exit chain paying THEM.
/// `S'` spends the deepest extension's output, pays the recipient's derived P2TR (from a Mercury
/// transfer address), and carries CSV `= current_state_csv − δ` (one lower, so it matures before the
/// sender's retained state). Returns the augmented bundle to convey (final state = `S'`,
/// `owner_exit_address` = the recipient's key). Co-signs on a CLONE so the caller's coin is untouched
/// for the rest of the transfer. Errors if the state CSV is at the floor (renew/rollover first).
pub async fn presign_receiver_state(
    cc: &ClientConfig,
    coin: &Coin,
    bundle: &TesrBundle,
    recipient_address: &str,
) -> Result<TesrBundle> {
    let p = bundle.params;
    let cur_csv = bundle.current().state.csv.ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;
    let new_csv = cur_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| anyhow::anyhow!("state CSV at the floor — renew/rollover before transferring"))?;

    let payee = mercurylib::tesr::payee_address(recipient_address, &bundle.network)?;
    let ext = bundle.current().extension.clone();
    let s = mercurylib::tesr::build_state(&ext.txid, ext.out_value, &payee, &bundle.network, new_csv, bundle.fee_rate)?;

    let mut c = coin.clone();
    let s_signed = cosign_tier(cc, &mut c, s.tx_hex.clone(), ext.out_value, &bundle.network).await?;

    let mut b = bundle.clone();
    b.owner_exit_address = payee;
    let last = b.levels.len() - 1;
    // Full-disclosure: the sender's own (now stale) state was co-signed; keep it so verify_bundle
    // counts it — and it sits at a HIGHER CSV than S', so it loses the maturity race to the receiver.
    b.superseded_states.push(b.levels[last].state.clone());
    b.levels[last].state = TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(new_csv), payload_vout: s.payload_vout };
    Ok(b)
}

/// Cooperative DE-TRIGGER: blind-co-sign a fresh no-timelock spend of `T.out[0]` paying `to_address`,
/// returning the signed tx. Broadcasting it in response to a hostile trigger confirms before any
/// pre-signed extension can mature, collapsing griefing to a priced nuisance and killing the ladder.
pub async fn cosign_detrigger(
    cc: &ClientConfig,
    coin: &mut Coin,
    bundle: &TesrBundle,
    to_address: &str,
) -> Result<String> {
    let de = mercurylib::tesr::build_detrigger(&bundle.trigger.txid, bundle.trigger.out_value, to_address, &bundle.network, bundle.fee_rate)?;
    cosign_tier(cc, coin, de.tx_hex.clone(), bundle.trigger.out_value, &bundle.network).await
}

/// **Chain-visibility vocabulary (external review F4).**
///
/// Every watch/exit pass in this module decides what to do by READING the chain through a backend
/// that can fail. A failed read is *not* a negative answer: if a tower cannot see whether `F` was
/// spent, "no action taken" means nothing at all, and reporting it as "nothing to do" is exactly
/// how a contested exit is lost — the tower looks idle while doing nothing, during the one window
/// where being blind is fatal.
///
/// So every backend call below returns `Ok` **only when the backend actually answered**, and every
/// caller turns a failure into a typed [`WatchState::Blind`] / an `Err`, never into an empty vector.
///
/// The one subtlety worth stating: an electrum server replying *"I have no such transaction"* IS an
/// answer, and a trustworthy one. [`is_missing_tx_error`] is what separates that answer from a
/// backend that did not answer, and it fails CLOSED — anything it does not positively recognise as
/// "absent" counts as blindness.
///
/// True iff `err` is the server's own, unambiguous "no such transaction". `Error::Protocol` is the
/// only variant `electrum_client` returns un-retried and un-wrapped for a JSON-RPC error object,
/// i.e. the only case where the server demonstrably reached us; every transport error, retry
/// exhaustion (`AllAttemptsErrored`), malformed response, or unrecognised server message is NOT a
/// negative answer and must surface as blindness.
fn is_missing_tx_error(err: &electrum_client::Error) -> bool {
    match err {
        electrum_client::Error::Protocol(v) => {
            let m = v.to_string().to_ascii_lowercase();
            [
                "no such mempool or blockchain transaction", // bitcoind/electrs verbatim
                "no such transaction",
                "transaction not found",
                "unknown transaction",
                "missing transaction",
            ]
            .iter()
            .any(|needle| m.contains(needle))
        }
        _ => false,
    }
}

/// `Ok(true)` iff `txid` is known to the chain backend (confirmed or in mempool), `Ok(false)` iff
/// the backend positively answered that it has no such transaction. `Err` = **blind**: the backend
/// could not be read, or `txid` is unusable material — either way the caller learned nothing and
/// must not treat this as "not on chain yet".
fn tx_known(electrum: &electrum_client::Client, txid: &str) -> Result<bool> {
    let t = electrum_client::bitcoin::Txid::from_str(txid)
        .map_err(|e| anyhow::anyhow!("unusable txid {txid:?} in stored exit material: {e}"))?;
    match electrum.transaction_get_raw(&t) {
        Ok(_) => Ok(true),
        Err(e) if is_missing_tx_error(&e) => Ok(false),
        Err(e) => Err(anyhow::anyhow!(
            "chain backend unreadable while looking up {txid} — cannot tell whether it is on chain: {e}"
        )),
    }
}

/// `Ok(true)` iff `txid:vout` is no longer unspent (its funding UTXO has been consumed), `Ok(false)`
/// iff it is still unspent — including the case where the backend positively answered that `txid`
/// itself is not on chain, which is a laddered SUB-coin's normal steady state (its `F` is an
/// un-broadcast split output): a transaction that does not exist has no spendable output, so it
/// cannot have been spent and there is genuinely nothing to defend.
///
/// `Err` = **blind**. Note also that the out-of-range `vout` that used to PANIC here is now a
/// named error.
fn outpoint_spent(electrum: &electrum_client::Client, txid: &str, vout: u32) -> Result<bool> {
    let t = electrum_client::bitcoin::Txid::from_str(txid)
        .map_err(|e| anyhow::anyhow!("unusable funding txid {txid:?} in stored ladder: {e}"))?;
    let raw = match electrum.transaction_get_raw(&t) {
        Ok(r) => r,
        Err(e) if is_missing_tx_error(&e) => return Ok(false),
        Err(e) => {
            return Err(anyhow::anyhow!(
                "chain backend unreadable while fetching funding {txid} — cannot tell whether the \
                 coin has been triggered: {e}"
            ))
        }
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw)
            .map_err(|e| anyhow::anyhow!("funding tx {txid} did not deserialize: {e}"))?;
    let out = tx
        .output
        .get(vout as usize)
        .ok_or_else(|| anyhow::anyhow!("funding {txid} has no output {vout} ({} outputs)", tx.output.len()))?;
    let listed = electrum.script_list_unspent(&out.script_pubkey).map_err(|e| {
        anyhow::anyhow!(
            "chain backend unreadable while listing unspent outputs of {txid}:{vout} — cannot tell \
             whether the coin has been triggered: {e}"
        )
    })?;
    Ok(!listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}

/// **Typed outcome of one watchtower pass** — the single vocabulary shared by the laddered (TES-R)
/// tower here and the un-laddered deadline tower in the SDK (`mercury_utexo_sdk::watch_pass`).
///
/// It exists for one reason (external review F4): a caller MUST be able to tell
/// *"I looked, and there is nothing to do"* ([`Self::Idle`]) from *"I could not look"*
/// ([`Self::Blind`]). Both used to be an empty `Vec<String>`, so a dead electrum backend was
/// indistinguishable from a quiet chain — during precisely the race window where being blind loses
/// the coin. `Blind` is therefore never a degenerate case of the others: it means this pass
/// determined **nothing**, and an app that holds off-chain coins must treat it as an alert.
///
/// `#[must_use]`: dropping this value on the floor is the bug the type was introduced to prevent.
#[must_use = "a watch pass reports whether it could SEE; ignoring it re-introduces the blind-looks-idle bug"]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchState {
    /// The backend answered and there is genuinely nothing to do — for the TES-R tower, the coin's
    /// funding `F` is still unspent, so it has not been triggered and an idle laddered coin never
    /// ages. This is the steady state, and it is a POSITIVE observation.
    Idle,
    /// The backend answered and this pass is DEFENDING: the coin is triggered (TES-R tower) or an
    /// entry is inside its deadline margin (deadline tower).
    ///
    /// `ids` are what the pass acted on — tier **txids** for the TES-R tower, **statechain_ids** for
    /// the deadline tower — and may be EMPTY, meaning "already under way: every mature tier is out
    /// and the next one has not matured yet". `failures` carries the rejections that stopped it
    /// (typically `non-BIP68-final`, i.e. a CSV that has simply not matured), so a caller can tell a
    /// waiting exit from a RACED or dead one instead of retrying forever in silence.
    Acted { ids: Vec<String>, failures: Vec<String> },
    /// 🔴 The chain backend could not be read, or the stored exit material could not be used. This
    /// pass saw NOTHING and defended nothing. Never fold this into [`Self::Idle`].
    Blind { reason: String },
}

impl WatchState {
    /// The pass could not see. The caller must alert and retry — silence here is not safety.
    pub fn is_blind(&self) -> bool {
        matches!(self, Self::Blind { .. })
    }
    /// The pass looked and found nothing to do. False when blind, which is the whole point.
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
    /// What the pass acted on (empty for [`Self::Idle`] and [`Self::Blind`] alike — so NEVER use
    /// emptiness to decide whether the pass worked; use [`Self::is_blind`]).
    pub fn ids(&self) -> &[String] {
        match self {
            Self::Acted { ids, .. } => ids,
            _ => &[],
        }
    }
    /// Broadcast rejections seen this pass (usually an immature CSV).
    pub fn failures(&self) -> &[String] {
        match self {
            Self::Acted { failures, .. } => failures,
            _ => &[],
        }
    }
    /// The blindness cause, if any.
    pub fn blind_reason(&self) -> Option<&str> {
        match self {
            Self::Blind { reason } => Some(reason),
            _ => None,
        }
    }
}

/// **Typed progress of one unilateral-exit pass.** Only ever produced when the chain was actually
/// readable — a backend failure is an `Err` from the pass, never a quiet "nothing happened"
/// (external review F4).
#[must_use = "an exit pass reports whether the exit is complete and why it stopped"]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitProgress {
    /// Tier txids broadcast by THIS pass (empty = every mature tier was already out).
    pub broadcast: Vec<String>,
    /// The final exit state is on chain or in the mempool — the value is committed to the owner.
    pub complete: bool,
    /// Why the pass stopped short, if it did: the first broadcast rejection, normally an immature
    /// relative timelock. Retained so a DEAD or RACED exit is not reported forever as "just wait".
    pub stalled: Option<String>,
}

/// **TES-R WatchBundle (keyless watchtower).** One reactive pass: if the coin's funding UTXO `F` has
/// been spent — i.e. someone broadcast the trigger — drive the OWNER's unilateral exit by
/// broadcasting each pre-signed tier in order as its relative-timelock matures. Keyless: it holds
/// only the pre-signed [`TesrBundle`] (every tier pays the owner) and NEVER co-signs, so a delegated
/// tower can defend an offline owner without any key material. It needs ONLY an electrum
/// connection, exactly like the un-laddered tower. Idempotent — call once per new block from a tower
/// loop; already-confirmed tiers are skipped and a not-yet-mature tier just retries next pass.
///
/// Returns a [`WatchState`]: [`WatchState::Idle`] when `F` is verifiably unspent,
/// [`WatchState::Acted`] with the tier txids broadcast this pass, or [`WatchState::Blind`] when the
/// backend could not be read. **`Idle` and `Blind` are different answers** — before F4 both were an
/// empty vector, so a tower with a dead backend reported the same "all quiet" as a tower watching a
/// healthy idle coin.
pub fn watch_pass(electrum: &electrum_client::Client, bundle: &TesrBundle) -> WatchState {
    match watch_pass_seen(electrum, bundle) {
        Ok(state) => state,
        Err(e) => WatchState::Blind { reason: e.to_string() },
    }
}

/// [`watch_pass`]'s body, with every unreadable backend answer as an `Err` (which the caller turns
/// into [`WatchState::Blind`]).
fn watch_pass_seen(electrum: &electrum_client::Client, bundle: &TesrBundle) -> Result<WatchState> {
    // Defend only once the coin has actually been triggered on-chain — an idle un-broadcast coin
    // never ages, so there is nothing to do until F is spent. `?` is load-bearing: a backend that
    // cannot answer "is F spent?" must NOT be read as "F is unspent".
    if !outpoint_spent(electrum, &bundle.f_txid, bundle.f_vout)? {
        return Ok(WatchState::Idle);
    }
    let mut ids = Vec::new();
    let mut failures = Vec::new();
    for tier in bundle.exit_tiers() {
        if tx_known(electrum, &tier.txid)? {
            continue; // already on-chain / in mempool
        }
        let raw = hex::decode(&tier.signed_tx).map_err(|e| {
            anyhow::anyhow!("tier {} carries unusable signed tx hex: {e}", tier.txid)
        })?;
        match electrum.transaction_broadcast_raw(&raw) {
            Ok(_) => ids.push(tier.txid.clone()),
            Err(e) => {
                // CSV not met yet / parent unconfirmed — retry on the next pass, but SAY SO.
                failures.push(format!("{}: {e}", tier.txid));
                break;
            }
        }
    }
    Ok(WatchState::Acted { ids, failures })
}

/// **Owner-initiated unilateral exit of a laddered coin.** Like [`watch_pass`], but this KICKS OFF the exit
/// by spending `F` with the trigger — a tower defends an already-triggered coin and never initiates,
/// whereas an owner walking away must start the clock. Broadcasts the trigger (if `F` is still unspent)
/// and then every subsequent tier whose relative-CSV is now met, in exit order, stopping at the first
/// not-yet-mature tier. Idempotent and incremental: call once per block (already-confirmed/known tiers
/// are skipped). Returns `(txids_broadcast_this_pass, done)` where `done` is true once the final exit
/// state is on-chain or in the mempool — i.e. the funds are committed to the owner's exit address.
///
/// **`Err` = blind** (external review F4): the chain backend could not be read, or a tier's stored
/// hex is unusable. It is never reported as `complete: false` with an empty broadcast list, which
/// would be indistinguishable from a healthy "waiting for the next CSV".
pub fn exit_pass(electrum: &electrum_client::Client, bundle: &TesrBundle) -> Result<ExitProgress> {
    let mut broadcast = Vec::new();
    let mut stalled = None;
    for tier in bundle.exit_tiers() {
        if tx_known(electrum, &tier.txid)? {
            continue; // already on-chain / in mempool
        }
        let raw = hex::decode(&tier.signed_tx).map_err(|e| {
            anyhow::anyhow!("tier {} carries unusable signed tx hex: {e}", tier.txid)
        })?;
        match electrum.transaction_broadcast_raw(&raw) {
            Ok(_) => broadcast.push(tier.txid.clone()),
            Err(e) => {
                // CSV not met yet / parent unconfirmed — retry on the next pass, but SAY SO.
                stalled = Some(format!("{}: {e}", tier.txid));
                break;
            }
        }
    }
    let complete = tx_known(electrum, &bundle.current().state.txid)?;
    Ok(ExitProgress { broadcast, complete, stalled })
}

/// The first tier not yet on-chain in exit order, and its relative-CSV (a wait-time hint).
/// `Ok(None)` once the exit is complete; **`Err` = blind** — a backend that cannot be read must not
/// be reported as a wait time, and must certainly not be reported as "complete".
pub fn next_exit_tier(electrum: &electrum_client::Client, bundle: &TesrBundle) -> Result<Option<u16>> {
    for tier in bundle.exit_tiers() {
        if !tx_known(electrum, &tier.txid)? {
            return Ok(Some(tier.csv.unwrap_or(0)));
        }
    }
    Ok(None)
}

fn net_from_str(network: &str) -> electrum_client::bitcoin::Network {
    use electrum_client::bitcoin::Network;
    match network.to_ascii_lowercase().as_str() {
        "bitcoin" | "mainnet" => Network::Bitcoin,
        "testnet" => Network::Testnet,
        "signet" => Network::Signet,
        _ => Network::Regtest,
    }
}

/// **Receiver R′ verification (PROTOCOL.md §5.11).** Soundly verify a conveyed TES-R ladder before
/// accepting a coin: it must be a valid unilateral-exit chain over the on-chain funding UTXO `F`, and
/// the SE's PUBLIC finalized-signature count must EXACTLY account for its tiers (plus the signed-once
/// backup txs conveyed with the coin — `flat_backups`, which is 1 for an ordinary on-chain root coin,
/// whose deposit co-signs one backup before the ladder is established, and 0 for an un-broadcast split
/// slot). Exact equality is the linchpin — it makes a hidden extra co-signed state (a
/// double-spend the receiver can't see) impossible, and prevents padding the ladder with junk. Checks:
///   1. the trigger spends `F` (no relative-timelock) and pays the aggregate key `A`;
///   2. every later tier spends its parent's `out[0]`, carries a BIP-68 block CSV within the coin's
///      schedule bounds, and pays `A` — except the final state, which pays the owner;
///   3. `se_num_sigs == flat_backups + <number of tiers>`.
/// This is a PURE function (no network) so it is unit-testable and reusable by the transfer receiver.
///
/// ⚠️ **SELF-VERIFICATION ONLY — this entry point is UNBOUND (audit C-1).** Every authority it checks
/// against (`f_txid`/`f_vout`/`f_value`, `agg_address`, `statechain_id`) is a field OF THE BUNDLE, so
/// it proves INTERNAL CONSISTENCY, not that the bundle describes the coin you are about to accept. A
/// sender can convey a self-consistent DECOY ladder over an attacker-controlled outpoint, with
/// `owner_exit_address` set correctly and the tiers padded so the census balances, and keep the REAL
/// trigger — then take the coin back after the receiver accepts. Use it to re-check a ladder you built
/// yourself; to ACCEPT a conveyed one, use [`verify_bundle_bound`].
pub fn verify_bundle(bundle: &TesrBundle, se_num_sigs: u32, flat_backups: u32) -> Result<()> {
    // Ordinary bundle: the final state pays the owner. (A split parent uses verify_bundle_ex(true).)
    verify_bundle_ex(bundle, se_num_sigs, flat_backups, false)
}

/// The AUTHORITATIVE description of the coin an incoming ladder must describe. Every field is read
/// from a source the SENDER does not control — the funding transaction on chain and the coordinator's
/// `/info/statechain` record — never from the conveyed bundle.
#[derive(Clone, Debug)]
pub struct CoinAuthority {
    /// The statechain id the receiver/payer is acting on (the id whose `num_sigs` feeds the census).
    pub statechain_id: String,
    /// The coin's funding outpoint (`tx0_outpoint`), which the ladder's trigger must spend.
    pub f_txid: String,
    pub f_vout: u32,
    /// `tx0.output[f_vout].value`, read from the funding transaction.
    pub f_value: u64,
    /// `tx0.output[f_vout].script_pubkey` as hex — the on-chain P2TR of the coin's aggregate key `A`.
    pub f_spk_hex: String,
    /// The coordinator's recorded aggregate x-only key for `statechain_id` (`aggregate_pubkey`,
    /// UNIQUE per sid). `None` ⟹ REJECT: without it a rogue-key decomposition lets a sender point at
    /// a decoy sid whose counter happens to balance ([FATAL-B], migration 0009).
    pub se_aggregate_pubkey: Option<String>,
}

/// Build a [`CoinAuthority`] from the coin's FUNDING TRANSACTION plus the coordinator's record.
///
/// `tx0_hex` must be the funding transaction as read from the chain (never a sender-supplied
/// un-broadcast branch tx), and `(tx0_txid, tx0_vout)` the outpoint the coin rests on. The value and
/// the aggregate scriptPubKey are taken FROM that output, so neither can be restated by a sender.
pub fn coin_authority_from_tx0(
    statechain_id: &str,
    tx0_txid: &str,
    tx0_vout: u32,
    tx0_hex: &str,
    se_aggregate_pubkey: Option<String>,
) -> Result<CoinAuthority> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    let raw = hex::decode(tx0_hex).map_err(|_| anyhow::anyhow!("funding tx is not hex"))?;
    let tx0: Transaction =
        deserialize(&raw).map_err(|_| anyhow::anyhow!("funding tx does not parse"))?;
    if tx0.txid().to_string() != tx0_txid {
        return Err(anyhow::anyhow!(
            "funding tx hex is {} but the coin's funding outpoint names {tx0_txid}",
            tx0.txid()
        ));
    }
    let out = tx0
        .output
        .get(tx0_vout as usize)
        .ok_or_else(|| anyhow::anyhow!("funding tx has no output {tx0_vout}"))?;
    Ok(CoinAuthority {
        statechain_id: statechain_id.to_string(),
        f_txid: tx0_txid.to_string(),
        f_vout: tx0_vout,
        f_value: out.value,
        f_spk_hex: hex::encode(out.script_pubkey.as_bytes()),
        se_aggregate_pubkey,
    })
}

/// The BIP-341-tweaked P2TR output key (x-only hex) of an UNTWEAKED aggregate x-only key. The server
/// records the untweaked aggregate; an on-chain scriptPubKey commits to the tweaked output key, so a
/// comparison between the two must go through this.
fn tweaked_p2tr_key_hex(
    agg_xonly_hex: &str,
    net: electrum_client::bitcoin::Network,
) -> Result<String> {
    use electrum_client::bitcoin::{
        secp256k1::{Secp256k1, XOnlyPublicKey},
        Address,
    };
    let xonly = XOnlyPublicKey::from_str(agg_xonly_hex)
        .map_err(|_| anyhow::anyhow!("bad aggregate x-only hex"))?;
    let spk =
        Address::p2tr(&Secp256k1::verification_only(), xonly, None, net).script_pubkey();
    taproot_key_hex(spk.as_bytes())
}

/// **[R5] ESTABLISH-TIME bindability gate — do not create a ladder no receiver can bind.**
///
/// `verify_bundle_bound` fails CLOSED when the coordinator has no `aggregate_pubkey` on record for the
/// sid. On the ACCEPTANCE path that is the only correct answer (see the rejected alternatives below).
/// But the coordinator's aggregate column was added by migration 0009 and backfilled FORWARD only, so
/// pre-0009 rows are NULL (empirically a closed, contiguous legacy set: every NULL id below a clean
/// cut, none above). A pre-0009 coin is otherwise an ordinary confirmed on-chain root coin, so the
/// default IN-PLACE ladder pass happily ladders it — and the resulting ladder is unclaimable.
///
/// ⚠️ **This gate does NOT make such a coin transferable again, and must not be advertised as if it
/// did.** A legacy no-aggregate coin cannot carry a BOUND ladder — there is no coordinator aggregate
/// to bind against — so this gate keeps it un-laddered rather than minting an unclaimable ladder.
/// It still transfers on the flat lane at `protocol_version 0`: the claim path's unconditional
/// version floor was implemented, shown to break sdk41, and narrowed to a version/payload
/// consistency check, so that door is NOT shut. The only
/// complete fix is coordinator-side: backfill `aggregate_xonly` for the legacy rows from the
/// coordinator's OWN columns (`x_only(user_public_key + server_public_key)`), which is the same value
/// deposit-init records today and involves no client input. Until that runs, a legacy coin's value is
/// still recoverable — on-chain withdrawal and its signed-once backup exit are untouched — but it
/// cannot move off-chain.
///
/// What this gate DOES buy, and why it is still worth having:
///   * establishing a ladder co-signs three tiers through the SE. That is IRREVERSIBLE: it spends
///     signature budget and permanently raises the sid's `num_sigs`, on a coin that gains nothing;
///   * it stops an unclaimable bundle from being persisted and later conveyed as authentic;
///   * it is self-healing — the moment the coordinator backfills, the next `claim()` pass ladders the
///     coin normally, with no client change.
///
/// The predicate is exactly the authority half of `verify_bundle_bound`, hoisted ahead of
/// establishment: the coordinator must have an aggregate on record for the sid, and it must be the key
/// that actually controls the funding output. `f_spk_hex` is `tx0.output[vout].script_pubkey` read FROM
/// THE CHAIN and `se_aggregate_pubkey` the coordinator's `/info/statechain` record — the same two
/// authorities the receiver will use, so this predicate and the acceptance check cannot drift.
///
/// **Rejected alternatives** (both would have re-opened C-1/[FATAL-B]):
///   * *"When the coordinator has no aggregate, derive it from `tx0.output[vout]` instead."* That key
///     is already `a_onchain`, and the ladder is already checked against it — so this is not a second
///     authority, it is the absence of one. `tx0`, its outpoint and the ladder all arrive from the
///     SENDER; the only other thing tying them to the sid is `validate_tx0_output_pubkey`, which tests
///     `enclave_pubkey(sid) + transfer_msg.user_public_key == tx0.out[vout]` — and the sender chooses
///     `user_public_key`, so the rogue-key decomposition `U := D − E_sid` makes ANY attacker-controlled
///     output `D` pass. The coordinator's per-sid, UNIQUE aggregate is the one value in the whole
///     acceptance path that is not restatable by the sender. Making its absence a fallback would also
///     hand the attacker the trigger: he picks which sid to convey, hence whether the record is NULL.
///   * *"Accept a ladder whose tiers carry genuine SE co-signatures under `A`, sid-record or not."* A
///     schnorr co-sign proves the SE signed under `A` for SOME sid, never for THIS one; an attacker
///     who owns a second coin can pad a decoy ladder over it to any tier count, and the census is run
///     against the conveyed sid's `num_sigs`, which he also controls. Not a substitute.
pub fn ladder_binding_precheck(
    statechain_id: &str,
    f_spk_hex: &str,
    se_aggregate_pubkey: Option<&str>,
    network: &str,
) -> Result<()> {
    ladder_binding_precheck_cause(statechain_id, f_spk_hex, se_aggregate_pubkey, network)
        .map_err(anyhow::Error::new)
}

/// **WHY the ladder could not be bound — the typed counterpart of [`ladder_binding_precheck`].**
///
/// The causes are NOT interchangeable, and a caller that collapses them fails OPEN. Only
/// [`BindingRefusal::NoCoordinatorAggregate`] is a structurally PERMANENT property of the coin (a
/// pre-0009 legacy row: nothing about the coin will ever make it bindable until the coordinator
/// backfills), and it is the only cause that may be read as "this coin legitimately has no ladder".
/// Every other cause means the coin's shape is WRONG or unreadable — a non-taproot funding output, a
/// scriptPubKey we could not parse, an aggregate that does not control `F` — and a caller must
/// refuse rather than treat it as a licence.
///
/// The flat-conveyance classifier in `transfer_sender` used to test `ladder_binding_precheck(..)
/// .is_err()` and license the flat lane on ANY error, which silently folded "decoy-shaped coin" and
/// "spk we could not decode" into "harmless legacy coin". This enum exists so it cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingRefusal {
    /// The coordinator has NO `aggregate_pubkey` on record for the sid (pre-migration-0009 legacy
    /// row). PERMANENT for the coin until a coordinator-side backfill.
    NoCoordinatorAggregate,
    /// The funding scriptPubKey is not hex — the caller handed us something unreadable.
    FundingSpkUnparseable,
    /// The funding output is not a v1 taproot output, so it has no aggregate key to bind to.
    FundingNotTaproot,
    /// The coordinator's recorded aggregate could not be tweaked into a P2TR key (unusable key).
    AggregateUnusable,
    /// The coordinator HAS an aggregate for the sid, but it is not the key controlling `F`. A ladder
    /// here would be refused as a decoy at acceptance.
    AggregateMismatch,
}

/// [`BindingRefusal`] plus the human-readable message the untyped
/// [`ladder_binding_precheck`] has always produced (unchanged, so callers that string-match keep
/// working).
#[derive(Clone, Debug)]
pub struct LadderBindingError {
    pub cause: BindingRefusal,
    pub message: String,
}

impl std::fmt::Display for LadderBindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LadderBindingError {}

/// [`ladder_binding_precheck`] with the refusal CAUSE preserved. See [`BindingRefusal`].
pub fn ladder_binding_precheck_cause(
    statechain_id: &str,
    f_spk_hex: &str,
    se_aggregate_pubkey: Option<&str>,
    network: &str,
) -> std::result::Result<(), LadderBindingError> {
    let refuse = |cause: BindingRefusal, message: String| LadderBindingError { cause, message };
    let net = net_from_str(network);
    let f_spk = hex::decode(f_spk_hex).map_err(|_| {
        refuse(
            BindingRefusal::FundingSpkUnparseable,
            "funding scriptPubKey is not hex — cannot bind a ladder".to_string(),
        )
    })?;
    let a_onchain = taproot_key_hex(&f_spk).map_err(|e| {
        refuse(
            BindingRefusal::FundingNotTaproot,
            format!("coin funding output is not a v1 taproot output: {e}"),
        )
    })?;
    let se_agg = se_aggregate_pubkey.ok_or_else(|| {
        refuse(
            BindingRefusal::NoCoordinatorAggregate,
            format!(
                "the coordinator recorded no aggregate for statechain id {statechain_id} — a ladder \
                 over this coin could not be bound by any receiver (pre-0009 legacy coin: leave it \
                 un-laddered)"
            ),
        )
    })?;
    let se_agg_spk = tweaked_p2tr_key_hex(se_agg, net).map_err(|e| {
        refuse(
            BindingRefusal::AggregateUnusable,
            format!(
                "the coordinator's aggregate for statechain id {statechain_id} is not a usable \
                 x-only key ({e}) — a ladder over this coin could not be bound"
            ),
        )
    })?;
    if se_agg_spk != a_onchain {
        return Err(refuse(
            BindingRefusal::AggregateMismatch,
            format!(
                "the coordinator's aggregate for statechain id {statechain_id} does not match the \
                 funding output key — a ladder over this coin would be refused as a decoy"
            ),
        ));
    }
    Ok(())
}

/// **[C-1] ACCEPTANCE-PATH verifier — [`verify_bundle`] BOUND TO THE COIN.**
///
/// `verify_bundle` proves a ladder is internally consistent. That is not the question a receiver (or a
/// pre-paying SSP) is asking: it needs to know that THIS ladder describes THE COIN it is accepting.
/// The attack this closes is the default coin shape, not an exotic one — a self-signed decoy ladder
/// over an attacker-controlled outpoint, `owner_exit_address` correctly set to the receiver's key (so
/// the Model-A gate passes) and tiers padded so the census balances, while the sender retains the REAL
/// trigger and spends the coin back afterwards.
///
/// So the authority is derived from the COIN and the ladder is checked against it:
///   * `bundle.statechain_id` == the sid whose `num_sigs` the census is run against — otherwise the
///     count being balanced belongs to a different coin;
///   * `(f_txid, f_vout)` == the coin's funding outpoint, and `f_value` == that output's value read
///     from the funding transaction (the value every tier's co-sign sighash commits to);
///   * `bundle.agg_address` == P2TR of the key in `tx0.output[f_vout].script_pubkey`, so the key the
///     tiers are verified against is the ON-CHAIN one;
///   * that same key == the coordinator's recorded `aggregate_pubkey` for the sid, which is UNIQUE per
///     sid — so a second outpoint paying the same aggregate cannot be substituted under a decoy sid,
///     and (with the exact-equality census) a second ladder over a second UTXO of the SAME sid would
///     need extra SE co-signs and blows the count.
///
/// Fails CLOSED on every absent/unparseable field, including a missing server aggregate.
pub fn verify_bundle_bound(
    bundle: &TesrBundle,
    se_num_sigs: u32,
    flat_backups: u32,
    coin: &CoinAuthority,
) -> Result<()> {
    if bundle.statechain_id != coin.statechain_id {
        return Err(anyhow::anyhow!(
            "conveyed ladder is for statechain id {} but the coin being accepted is {} — the census would balance a different coin",
            bundle.statechain_id,
            coin.statechain_id
        ));
    }
    if bundle.f_txid != coin.f_txid || bundle.f_vout != coin.f_vout {
        return Err(anyhow::anyhow!(
            "conveyed ladder is rooted at {}:{} but the coin's funding outpoint is {}:{} — decoy ladder",
            bundle.f_txid,
            bundle.f_vout,
            coin.f_txid,
            coin.f_vout
        ));
    }
    if bundle.f_value != coin.f_value {
        return Err(anyhow::anyhow!(
            "conveyed ladder declares F value {} but the funding output carries {} — the tier sighashes commit to the real value",
            bundle.f_value,
            coin.f_value
        ));
    }

    let net = net_from_str(&bundle.network);
    let f_spk = hex::decode(&coin.f_spk_hex).map_err(|_| anyhow::anyhow!("bad funding spk hex"))?;
    let a_onchain = taproot_key_hex(&f_spk)
        .map_err(|e| anyhow::anyhow!("coin funding output is not a v1 taproot output: {e}"))?;
    let declared_agg_spk = electrum_client::bitcoin::Address::from_str(&bundle.agg_address)
        .map_err(|_| anyhow::anyhow!("bad ladder agg_address"))?
        .require_network(net)
        .map_err(|_| anyhow::anyhow!("ladder agg_address is on the wrong network"))?
        .script_pubkey();
    if taproot_key_hex(declared_agg_spk.as_bytes())? != a_onchain {
        return Err(anyhow::anyhow!(
            "conveyed ladder's aggregate address does not match the coin's on-chain funding key — decoy ladder"
        ));
    }

    let se_agg = coin.se_aggregate_pubkey.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "the coordinator recorded no aggregate for statechain id {} (fail-closed)",
            coin.statechain_id
        )
    })?;
    if tweaked_p2tr_key_hex(se_agg, net)? != a_onchain {
        return Err(anyhow::anyhow!(
            "the coordinator's aggregate for statechain id {} does not match the funding output key — decoy coin",
            coin.statechain_id
        ));
    }

    verify_bundle_ex(bundle, se_num_sigs, flat_backups, false)
}

/// As [`verify_bundle`], but when `final_is_split` the FINAL tier is an in-ladder split state `SP` that
/// pays the children (not the owner), so its output-payee check is skipped here — each child's outputs
/// are verified against its own aggregate by [`verify_child_bundle`]. Everything else (co-signs under
/// `A`, the per-outpoint race, the exact-equality census) is unchanged.
/// Validate + count a segment's DISCLOSED SUPERSEDED tiers, returning how many were accepted.
///
/// Shared by the root ladder (`verify_bundle_ex`) and a split CHILD's own segment, so the two can
/// never drift: a second copy of this battery is exactly how the `[S1]` count-padding class returns.
/// The caller supplies the segment's aggregate scriptPubKey, its schedule params, the running
/// `prevout_value_of` map (this fn INSERTS every superseded output into it before validating, so a
/// superseded tier may legitimately parent another — the renewal/transitive-death case), and the
/// per-outpoint CSV of the LIVE tier spending it.
///
/// Every returned entry has been: parsed, txid-bound, ladder-linked, verified as a genuine co-sign by
/// the aggregate key, CSV-checked within the schedule bounds, and proven NON-CONFIRMABLE — either it
/// directly loses a maturity race to the live tier over the same outpoint, or its prevout is
/// transitively never created. Anything else is an orphan/threat branch and is refused.
fn verify_superseded_segment(
    sup_states: &[TesrTier],
    sup_exts: &[TesrTier],
    agg_spk: &electrum_client::bitcoin::ScriptBuf,
    p: &mercurylib::tesr::TesrParams,
    prevout_value_of: &mut std::collections::HashMap<(electrum_client::bitcoin::Txid, u32), u64>,
    live_csv_by_outpoint: &std::collections::HashMap<(electrum_client::bitcoin::Txid, u32), u32>,
    live_txids: &std::collections::HashSet<electrum_client::bitcoin::Txid>,
) -> Result<u32> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction, Txid};
    // [C-2] ONE co-sign, ONE census slot. The return value of this function is added to the SE's
    // exact-equality count, so a DUPLICATE disclosure inflates `expected` by one for free and absorbs
    // one hidden co-signed state — the same padding class as `[S1]`, except every padded entry is a
    // GENUINE tier and so passes parse, txid-binding, linkage, signature and race checks unchanged.
    // One set spanning BOTH superseded lists AND the live exit tiers closes it: a tier may appear in
    // the bundle exactly once, whether it is disclosed twice or disclosed once while also being live.
    let mut seen_txids: std::collections::HashSet<Txid> = live_txids.clone();
    // Parsed up-front so their outputs can also serve as parents (e.g. a superseded extension's state
    // after a renewal) and so no unparseable entry reaches the checks below.
    let mut superseded_parsed: Vec<(&'static str, usize, &TesrTier, Transaction)> = Vec::new();
    for (kind, list) in [("state", sup_states), ("extension", sup_exts)] {
        for (j, s) in list.iter().enumerate() {
            let raw = hex::decode(&s.signed_tx)
                .map_err(|_| anyhow::anyhow!("superseded {kind} {j}: bad hex"))?;
            let tx: Transaction = deserialize(&raw)
                .map_err(|_| anyhow::anyhow!("superseded {kind} {j}: not a transaction"))?;
            if tx.input.len() != 1 || tx.output.is_empty() {
                return Err(anyhow::anyhow!("superseded {kind} {j}: malformed tier"));
            }
            if tx.txid().to_string() != s.txid {
                return Err(anyhow::anyhow!("superseded {kind} {j}: txid does not match its tx"));
            }
            let id = tx.txid();
            if !seen_txids.insert(id) {
                return Err(anyhow::anyhow!(
                    "superseded {kind} {j}: tier {id} is disclosed more than once (or is also a live tier) \
                     — one co-sign may be counted only once, and a repeat masks a hidden state"
                ));
            }
            for (vout, o) in tx.output.iter().enumerate() {
                prevout_value_of.insert((id, vout as u32), o.value);
            }
            superseded_parsed.push((kind, j, s, tx));
        }
    }

    // Static per-tier validation (parse/linkage/co-sign/CSV) followed by a NON-CONFIRMABILITY fixpoint.
    //
    // A disclosed superseded tier is safe to count iff it can never CONFIRM and out-race the owner. There
    // are two ways it is proven non-confirmable:
    //   (i)  DIRECT CONTENTION: it spends the same outpoint as a LIVE exit tier, and its CSV is strictly
    //        greater — it loses the maturity race (lower CSV wins). This is the presign/transfer case
    //        (old state and S' both spend the live extension's out[0]).
    //   (ii) TRANSITIVE DEATH: its parent (the tier whose output it spends) is itself a superseded tier
    //        proven non-confirmable — so this tier's prevout is never created on-chain. This is the RENEW
    //        case: renew supersedes BOTH the extension and the state, so the old state spends the OLD
    //        (superseded) extension's out[0], which no LIVE tier spends. The old extension loses its own
    //        race for T.out[0] to the new extension, so it can never confirm, so the old state can never
    //        confirm. Rejecting it as an "orphan" (the earlier check did) wrongly bricked every renewed
    //        coin — uncaught because no test ran renew + verify_bundle together.
    // A tier that is NEITHER out-raced by a live tier NOR rooted (transitively) in such a losing race is a
    // real orphan/threat and is refused. Cycles never root in a live contention, so they are never marked
    // dead ⟹ never accepted.
    struct Sup {
        kind: &'static str,
        j: usize,
        prevout: (Txid, u32),
        csv: u32,
        outputs: Vec<(Txid, u32)>,
    }
    let mut sups: Vec<Sup> = Vec::with_capacity(superseded_parsed.len());
    for (kind, j, s, tx) in superseded_parsed.iter() {
        let (kind, j) = (*kind, *j);
        // (a) parsed + txid-bound already (pre-pass above) — no unparseable entry reaches here.
        // (b) ladder linkage — it must spend an output of a tier of THIS ladder.
        let op = tx.input[0].previous_output;
        let value = *prevout_value_of
            .get(&(op.txid, op.vout))
            .ok_or_else(|| anyhow::anyhow!("superseded {kind} {j}: spends an outpoint outside this ladder"))?;
        // (c) signature — proves it actually consumed an SE co-sign.
        verify_tier_cosigned(tx, value, &agg_spk)
            .map_err(|e| anyhow::anyhow!("superseded {kind} {j} is not co-signed by A: {e}"))?;
        // (d) CSV is REQUIRED (never skippable), a BIP-68 block relative-timelock, in schedule bounds.
        let seq = tx.input[0].sequence.0;
        if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
            return Err(anyhow::anyhow!("superseded {kind} {j}: not a BIP-68 block relative-timelock"));
        }
        let csv = seq as u16;
        let declared = s.csv.ok_or_else(|| anyhow::anyhow!("superseded {kind} {j}: no CSV declared"))?;
        if declared != csv {
            return Err(anyhow::anyhow!("superseded {kind} {j}: declared CSV {declared} != tx CSV {csv}"));
        }
        let (lo, hi) = if kind == "extension" { (p.e_floor, p.e0) } else { (p.d_floor, p.d0) };
        if csv < lo || csv > hi {
            return Err(anyhow::anyhow!("superseded {kind} {j}: CSV {csv} outside bounds [{lo},{hi}]"));
        }
        let outputs = tx
            .output
            .iter()
            .enumerate()
            .map(|(v, _)| (tx.txid(), v as u32))
            .collect();
        sups.push(Sup { kind, j, prevout: (op.txid, op.vout), csv: csv as u32, outputs });
    }

    // (e) NON-CONFIRMABILITY. Seed from direct contention with a live tier; propagate transitive death.
    let mut dead = vec![false; sups.len()];
    let mut dead_outputs: std::collections::HashSet<(Txid, u32)> = std::collections::HashSet::new();
    for (idx, sup) in sups.iter().enumerate() {
        if let Some(&live_csv) = live_csv_by_outpoint.get(&sup.prevout) {
            // Directly contends with a live tier over the SAME outpoint — it MUST lose the race, or it
            // could mature first and steal. (This is [S-1/S-2]: extensions and states alike, against the
            // live tier spending the same outpoint, never a global final_csv.)
            if sup.csv <= live_csv {
                return Err(anyhow::anyhow!(
                    "superseded {} {} has CSV {} <= the live tier's {} over the same outpoint — it could out-race the owner",
                    sup.kind, sup.j, sup.csv, live_csv
                ));
            }
            dead[idx] = true;
            for o in &sup.outputs {
                dead_outputs.insert(*o);
            }
        }
    }
    loop {
        let mut changed = false;
        for idx in 0..sups.len() {
            if dead[idx] {
                continue;
            }
            if dead_outputs.contains(&sups[idx].prevout) {
                dead[idx] = true;
                for o in sups[idx].outputs.clone() {
                    dead_outputs.insert(o);
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for (idx, sup) in sups.iter().enumerate() {
        if !dead[idx] {
            return Err(anyhow::anyhow!(
                "superseded {} {} is neither out-raced by a live tier nor transitively dead (spends {}:{}) — orphan/threat branch",
                sup.kind, sup.j, sup.prevout.0, sup.prevout.1
            ));
        }
    }
    Ok(sups.len() as u32)
}

fn verify_bundle_ex(bundle: &TesrBundle, se_num_sigs: u32, flat_backups: u32, final_is_split: bool) -> Result<()> {
    use electrum_client::bitcoin::{consensus::deserialize, Address, Transaction, Txid};

    let net = net_from_str(&bundle.network);
    let spk_of = |addr: &str| -> Result<_> {
        Ok(Address::from_str(addr)
            .map_err(|_| anyhow::anyhow!("bad address {addr}"))?
            .require_network(net)
            .map_err(|_| anyhow::anyhow!("address {addr} wrong network"))?
            .script_pubkey())
    };
    let agg_spk = spk_of(&bundle.agg_address)?;
    let owner_spk = spk_of(&bundle.owner_exit_address)?;

    let tiers = bundle.exit_tiers(); // [trigger, ext0, state0, ext1, state1, ...]
    if tiers.len() < 3 || (tiers.len() - 1) % 2 != 0 {
        return Err(anyhow::anyhow!("malformed ladder: expected trigger + N*(extension,state)"));
    }

    let txs: Vec<Transaction> = tiers
        .iter()
        .map(|t| {
            let raw = hex::decode(&t.signed_tx).map_err(|_| anyhow::anyhow!("bad tier hex"))?;
            deserialize::<Transaction>(&raw).map_err(|_| anyhow::anyhow!("bad tier tx"))
        })
        .collect::<Result<_>>()?;

    // 1. Trigger spends F and pays A.
    let t = &txs[0];
    if t.input.len() != 1
        || t.input[0].previous_output.txid != Txid::from_str(&bundle.f_txid).map_err(|_| anyhow::anyhow!("bad F txid"))?
        || t.input[0].previous_output.vout != bundle.f_vout
    {
        return Err(anyhow::anyhow!("trigger does not spend the funding UTXO F"));
    }
    // The trigger must pay `A` on its declared PAYLOAD output (`payload_vout`, 0 today). Reading the
    // payload through the accessor rather than `output[0]` is what keeps this check meaningful once a
    // coloured tier carries an opret at index 0 — and a bogus declared index fails closed right here.
    if tiers[0].payload_out(t, "trigger")?.script_pubkey != agg_spk {
        return Err(anyhow::anyhow!("trigger does not pay the aggregate key A"));
    }

    // 2. Each later tier spends its parent's PAYLOAD output, within schedule bounds, paying A (or the
    //    owner if final).
    let p = &bundle.params;
    for i in 1..txs.len() {
        let tx = &txs[i];
        if tx.input.len() != 1
            || tx.input[0].previous_output.txid != txs[i - 1].txid()
            || tx.input[0].previous_output.vout != tiers[i - 1].payload_vout
        {
            return Err(anyhow::anyhow!("tier {i} does not spend its parent's payload output"));
        }
        let seq = tx.input[0].sequence.0;
        if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
            return Err(anyhow::anyhow!("tier {i} is not a BIP-68 block relative-timelock"));
        }
        let csv = seq as u16;
        let is_extension = i % 2 == 1;
        let (lo, hi) = if is_extension { (p.e_floor, p.e0) } else { (p.d_floor, p.d0) };
        if csv < lo || csv > hi {
            return Err(anyhow::anyhow!("tier {i} CSV {csv} outside schedule bounds [{lo},{hi}]"));
        }
        let is_final = i == txs.len() - 1;
        if is_final && final_is_split {
            // SP (split state) pays the children, not the owner; its outputs are verified per-child by
            // verify_child_bundle (A_child == SP.out[j]). Skip the single-owner payee check here.
        } else {
            let want = if is_final { &owner_spk } else { &agg_spk };
            if &tiers[i].payload_out(tx, &format!("tier {i}"))?.script_pubkey != want {
                return Err(anyhow::anyhow!("tier {i} pays the wrong output"));
            }
        }
    }

    // ---- Superseded tiers: PARSE + LADDER-LINK + SIGNATURE-VERIFY before they may be counted [S1].
    //
    // The count in check 3 is the anti-theft linchpin, so EVERY term of it must correspond to a real,
    // co-signed tier OF THIS LADDER. Previously `superseded_*` were only `.len()`-counted — never
    // parsed, linked or signature-checked — and the CSV race-check skipped `csv: None`. A sender
    // holding a hidden low-CSV state could pad one junk entry (`signed_tx: ""`, `csv: None`) to make
    // `expected` match the inflated `num_sigs`, get ACCEPTED, then broadcast the hidden state and take
    // the coin back. Parsing alone is NOT sufficient either: a structurally valid but never-co-signed
    // tx would still pad the count. Only a valid signature by `A` proves a tier consumed a co-sign.
    //
    // `prevout_value_of` maps (parent txid, vout) → the value that output carries — exactly the prevout
    // value the child's taproot key-spend sighash commits to (mirrors `cosign_tier_request`). Two
    // properties matter:
    //   * keyed PER-OUTPUT, not per-txid. A tier may legitimately hang off any `out[j]` of its parent —
    //     that is how an in-ladder split state (PROTOCOL.md §5.4) hosts N children, and the mechanism that
    //     dissolves B1 (a split that DESCENDS from the trigger instead of racing it for `F`). A
    //     txid-only map silently assumed `out[0]` and would mis-value every child but the first.
    //   * values are read from the PARSED transactions, never from the declared `out_value` field, which
    //     is attacker-supplied. The tx is the authority; there is no reason to consult the claim.
    let mut prevout_value_of: std::collections::HashMap<(Txid, u32), u64> = std::collections::HashMap::new();
    prevout_value_of.insert(
        (Txid::from_str(&bundle.f_txid).map_err(|_| anyhow::anyhow!("bad F txid"))?, bundle.f_vout),
        bundle.f_value,
    );
    for tx in txs.iter() {
        let id = tx.txid();
        for (vout, o) in tx.output.iter().enumerate() {
            prevout_value_of.insert((id, vout as u32), o.value);
        }
    }
    // Every counted tier (exit chain AND superseded) must verify as a genuine co-sign by A.
    for (i, tx) in txs.iter().enumerate() {
        let op = tx.input[0].previous_output;
        let value = *prevout_value_of
            .get(&(op.txid, op.vout))
            .ok_or_else(|| anyhow::anyhow!("tier {i} spends an outpoint outside this ladder"))?;
        verify_tier_cosigned(tx, value, &agg_spk)
            .map_err(|e| anyhow::anyhow!("exit tier {i} is not co-signed by A: {e}"))?;
    }

    // PER-PREVOUT race map [S-1/S-2]: for each outpoint the exit chain spends, the CSV of the LIVE tier
    // that spends it. A superseded tier only ever contends with the exit tier consuming the SAME
    // outpoint, so that — not a global `final_csv` — is what it must be compared against.
    //   S-2: comparing every superseded entry to `txs.last()`'s CSV is meaningless across unrelated
    //        outpoints (post-rollover a level-0 superseded state was compared to the level-1 final
    //        state — they never contend), and it also bricks honest renew→transfer sequences.
    //   S-1: the old check was additionally gated on `kind == "state"`, leaving superseded EXTENSIONS
    //        race-UNCHECKED. A genuinely co-signed X_evil at e_floor + its child S_evil both verify
    //        against A and balance the count, but X_evil matures far ahead of the honest extension and
    //        its state pays the attacker — outright theft. Extensions are now checked identically.
    // Keyed per-OUTPUT for the same reason as `prevout_value_of`: a split state hosts a child on each
    // `out[j]`, so "the live tier over this outpoint" is only well-defined per (txid, vout).
    let mut live_csv_by_outpoint: std::collections::HashMap<(Txid, u32), u32> =
        std::collections::HashMap::new();
    for i in 1..txs.len() {
        let op = txs[i].input[0].previous_output;
        live_csv_by_outpoint.insert((op.txid, op.vout), txs[i].input[0].sequence.0 & 0xFFFF);
    }
    // [C-2] Every LIVE exit tier's txid, so a superseded list cannot re-declare one of them (or repeat
    // itself) to buy an extra census slot.
    let live_txids: std::collections::HashSet<Txid> = txs.iter().map(|t| t.txid()).collect();
    if live_txids.len() != txs.len() {
        return Err(anyhow::anyhow!(
            "the exit chain repeats a tier txid — one co-sign counted twice"
        ));
    }
    let superseded_ok = verify_superseded_segment(
        &bundle.superseded_states,
        &bundle.superseded_extensions,
        &agg_spk,
        &p,
        &mut prevout_value_of,
        &live_csv_by_outpoint,
        &live_txids,
    )?;

    // 3. The linchpin: the SE's finalized-signature count must EXACTLY account for EVERY co-signed
    //    tier — the exit chain PLUS the superseded states/extensions (full-disclosure counting). Every
    //    term below is now a VERIFIED co-sign of this ladder (above), so the count cannot be padded.
    //    A hidden co-signed state bumps num_sigs without appearing here ⟹ reject.
    let expected = flat_backups + tiers.len() as u32 + superseded_ok;
    if se_num_sigs != expected {
        return Err(anyhow::anyhow!(
            "num_sigs mismatch: SE issued {se_num_sigs}, disclosed tiers+backups account for {expected} — possible hidden state"
        ));
    }
    Ok(())
}

/// [in-ladder split] The x-only taproot key (hex) of a v1 taproot scriptPubKey, or an error if `spk`
/// is not `OP_1 <32-byte push>`.
fn taproot_key_hex(spk: &[u8]) -> Result<String> {
    if spk.len() != 34 || spk[0] != 0x51 || spk[1] != 0x20 {
        return Err(anyhow::anyhow!("not a v1 taproot scriptPubKey"));
    }
    Ok(hex::encode(&spk[2..34]).to_lowercase())
}

/// [in-ladder split] Verify a split child's exit bundle — the 8-check Stage-2 predicate (ruling
/// wqvoxvusg). Proves the child is safe from a hidden parent state rivalling `SP` over `X_m.out[0]`
/// with NO SGX: soundness rests on the on-chain root of `A_parent`, the authoritative server aggregate
/// records (Stage 1, UNIQUE), the exact-equality censuses, and coordinator-enforced terminality.
///
/// PURE + unit-testable: all authoritative values are passed in (the caller fetches them from chain +
/// `/info/statechain`). `parent_f_onchain_spk_hex` is `F.output[f_vout].script_pubkey` read from the
/// chain (the caller having confirmed `F` unspent+confirmed at `cb.parent.f_txid/f_vout`); the
/// `*_aggregate_pubkey` are the server's recorded aggregates (None ⟹ fail-closed).
///
/// ⚠️ DORMANT + UNREVIEWED: nothing calls this for a live split yet (HF-1 still refuses to split a
/// laddered coin). It must pass the split E2E + an adversarial test suite + an independent review before
/// HF-1 is removed. Conservative for now: a child that has been renewed/transferred (non-empty child
/// superseded sets) is REJECTED rather than under-validated — that path is future work.
pub fn verify_child_bundle(
    cb: &ChildTesrBundle,
    parent_f_onchain_spk_hex: &str,
    parent_num_sigs: u32,
    parent_flat_backups: u32,
    parent_aggregate_pubkey: Option<&str>,
    parent_terminal: bool,
    child_num_sigs: u32,
    child_flat_backups: u32,
    child_aggregate_pubkey: Option<&str>,
    ancestor_facts: &[AncestorFacts],
    receiver_backup_address: &str,
) -> Result<()> {
    // [F2] The two segments are secured DIFFERENTLY, because only one of them is being handed over.
    //
    // PARENT (ancestor segment) — terminality is load-bearing and still REQUIRED. Its census is a
    // snapshot nobody in this transfer can refresh: the receiver never takes ownership of the parent,
    // so a sender who skipped `set_spend_budget` could pass the count here and then have the SE
    // co-sign a rival trigger T' over the live on-chain F afterwards. Terminal ⟹ the SE refuses
    // every further co-sign ⟹ the snapshot is durable. Fail-closed.
    //
    // CHILD (leaf segment) — terminality is NOT required, and requiring it would make the child
    // exit-only forever. Its census is made durable by a different, stronger mechanism: the receiver
    // COMPLETES the key handover during this same claim, which rotates the SE share and the auth key,
    // locking the sender out PERMANENTLY. The coordinator's pending-transfer lock (armed when
    // `convey_child_bundle` opened the transfer) covers the gap between this census and that
    // completion. See CHILDREN.md. NOTE the verifier must TOLERATE a terminal child — the
    // Lightning-latched lane deliberately keeps one — it simply no longer checks.
    if !parent_terminal {
        return Err(anyhow::anyhow!(
            "parent sid is NOT terminal — a rival state over F/X_m.out[0] could still be co-signed (fail-closed)"
        ));
    }
    use electrum_client::bitcoin::{consensus::deserialize, Address, Transaction};
    let net = net_from_str(&cb.parent.network);

    // The server records the UNTWEAKED aggregate x-only; an on-chain scriptPubKey commits to the
    // BIP-341-TWEAKED output key. So to compare a recorded aggregate to a key read from a spk, tweak the
    // recorded aggregate first (P2TR with no script tree) and take the resulting output key.
    let tweaked_key_hex = |agg_xonly_hex: &str| -> Result<String> {
        // Shared with `verify_bundle_bound` so the two acceptance paths can never drift.
        tweaked_p2tr_key_hex(agg_xonly_hex, net)
    };

    // [1] ON-CHAIN ROOT: A_parent := taproot key of the fetched on-chain F.spk. Bind the parent
    //     segment's declared aggregate to it — the co-sign key is anchored to the on-chain funding,
    //     not a sender field. (This closes the "sender picks a decoy F" path together with [2].)
    let f_spk = hex::decode(parent_f_onchain_spk_hex).map_err(|_| anyhow::anyhow!("bad F spk hex"))?;
    let a_parent = taproot_key_hex(&f_spk)?;
    let parent_agg_spk = Address::from_str(&cb.parent.agg_address)
        .map_err(|_| anyhow::anyhow!("bad parent agg_address"))?
        .require_network(net)
        .map_err(|_| anyhow::anyhow!("parent agg_address wrong network"))?
        .script_pubkey();
    if taproot_key_hex(parent_agg_spk.as_bytes())? != a_parent {
        return Err(anyhow::anyhow!("parent agg_address does not match the on-chain F aggregate"));
    }

    // [2] PARENT AGGREGATE AUTHORITY: the server's recorded aggregate for parent_sid must be non-NULL
    //     (fail-closed) and == A_parent. UNIQUE(aggregate_xonly) ⟹ only the REAL parent sid can hold
    //     A_parent, so a decoy parent_sid (whose counter a sender could pump) can never match here.
    let p_agg = parent_aggregate_pubkey
        .ok_or_else(|| anyhow::anyhow!("server recorded no aggregate for parent sid (fail-closed)"))?;
    if tweaked_key_hex(p_agg)? != a_parent {
        return Err(anyhow::anyhow!("parent sid's server aggregate != A_parent (decoy parent)"));
    }

    // [3]+[4] PARENT SEGMENT + CENSUS: verify_bundle over the parent proves every parent tier is
    //     co-signed under A_parent(=agg_address, now bound to on-chain F), SP is the current state,
    //     S_0 is disclosed as superseded and OUT-RACED by SP over X_m.out[0], and the exact-equality
    //     census holds (num_sigs(parent_sid) accounts for exactly the disclosed tiers).
    verify_bundle_ex(&cb.parent, parent_num_sigs, parent_flat_backups, true)
        .map_err(|e| anyhow::anyhow!("parent segment/census invalid: {e}"))?;

    // Parse SP (the parent's current, terminal state) — it hosts the children on its outputs.
    let sp_tx: Transaction = deserialize(
        &hex::decode(&cb.parent.current().state.signed_tx).map_err(|_| anyhow::anyhow!("bad SP hex"))?,
    )
    .map_err(|_| anyhow::anyhow!("SP is not a transaction"))?;

    // [4b] INTERMEDIATE CHILD SEGMENTS (a child that descends from another child). Walk them root→leaf,
    //      advancing the funding pointer; the leaf checks below then run unchanged at any depth.
    //
    //      An ancestor segment is NOT being handed over in this transfer, so — exactly like the parent —
    //      its census is a snapshot the receiver cannot refresh, and terminality is what makes it
    //      durable. Fail closed on any mismatch of supplied facts.
    if ancestor_facts.len() != cb.ancestors.len() {
        return Err(anyhow::anyhow!(
            "ancestor facts ({}) do not match disclosed ancestor segments ({}) — fail-closed",
            ancestor_facts.len(),
            cb.ancestors.len()
        ));
    }
    let mut cur_tx = sp_tx.clone();
    for (i, seg) in cb.ancestors.iter().enumerate() {
        let facts = &ancestor_facts[i];
        let fund_out = cur_tx
            .output
            .get(seg.funding_vout as usize)
            .ok_or_else(|| anyhow::anyhow!("ancestor {i}: funding tx has no output {}", seg.funding_vout))?
            .clone();
        let fund_txid = cur_tx.txid();
        let seg_spk = fund_out.script_pubkey.clone();
        // The segment's aggregate is KEY-DERIVED from the output it spends, then bound to the server's
        // recorded aggregate for its statechain id.
        let a_seg = taproot_key_hex(seg_spk.as_bytes())?;
        let seg_agg = facts
            .aggregate_pubkey
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("ancestor {i}: server recorded no aggregate (fail-closed)"))?;
        if tweaked_key_hex(seg_agg)? != a_seg {
            return Err(anyhow::anyhow!("ancestor {i}: server aggregate != its funding output key (decoy)"));
        }
        if !facts.terminal {
            return Err(anyhow::anyhow!(
                "ancestor {i} is NOT terminal — a rival state over its funding outpoint could still be co-signed (fail-closed)"
            ));
        }
        // ext spends the funding outpoint; state spends ext.out[0]; both co-signed by A_seg.
        let ext_tx: Transaction = deserialize(
            &hex::decode(&seg.extension.signed_tx).map_err(|_| anyhow::anyhow!("ancestor {i}: bad ext hex"))?,
        )
        .map_err(|_| anyhow::anyhow!("ancestor {i}: extension is not a transaction"))?;
        let ein = ext_tx.input.first().ok_or_else(|| anyhow::anyhow!("ancestor {i}: ext has no input"))?;
        if ext_tx.input.len() != 1
            || ein.previous_output.txid != fund_txid
            || ein.previous_output.vout != seg.funding_vout
        {
            return Err(anyhow::anyhow!("ancestor {i}: extension does not spend its funding outpoint"));
        }
        verify_tier_cosigned(&ext_tx, fund_out.value, &seg_spk)
            .map_err(|e| anyhow::anyhow!("ancestor {i}: extension not co-signed by its aggregate: {e}"))?;
        let ext0 = seg.extension.payload_out(&ext_tx, &format!("ancestor {i} extension"))?.clone();
        let st_tx: Transaction = deserialize(
            &hex::decode(&seg.state.signed_tx).map_err(|_| anyhow::anyhow!("ancestor {i}: bad state hex"))?,
        )
        .map_err(|_| anyhow::anyhow!("ancestor {i}: state is not a transaction"))?;
        let sin = st_tx.input.first().ok_or_else(|| anyhow::anyhow!("ancestor {i}: state has no input"))?;
        if st_tx.input.len() != 1
            || sin.previous_output.txid != ext_tx.txid()
            || sin.previous_output.vout != seg.extension.payload_vout
        {
            return Err(anyhow::anyhow!(
                "ancestor {i}: state does not spend its extension's payload output"
            ));
        }
        verify_tier_cosigned(&st_tx, ext0.value, &seg_spk)
            .map_err(|e| anyhow::anyhow!("ancestor {i}: state not co-signed by its aggregate: {e}"))?;
        // CSV bounds for both tiers.
        for (kind, tx) in [("extension", &ext_tx), ("state", &st_tx)] {
            let seq = tx.input[0].sequence.0;
            if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
                return Err(anyhow::anyhow!("ancestor {i} {kind}: not a BIP-68 block relative-timelock"));
            }
            let csv = seq as u16;
            let p = cb.parent.params;
            let (lo, hi) = if kind == "extension" { (p.e_floor, p.e0) } else { (p.d_floor, p.d0) };
            if csv < lo || csv > hi {
                return Err(anyhow::anyhow!("ancestor {i} {kind}: CSV {csv} outside [{lo},{hi}]"));
            }
        }
        // Superseded battery + exact-equality census for this segment (same shared logic as everywhere).
        let seg_superseded_ok = {
            let mut prevouts: std::collections::HashMap<(electrum_client::bitcoin::Txid, u32), u64> = std::collections::HashMap::new();
            prevouts.insert((fund_txid, seg.funding_vout), fund_out.value);
            for tx in [&ext_tx, &st_tx] {
                let id = tx.txid();
                for (v, o) in tx.output.iter().enumerate() {
                    prevouts.insert((id, v as u32), o.value);
                }
            }
            let mut live: std::collections::HashMap<(electrum_client::bitcoin::Txid, u32), u32> = std::collections::HashMap::new();
            // The census key MUST move with the payload vout it describes (CTESR-GATE §3.2): this map
            // is KEYED rather than content-checked, so a key that disagreed with its tier would make
            // the superseded battery compare a rival against the WRONG live CSV — the one migration
            // site that warrants explicit care. `st_tx` is asserted above to spend exactly this
            // outpoint, so key and tier can never drift.
            live.insert((fund_txid, seg.funding_vout), ext_tx.input[0].sequence.0 & 0xFFFF);
            live.insert(
                (ext_tx.txid(), seg.extension.payload_vout),
                st_tx.input[0].sequence.0 & 0xFFFF,
            );
            let live_ids: std::collections::HashSet<electrum_client::bitcoin::Txid> =
                [ext_tx.txid(), st_tx.txid()].into_iter().collect();
            verify_superseded_segment(
                &seg.superseded_states,
                &seg.superseded_extensions,
                &seg_spk,
                &cb.parent.params,
                &mut prevouts,
                &live,
                &live_ids,
            )
            .map_err(|e| anyhow::anyhow!("ancestor {i}: {e}"))?
        };
        let expected = CHILD_V2_BASELINE + 2 + seg_superseded_ok;
        if facts.num_sigs != expected {
            return Err(anyhow::anyhow!(
                "ancestor {i} num_sigs mismatch: SE issued {}, disclosed accounts for {expected} — possible hidden state",
                facts.num_sigs
            ));
        }
        cur_tx = st_tx;
    }

    let sp_txid = cur_tx.txid();
    // The funding tier's own payload vout — `SP.out[j]` means the j-th PAYLOAD output, so a child slot
    // can never sit before it (index 0 becomes the opret once a tier is coloured).
    let funding_payload_vout = cb
        .ancestors
        .last()
        .map(|a| a.state.payload_vout)
        .unwrap_or(cb.parent.current().state.payload_vout);
    if cb.sp_vout < funding_payload_vout {
        return Err(anyhow::anyhow!(
            "child funding vout {} precedes the funding tier's payload vout {funding_payload_vout}",
            cb.sp_vout
        ));
    }
    let sp_out = cur_tx
        .output
        .get(cb.sp_vout as usize)
        .ok_or_else(|| anyhow::anyhow!("funding tx has no output {}", cb.sp_vout))?;

    // [5] CHILD AGGREGATE AUTHORITY: A_child := SP.out[j].spk (parsed from SP, not declared). The
    //     server's recorded aggregate for child_sid must be non-NULL and == A_child. SP is un-broadcast
    //     so A_child has no on-chain root — its authority is the UNIQUE server registration (child_sid
    //     must be server-created at split time, not sender-chosen).
    let a_child = taproot_key_hex(sp_out.script_pubkey.as_bytes())?;
    let c_agg = child_aggregate_pubkey
        .ok_or_else(|| anyhow::anyhow!("server recorded no aggregate for child sid (fail-closed)"))?;
    if tweaked_key_hex(c_agg)? != a_child {
        return Err(anyhow::anyhow!("child sid's server aggregate != SP.out[j] key (decoy child)"));
    }

    // [6] CHILD SEGMENT + CENSUS, verified under A_child (attribution is KEY-DERIVED — check [7] — since
    //     each tier is verified against SP.out[j]'s key, not a sender-filled segment field).
    let child_agg_spk = sp_out.script_pubkey.clone();

    // ext_child spends exactly SP.out[j], co-signed by A_child.
    let ext_tx: Transaction = deserialize(
        &hex::decode(&cb.child_extension.signed_tx).map_err(|_| anyhow::anyhow!("bad child ext hex"))?,
    )
    .map_err(|_| anyhow::anyhow!("child extension is not a transaction"))?;
    let ext_in = ext_tx.input.first().ok_or_else(|| anyhow::anyhow!("child ext has no input"))?;
    if ext_tx.input.len() != 1 || ext_in.previous_output.txid != sp_txid || ext_in.previous_output.vout != cb.sp_vout {
        return Err(anyhow::anyhow!("child extension does not spend SP.out[j]"));
    }
    verify_tier_cosigned(&ext_tx, sp_out.value, &child_agg_spk)
        .map_err(|e| anyhow::anyhow!("child extension not co-signed by A_child: {e}"))?;
    // [F4] child extension CSV: a valid BIP-68 block relative-timelock within the extension schedule.
    {
        let seq = ext_tx.input[0].sequence.0;
        if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
            return Err(anyhow::anyhow!("child extension: not a BIP-68 block relative-timelock"));
        }
        let (csv, p) = (seq as u16, cb.parent.params);
        if csv < p.e_floor || csv > p.e0 {
            return Err(anyhow::anyhow!("child extension CSV {csv} outside [{},{}]", p.e_floor, p.e0));
        }
    }

    // state_child spends ext_child's PAYLOAD output, co-signed by A_child.
    let ext_out0 = cb.child_extension.payload_out(&ext_tx, "child extension")?;
    let st_tx: Transaction = deserialize(
        &hex::decode(&cb.child_state.signed_tx).map_err(|_| anyhow::anyhow!("bad child state hex"))?,
    )
    .map_err(|_| anyhow::anyhow!("child state is not a transaction"))?;
    let st_in = st_tx.input.first().ok_or_else(|| anyhow::anyhow!("child state has no input"))?;
    if st_tx.input.len() != 1
        || st_in.previous_output.txid != ext_tx.txid()
        || st_in.previous_output.vout != cb.child_extension.payload_vout
    {
        return Err(anyhow::anyhow!("child state does not spend ext_child's payload output"));
    }
    verify_tier_cosigned(&st_tx, ext_out0.value, &child_agg_spk)
        .map_err(|e| anyhow::anyhow!("child state not co-signed by A_child: {e}"))?;
    // [F4] child state CSV: a valid BIP-68 block relative-timelock within the state schedule.
    {
        let seq = st_tx.input[0].sequence.0;
        if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
            return Err(anyhow::anyhow!("child state: not a BIP-68 block relative-timelock"));
        }
        let (csv, p) = (seq as u16, cb.parent.params);
        if csv < p.d_floor || csv > p.d0 {
            return Err(anyhow::anyhow!("child state CSV {csv} outside [{},{}]", p.d_floor, p.d0));
        }
    }

    // MODEL A: the final child state must pay the RECEIVER's own key.
    let recv_spk = Address::from_str(receiver_backup_address)
        .map_err(|_| anyhow::anyhow!("bad receiver backup address"))?
        .require_network(net)
        .map_err(|_| anyhow::anyhow!("receiver backup address wrong network"))?
        .script_pubkey();
    let recv_key = taproot_key_hex(recv_spk.as_bytes())?;
    let st_out0 = cb.child_state.payload_out(&st_tx, "child state")?;
    if taproot_key_hex(st_out0.script_pubkey.as_bytes())? != recv_key {
        return Err(anyhow::anyhow!("child state does not pay the receiver's key (Model A violated)"));
    }
    // [value-binding — child value-gate spoof] Bind the receiver-paying output's VALUE to the bundle's
    // declared `out_value`. `verify_tier_cosigned` binds the co-sign to the INPUT amount, not the
    // output split, and the blind SE co-signs ANY output distribution — so without this a payer crafts
    // `state_child.out[0]` paying the receiver a few sats while declaring a large `out_value` (remainder
    // to a second output back to itself), and any value gate that trusts the declared field (the SSP's
    // pre-pay value gate — audit) pays the full invoice for a near-worthless piece. out[0] is forced to
    // be the receiver payment by the key check above (a P2A anchor or change can only be a LATER
    // output), so binding `out[0].value == out_value` makes `verify_conveyed_child`'s returned value
    // trustworthy. Live on the shipped child census (sdk59), not just non-exact LN.
    if st_out0.value != cb.child_state.out_value {
        return Err(anyhow::anyhow!(
            "child state out[0] pays {} sat but the bundle declares out_value {} — value-gate spoof",
            st_out0.value, cb.child_state.out_value
        ));
    }

    // [6 cont.] CHILD SUPERSEDED SEGMENT. A child that has been RE-TRANSFERRED discloses the states it
    // replaced (one per hop), each of which consumed a real co-sign and so must be counted — but only
    // after being proven non-confirmable, exactly like the root ladder's. Same shared battery, so the
    // two can never drift.
    //
    // Seeding differs from `verify_bundle_ex` in one way worth stating: that function seeds `live` from
    // `txs[1..]` because a root ladder's first tier is the TRIGGER, which carries no CSV. A child
    // segment is HEADLESS — it starts at its extension, which does have a CSV — so BOTH child tiers
    // seed the live map.
    let child_superseded_ok = {
        use electrum_client::bitcoin::Txid as _Txid;
        let mut child_prevouts: std::collections::HashMap<(_Txid, u32), u64> =
            std::collections::HashMap::new();
        child_prevouts.insert((sp_txid, cb.sp_vout), sp_out.value);
        for tx in [&ext_tx, &st_tx] {
            let id = tx.txid();
            for (vout, o) in tx.output.iter().enumerate() {
                child_prevouts.insert((id, vout as u32), o.value);
            }
        }
        let mut child_live: std::collections::HashMap<(_Txid, u32), u32> =
            std::collections::HashMap::new();
        // Same rule as the ancestor segment: the KEYED census map must track the payload vout of the
        // tier it describes, or a rival would be raced against the wrong live CSV (CTESR-GATE §3.2).
        child_live.insert((sp_txid, cb.sp_vout), ext_tx.input[0].sequence.0 & 0xFFFF);
        child_live.insert(
            (ext_tx.txid(), cb.child_extension.payload_vout),
            st_tx.input[0].sequence.0 & 0xFFFF,
        );
        let child_live_ids: std::collections::HashSet<_Txid> =
            [ext_tx.txid(), st_tx.txid()].into_iter().collect();
        verify_superseded_segment(
            &cb.child_superseded_states,
            &cb.child_superseded_extensions,
            &child_agg_spk,
            &cb.parent.params,
            &mut child_prevouts,
            &child_live,
            &child_live_ids,
        )?
    };

    // [6 cont.] CHILD CENSUS exact-equality: the child discloses exactly ext_child + state_child (2
    //     co-signs) plus one superseded state per onward hop, on top of any flat backups (a derived slot
    //     has none — CHILD_V2_BASELINE = 0). A hidden child co-sign would push child_num_sigs above
    //     this ⟹ reject. Key handovers are census-NEUTRAL (the enclave bumps sig_count only when it
    //     signs), so an adopted child counts the same as a conveyed one.
    let child_expected = child_flat_backups + 2 + child_superseded_ok;
    if child_num_sigs != child_expected {
        return Err(anyhow::anyhow!(
            "child num_sigs mismatch: SE issued {child_num_sigs}, disclosed accounts for {child_expected} — possible hidden child state"
        ));
    }

    Ok(())
}

/// Verify a tier tx carries a genuine SE/owner co-signature by the aggregate key `A` — i.e. that it
/// actually consumed one of the SE's finalized signatures. Consensus-style: the taproot key-spend
/// signature must verify against the OUTPUT key in `agg_spk`'s witness program over the sighash that
/// `cosign_tier_request` commits to (`TxOut { value: prevout_value, script_pubkey: agg_spk }`,
/// `TapSighashType::All`). Without this, a structurally valid but never-co-signed tx could pad
/// `verify_bundle`'s count and mask a hidden state [S1].
fn verify_tier_cosigned(
    tx: &electrum_client::bitcoin::Transaction,
    prevout_value: u64,
    agg_spk: &electrum_client::bitcoin::ScriptBuf,
) -> Result<()> {
    use electrum_client::bitcoin::{
        secp256k1::{schnorr, Message, Secp256k1, XOnlyPublicKey},
        sighash::{Prevouts, SighashCache, TapSighashType},
        TxOut,
    };

    let prog = agg_spk.as_bytes();
    if prog.len() != 34 || prog[0] != 0x51 || prog[1] != 0x20 {
        return Err(anyhow::anyhow!("aggregate address is not a v1 taproot output"));
    }
    let output_key = XOnlyPublicKey::from_slice(&prog[2..34])
        .map_err(|_| anyhow::anyhow!("bad taproot output key"))?;

    let prevout = TxOut { value: prevout_value, script_pubkey: agg_spk.clone() };
    let sighash = SighashCache::new(tx)
        .taproot_key_spend_signature_hash(0, &Prevouts::All(&[prevout]), TapSighashType::All)
        .map_err(|e| anyhow::anyhow!("sighash: {e}"))?;

    let wit = tx.input[0].witness.to_vec();
    if wit.len() != 1 {
        return Err(anyhow::anyhow!("witness is not a single key-spend signature"));
    }
    let sig = match wit[0].len() {
        64 => schnorr::Signature::from_slice(&wit[0]),
        65 => {
            if wit[0][64] != TapSighashType::All as u8 {
                return Err(anyhow::anyhow!("tier is not SIGHASH_ALL"));
            }
            schnorr::Signature::from_slice(&wit[0][..64])
        }
        n => return Err(anyhow::anyhow!("bad signature length {n}")),
    }
    .map_err(|_| anyhow::anyhow!("malformed schnorr signature"))?;

    let msg = Message::from_slice(sighash.as_ref()).map_err(|_| anyhow::anyhow!("bad sighash"))?;
    Secp256k1::verification_only()
        .verify_schnorr(&sig, &msg, &output_key)
        .map_err(|_| anyhow::anyhow!("signature does not verify against the aggregate key A"))
}

#[cfg(test)]
mod verify_tests {
    use super::*;

    const AGG: &str = "bcrt1p83afnxgnczlsqvd20swjlnr3kcm7hvz9p338dgueetjz2tx6vvjs05rsfy";
    const OWNER: &str = "bcrt1qv23qwf82jw5k68juxnlxx06yz8plu0mrfrqvws";
    const F_TXID: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    // A schedule-conformant single-level bundle (unsigned tiers — verify_bundle checks structure).
    pub(super) fn sample_bundle() -> TesrBundle {
        let p = mercurylib::tesr::TesrParams::regtest();
        let f_value = 100_000u64;
        let t = mercurylib::tesr::build_trigger(F_TXID, 0, f_value, AGG, "regtest", p.committed_fee_rate).unwrap();
        let x = mercurylib::tesr::build_extension(&t.txid, t.out_value, AGG, "regtest", p.ext_csv(0), p.committed_fee_rate).unwrap();
        let s = mercurylib::tesr::build_state(&x.txid, x.out_value, OWNER, "regtest", p.state_csv(0), p.committed_fee_rate).unwrap();
        TesrBundle {
            version: 1, statechain_id: "sid".into(), network: "regtest".into(),
            fee_rate: p.committed_fee_rate, agg_address: AGG.into(), owner_exit_address: OWNER.into(),
            f_txid: F_TXID.into(), f_vout: 0, f_value,
            trigger: TesrTier { txid: t.txid, signed_tx: t.tx_hex, out_value: t.out_value, csv: None, payload_vout: t.payload_vout },
            levels: vec![TesrLevel {
                extension: TesrTier { txid: x.txid, signed_tx: x.tx_hex, out_value: x.out_value, csv: Some(p.ext_csv(0)), payload_vout: x.payload_vout },
                state: TesrTier { txid: s.txid, signed_tx: s.tx_hex, out_value: s.out_value, csv: Some(p.state_csv(0)), payload_vout: s.payload_vout },
            }],
            m: 0, superseded_states: vec![], superseded_extensions: vec![], params: p,
        }
    }

    /// PRE-EXISTING, corrected (not weakened): this used to assert `verify_bundle(&b, 3, 0).is_ok()`
    /// and had been RED since the `[S1]` fix, because `sample_bundle` builds UNSIGNED tiers and every
    /// counted tier must now verify as a genuine co-sign by `A`. A structurally perfect but
    /// never-co-signed ladder must be REFUSED — that is the property, and it is what is asserted here.
    /// The positive control (an honest, live-SE-co-signed ladder is ACCEPTED) cannot be built without
    /// the SE and lives in the E2Es: sdk46, sdk48, sdk54 and sdk70.
    #[test]
    fn rejects_a_structurally_perfect_but_uncosigned_ladder() {
        let b = sample_bundle();
        // trigger + extension + state = 3 tiers, 0 flat backups — the COUNT balances exactly, so the
        // only thing left to refuse on is the absent signature.
        let e = verify_bundle(&b, 3, 0).expect_err("an un-co-signed ladder must never be accepted");
        assert!(
            e.to_string().contains("not co-signed by A"),
            "must reject on the missing co-sign, got: {e}"
        );
    }

    /// The scriptPubKey of the sample bundle's aggregate address, hex — what a coin's on-chain
    /// funding output would carry.
    fn agg_spk_hex() -> String {
        use electrum_client::bitcoin::{Address, Network};
        let spk = Address::from_str(AGG)
            .unwrap()
            .require_network(Network::Regtest)
            .unwrap()
            .script_pubkey();
        hex::encode(spk.as_bytes())
    }

    /// The authority a receiver derives from the coin the sample bundle claims to describe.
    fn sample_authority() -> CoinAuthority {
        CoinAuthority {
            statechain_id: "sid".into(),
            f_txid: F_TXID.into(),
            f_vout: 0,
            f_value: 100_000,
            f_spk_hex: agg_spk_hex(),
            // A real one is the coordinator's untweaked aggregate; the tests below all fire on checks
            // that precede it, or on its absence.
            se_aggregate_pubkey: None,
        }
    }

    fn reject_msg(r: Result<()>) -> String {
        r.expect_err("must be rejected").to_string()
    }

    // ---- payload_vout: every migrated site fails CLOSED with a named error (CTES-R commit 1). ----
    //
    // These run BEFORE the signature battery, so they are exercisable without the SE — which is the
    // point: a wrong payload vout is caught structurally, early, and loudly.

    #[test]
    fn wrong_trigger_payload_vout_is_rejected() {
        let mut b = sample_bundle();
        b.trigger.payload_vout = 1; // the P2A anchor, not the payload
        assert!(reject_msg(verify_bundle(&b, 3, 0)).contains("does not pay the aggregate key A"));
    }

    #[test]
    fn out_of_range_payload_vout_is_rejected_not_defaulted() {
        let mut b = sample_bundle();
        b.trigger.payload_vout = 9;
        let e = reject_msg(verify_bundle(&b, 3, 0));
        assert!(e.contains("out of range"), "must fail closed on the accessor, got: {e}");
    }

    #[test]
    fn wrong_extension_payload_vout_is_rejected() {
        let mut b = sample_bundle();
        b.levels[0].extension.payload_vout = 1;
        assert!(reject_msg(verify_bundle(&b, 3, 0)).contains("tier 1 pays the wrong output"));
    }

    #[test]
    fn wrong_final_state_payload_vout_is_rejected() {
        let mut b = sample_bundle();
        b.levels[0].state.payload_vout = 1;
        assert!(reject_msg(verify_bundle(&b, 3, 0)).contains("tier 2 pays the wrong output"));
    }

    #[test]
    fn child_spending_a_non_payload_output_of_its_parent_is_rejected() {
        // Isolate the LINKAGE check: give the trigger a payload that really does sit at index 1 (swap
        // its two outputs) so the payee check passes, then hang the extension off `out[0]`. The chain
        // is broken and the linkage check — not the payee check — must say so.
        use electrum_client::bitcoin::{
            consensus::{deserialize, serialize},
            Transaction,
        };
        let mut b = sample_bundle();
        let mut t: Transaction = deserialize(&hex::decode(&b.trigger.signed_tx).unwrap()).unwrap();
        t.output.swap(0, 1);
        let t_txid = t.txid().to_string();
        let payload_value = t.output[1].value;
        let p = b.params;
        let x = mercurylib::tesr::build_extension(
            &t_txid, payload_value, AGG, "regtest", p.ext_csv(0), p.committed_fee_rate,
        )
        .unwrap();
        b.trigger = TesrTier {
            txid: t_txid,
            signed_tx: hex::encode(serialize(&t)),
            out_value: payload_value,
            csv: None,
            payload_vout: 1,
        };
        b.levels[0].extension = TesrTier {
            txid: x.txid,
            signed_tx: x.tx_hex,
            out_value: x.out_value,
            csv: Some(p.ext_csv(0)),
            payload_vout: x.payload_vout,
        };
        let e = reject_msg(verify_bundle(&b, 3, 0));
        assert!(e.contains("does not spend its parent's payload output"), "got: {e}");
    }

    // ---- [C-1] the acceptance-path verifier is bound to the COIN, not to the bundle. ----

    #[test]
    fn bound_verifier_rejects_a_ladder_for_another_statechain_id() {
        let b = sample_bundle();
        let mut coin = sample_authority();
        coin.statechain_id = "a-different-sid".into();
        assert!(reject_msg(verify_bundle_bound(&b, 3, 0, &coin)).contains("statechain id"));
    }

    #[test]
    fn bound_verifier_rejects_a_ladder_over_another_outpoint() {
        let b = sample_bundle();
        let mut coin = sample_authority();
        coin.f_txid = "2".repeat(64);
        assert!(reject_msg(verify_bundle_bound(&b, 3, 0, &coin)).contains("decoy ladder"));
        let mut coin = sample_authority();
        coin.f_vout = 7;
        assert!(reject_msg(verify_bundle_bound(&b, 3, 0, &coin)).contains("decoy ladder"));
    }

    #[test]
    fn bound_verifier_rejects_a_restated_funding_value() {
        let b = sample_bundle();
        let mut coin = sample_authority();
        coin.f_value += 1;
        assert!(reject_msg(verify_bundle_bound(&b, 3, 0, &coin)).contains("F value"));
    }

    #[test]
    fn bound_verifier_rejects_an_aggregate_that_is_not_the_coins() {
        use electrum_client::bitcoin::{
            secp256k1::{Secp256k1, XOnlyPublicKey},
            Address, Network,
        };
        let b = sample_bundle();
        let mut coin = sample_authority();
        // A different, perfectly valid P2TR funding output — the ladder's tiers are co-signed under a
        // key that does not control it.
        let x = XOnlyPublicKey::from_slice(
            &hex::decode("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798").unwrap(),
        )
        .unwrap();
        coin.f_spk_hex = hex::encode(
            Address::p2tr(&Secp256k1::verification_only(), x, None, Network::Regtest)
                .script_pubkey()
                .as_bytes(),
        );
        let e = reject_msg(verify_bundle_bound(&b, 3, 0, &coin));
        assert!(e.contains("on-chain funding key"), "got: {e}");
    }

    #[test]
    fn bound_verifier_rejects_a_non_taproot_funding_output() {
        // `taproot_key_hex` on a non-P2TR spk — here the P2A anchor script, a witness program that is
        // NOT P2TR (the exact shape that broke `create_colored_split_tx`'s filter, CTESR-GATE §2.1d).
        let b = sample_bundle();
        let mut coin = sample_authority();
        coin.f_spk_hex = "51024e73".into();
        let e = reject_msg(verify_bundle_bound(&b, 3, 0, &coin));
        assert!(e.contains("not a v1 taproot output"), "got: {e}");
    }

    #[test]
    fn bound_verifier_fails_closed_without_a_coordinator_aggregate() {
        // Everything else matches; the coordinator simply has no aggregate on record for the sid.
        // That is a REJECTION, never a fallback to the sender-supplied key.
        let b = sample_bundle();
        let coin = sample_authority();
        assert!(coin.se_aggregate_pubkey.is_none());
        let e = reject_msg(verify_bundle_bound(&b, 3, 0, &coin));
        assert!(e.contains("recorded no aggregate"), "got: {e}");
    }

    // ---- [R5] The establish-time counterpart: a legacy coin is left UN-LADDERED, not bricked. ----
    //
    // `verify_bundle_bound`'s fail-closed-on-missing-aggregate is correct and stays exactly as it is.
    // Its cost is that a pre-0009 coin (`aggregate_xonly IS NULL`) which gets laddered in place carries
    // a bundle no receiver can bind. `ladder_binding_precheck` is the same authority test hoisted ahead
    // of establishment, so such a coin never acquires that ladder — and never burns the three
    // irreversible SE co-signs it would cost. It does NOT restore off-chain transferability (the [R4]
    // version floor refuses the un-laddered shape too); only a coordinator-side backfill of
    // `aggregate_xonly` does that.

    /// The generator's x-coordinate — a valid x-only key standing in for "some other aggregate".
    const OTHER_XONLY: &str =
        "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    /// P2TR spk (hex) of an UNTWEAKED x-only key — what the coordinator's recorded aggregate looks
    /// like once it is on chain.
    fn p2tr_spk_hex_of(untweaked_xonly: &str) -> String {
        use electrum_client::bitcoin::{
            secp256k1::{Secp256k1, XOnlyPublicKey},
            Address, Network,
        };
        let x = XOnlyPublicKey::from_str(untweaked_xonly).unwrap();
        hex::encode(
            Address::p2tr(&Secp256k1::verification_only(), x, None, Network::Regtest)
                .script_pubkey()
                .as_bytes(),
        )
    }

    #[test]
    fn precheck_refuses_to_ladder_a_legacy_coin_with_no_coordinator_aggregate() {
        // A perfectly legitimate, live, on-chain root coin — the coordinator simply predates the
        // aggregate column. Laddering it would spend three SE co-signs to produce an unclaimable
        // bundle, so the pass must leave it alone.
        let e = ladder_binding_precheck("sid-legacy", &agg_spk_hex(), None, "regtest")
            .expect_err("a coin with no coordinator aggregate must never be laddered");
        assert!(e.to_string().contains("recorded no aggregate"), "got: {e}");

        // Why it must never be laddered: this is precisely the coin whose conveyed ladder the
        // acceptance path refuses. The refusal is NOT relaxed — the fix is upstream of it.
        let b = sample_bundle();
        let legacy = sample_authority(); // se_aggregate_pubkey: None
        assert!(legacy.se_aggregate_pubkey.is_none());
        assert!(
            reject_msg(verify_bundle_bound(&b, 3, 0, &legacy)).contains("recorded no aggregate"),
            "the acceptance-path fail-closed must stand unchanged"
        );
    }

    #[test]
    fn precheck_refuses_an_aggregate_that_does_not_control_the_funding_output() {
        // Not the legacy case: the coordinator HAS a record, it just is not the key on chain. A ladder
        // here would be refused as a decoy at acceptance, so it must not be built either.
        let e = ladder_binding_precheck("sid", &agg_spk_hex(), Some(OTHER_XONLY), "regtest")
            .expect_err("a mismatched coordinator aggregate must not be laddered");
        assert!(e.to_string().contains("does not match the funding output key"), "got: {e}");
    }

    #[test]
    fn precheck_fails_closed_on_a_non_taproot_funding_output() {
        let e = ladder_binding_precheck("sid", "51024e73", Some(OTHER_XONLY), "regtest")
            .expect_err("a non-P2TR funding output has no aggregate key to bind to");
        assert!(e.to_string().contains("not a v1 taproot output"), "got: {e}");
        assert!(
            ladder_binding_precheck("sid", "zz", Some(OTHER_XONLY), "regtest").is_err(),
            "unparseable spk must fail closed"
        );
    }

    /// The four refusal causes are DISTINGUISHABLE. A caller that collapses them into `is_err()`
    /// licenses "decoy-shaped coin" and "unreadable spk" as if they were the harmless legacy case —
    /// which is exactly the fail-open the flat-conveyance classifier used to have.
    #[test]
    fn precheck_reports_a_distinguishable_cause_for_every_refusal() {
        use BindingRefusal::*;
        let cases: &[(BindingRefusal, &str, Option<&str>)] = &[
            (NoCoordinatorAggregate, &"", None),
            (AggregateMismatch, &"", Some(OTHER_XONLY)),
            (FundingNotTaproot, "51024e73", Some(OTHER_XONLY)),
            (FundingSpkUnparseable, "zz", Some(OTHER_XONLY)),
        ];
        let agg = agg_spk_hex();
        for (expected, spk, se_agg) in cases {
            let spk = if spk.is_empty() { agg.as_str() } else { *spk };
            let e = ladder_binding_precheck_cause("sid", spk, *se_agg, "regtest")
                .expect_err("this shape must not be ladderable");
            assert_eq!(e.cause, *expected, "wrong cause for spk={spk} agg={se_agg:?}: {e}");
            // The untyped wrapper keeps producing the exact same prose.
            assert_eq!(
                ladder_binding_precheck("sid", spk, *se_agg, "regtest").unwrap_err().to_string(),
                e.message
            );
        }
        // ...and ONLY the legacy case is the permanent one a caller may license.
        assert_ne!(AggregateMismatch, NoCoordinatorAggregate);
    }

    #[test]
    fn precheck_passes_exactly_when_the_receiver_could_bind_the_ladder() {
        // The coordinator's untweaked aggregate, and the funding output that key actually controls.
        let f_spk_hex = p2tr_spk_hex_of(OTHER_XONLY);
        ladder_binding_precheck("sid", &f_spk_hex, Some(OTHER_XONLY), "regtest")
            .expect("a coin whose coordinator aggregate controls F is ladderable");

        // And the acceptance-path verifier agrees on the same coin: a ladder built over that aggregate
        // clears EVERY binding check and stops only at the SE co-signature, which no unit test can
        // forge. So `precheck ⟹ bindable`: the establish gate never creates an unclaimable ladder.
        use electrum_client::bitcoin::{
            secp256k1::{Secp256k1, XOnlyPublicKey},
            Address, Network,
        };
        let agg_addr = Address::p2tr(
            &Secp256k1::verification_only(),
            XOnlyPublicKey::from_str(OTHER_XONLY).unwrap(),
            None,
            Network::Regtest,
        )
        .to_string();
        let p = mercurylib::tesr::TesrParams::regtest();
        let f_value = 100_000u64;
        let t = mercurylib::tesr::build_trigger(F_TXID, 0, f_value, &agg_addr, "regtest", p.committed_fee_rate).unwrap();
        let x = mercurylib::tesr::build_extension(&t.txid, t.out_value, &agg_addr, "regtest", p.ext_csv(0), p.committed_fee_rate).unwrap();
        let s = mercurylib::tesr::build_state(&x.txid, x.out_value, OWNER, "regtest", p.state_csv(0), p.committed_fee_rate).unwrap();
        let mut b = sample_bundle();
        b.agg_address = agg_addr;
        b.trigger = TesrTier { txid: t.txid, signed_tx: t.tx_hex, out_value: t.out_value, csv: None, payload_vout: t.payload_vout };
        b.levels[0].extension = TesrTier { txid: x.txid, signed_tx: x.tx_hex, out_value: x.out_value, csv: Some(p.ext_csv(0)), payload_vout: x.payload_vout };
        b.levels[0].state = TesrTier { txid: s.txid, signed_tx: s.tx_hex, out_value: s.out_value, csv: Some(p.state_csv(0)), payload_vout: s.payload_vout };

        let coin = CoinAuthority {
            statechain_id: "sid".into(),
            f_txid: F_TXID.into(),
            f_vout: 0,
            f_value,
            f_spk_hex,
            se_aggregate_pubkey: Some(OTHER_XONLY.into()),
        };
        let e = reject_msg(verify_bundle_bound(&b, 3, 0, &coin));
        assert!(
            e.contains("not co-signed by A"),
            "binding must not be what fails for a precheck-passing coin, got: {e}"
        );
    }

    // ---- [C-2] one co-sign, one census slot. ----

    /// The dedup guard sits in `verify_superseded_segment`'s PRE-PASS, ahead of the co-sign battery,
    /// so it is exercisable without the SE — which matters, because the padding it stops is padding
    /// with GENUINE, fully co-signed tiers that every other check waves through. (The end-to-end
    /// version, against a real renewed ladder, is sdk70 PART C.)
    fn superseded_dedup(sup: &[TesrTier], live: &[TesrTier]) -> String {
        use electrum_client::bitcoin::{Address, Network, Txid};
        let b = sample_bundle();
        let agg_spk = Address::from_str(AGG)
            .unwrap()
            .require_network(Network::Regtest)
            .unwrap()
            .script_pubkey();
        let mut prevouts: std::collections::HashMap<(Txid, u32), u64> =
            std::collections::HashMap::new();
        let live_csv: std::collections::HashMap<(Txid, u32), u32> = std::collections::HashMap::new();
        let live_txids: std::collections::HashSet<Txid> = live
            .iter()
            .map(|t| Txid::from_str(&t.txid).unwrap())
            .collect();
        verify_superseded_segment(
            sup,
            &[],
            &agg_spk,
            &b.params,
            &mut prevouts,
            &live_csv,
            &live_txids,
        )
        .expect_err("a repeated tier must be rejected")
        .to_string()
    }

    #[test]
    fn a_disclosed_tier_cannot_be_counted_twice() {
        // Repeat a superseded entry: `expected` grows by one for free and absorbs a hidden co-sign.
        let s = sample_bundle().levels[0].state.clone();
        let e = superseded_dedup(&[s.clone(), s], &[]);
        assert!(e.contains("disclosed more than once"), "got: {e}");
    }

    #[test]
    fn a_live_tier_cannot_also_be_disclosed_as_superseded() {
        // Already counted once by `tiers.len()`; re-declaring it buys the same free slot.
        let b = sample_bundle();
        let x = b.levels[0].extension.clone();
        let e = superseded_dedup(&[x.clone()], &[x]);
        assert!(e.contains("disclosed more than once"), "got: {e}");
    }

    #[test]
    fn rejects_hidden_extra_sig() {
        let b = sample_bundle();
        // SE issued one MORE sig than the ladder accounts for → a hidden state → reject.
        assert!(verify_bundle(&b, 4, 0).is_err());
    }

    #[test]
    fn rejects_undercount() {
        let b = sample_bundle();
        assert!(verify_bundle(&b, 2, 0).is_err());
    }

    #[test]
    fn rejects_broken_prevout_link() {
        let mut b = sample_bundle();
        // Point the extension at the wrong parent by corrupting the trigger txid it references:
        // rebuild the extension off a bogus parent.
        let p = b.params;
        let bogus = mercurylib::tesr::build_extension(
            "2222222222222222222222222222222222222222222222222222222222222222",
            b.trigger.out_value, AGG, "regtest", p.ext_csv(0), p.committed_fee_rate,
        ).unwrap();
        b.levels[0].extension = TesrTier { txid: bogus.txid, signed_tx: bogus.tx_hex, out_value: bogus.out_value, csv: Some(p.ext_csv(0)), payload_vout: bogus.payload_vout };
        assert!(verify_bundle(&b, 3, 0).is_err(), "extension not linked to the trigger must be rejected");
    }

    #[test]
    fn rejects_final_state_not_paying_owner() {
        let p = mercurylib::tesr::TesrParams::regtest();
        let mut b = sample_bundle();
        // Rebuild the final state paying A instead of the owner.
        let x = &b.levels[0].extension;
        let s = mercurylib::tesr::build_state(&x.txid, x.out_value, AGG, "regtest", p.state_csv(0), p.committed_fee_rate).unwrap();
        b.levels[0].state = TesrTier { txid: s.txid, signed_tx: s.tx_hex, out_value: s.out_value, csv: Some(p.state_csv(0)), payload_vout: s.payload_vout };
        assert!(verify_bundle(&b, 3, 0).is_err(), "final state must pay the owner");
    }
}

/// Blind-co-sign one TES-R tier transaction end-to-end against the SE and return the signed tx hex.
///
/// * `unsigned_tx_hex` — a tier tx built by `mercurylib::tesr::build_{trigger,extension,state}`.
/// * `prevout_value`   — the value of the output this tier spends (the parent tier's `out[0]`, or the
///   funding UTXO value for the trigger). Every tier's prevout is a P2TR of the coin's aggregate key.
///
/// Fresh MuSig2 nonces are committed per call, so co-signing several tiers of one coin never reuses a
/// secnonce (the SE would refuse a reused nonce with 409 anyway).
pub async fn cosign_tier(
    client_config: &ClientConfig,
    coin: &mut Coin,
    unsigned_tx_hex: String,
    prevout_value: u64,
    network: &str,
) -> Result<String> {
    let coin_nonce = mercurylib::transaction::create_and_commit_nonces(coin)?;
    coin.secret_nonce = Some(coin_nonce.secret_nonce);
    coin.public_nonce = Some(coin_nonce.public_nonce);
    coin.blinding_factor = Some(coin_nonce.blinding_factor);

    let server_public_nonce =
        sign_first(client_config, &coin_nonce.sign_first_request_payload).await?;
    coin.server_public_nonce = Some(server_public_nonce);

    let partial = cosign_tier_request(coin, unsigned_tx_hex, prevout_value, network.to_string())?;

    // [KEYSTONE / client half] Retry the SAME sign/second — NEVER restart sign/first here.
    //
    // Restarting sign/first would mint a fresh secnonce and a fresh SE co-sign, so sig_count would run
    // ahead of the one tier we actually keep, and the receiver census (num_sigs == flat_backups + tiers +
    // superseded) could never rebalance ⟹ the coin bricks. Resending the IDENTICAL payload is idempotent
    // at the lockbox (same session ⟹ cached partial sig, no re-sign, no re-increment — see the lockbox
    // signed_session_cache), so a lost sign/second response is recovered with the exact same signature.
    //
    // Retrying the same session is ALWAYS safe: it can only return the already-produced sig, produce it
    // once (secnonce still sealed), or 400 (a different session already consumed the secnonce — which
    // cannot happen for THIS session). If every attempt fails, we surface the error; the only
    // unrecoverable case (a crash between the enclave consume and the atomic store) left NO count
    // increment, so a caller that restarts the whole cosign is also safe. Bounded so a genuinely down SE
    // still returns promptly.
    let mut server_partial_sig = None;
    let mut last_err = None;
    for attempt in 0u32..5 {
        match sign_second(client_config, &partial.partial_signature_request_payload).await {
            Ok(sig) => {
                server_partial_sig = Some(sig);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < 4 {
                    tokio::time::sleep(std::time::Duration::from_millis(300 * (attempt as u64 + 1))).await;
                }
            }
        }
    }
    let server_partial_sig = match server_partial_sig {
        Some(sig) => sig,
        None => return Err(anyhow::anyhow!(
            "sign/second failed after retries (session unchanged, no double-count): {}",
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )),
    };

    let signature = create_signature(
        partial.msg,
        partial.client_partial_sig,
        hex::encode(server_partial_sig.serialize()),
        partial.encoded_session,
        partial.output_pubkey,
    )?;

    let signed_tx = new_backup_transaction(partial.encoded_unsigned_tx, signature)?;
    Ok(signed_tx)
}

/// **F4 — a blind pass must never look like an idle one.**
///
/// These are the regression tests for the external review's finding that the TES-R watch/exit
/// passes returned success-like defaults (`false` from every chain read, an empty `Vec<String>`
/// from the pass) whenever the backend failed. An empty result was the *only* signal available to a
/// caller, and a healthy quiet coin produced exactly the same one — so a tower with a dead electrum
/// connection reported "all quiet" while defending nothing, during precisely the race window in
/// which being blind loses the coin.
#[cfg(test)]
mod watch_visibility_tests {
    use super::*;
    use super::verify_tests::sample_bundle;

    /// An electrum backend that is REACHABLE but unusable: it completes the TCP handshake and then
    /// hangs up on every request. `Client::new` performs no handshake, so the client builds fine and
    /// each RPC fails at the transport — the realistic "the backend fell over" shape, and the one
    /// the old code silently read as "F is unspent / the tx is not on chain".
    fn dead_electrum() -> electrum_client::Client {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream); // accept, then immediately hang up
            }
        });
        electrum_client::Client::new(&format!("tcp://127.0.0.1:{port}")).expect("connect")
    }

    /// THE finding, stated as an assertion: with an unreadable backend the pass reports `Blind`,
    /// and `Blind` is *not* `Idle` even though both carry no ids. Before F4 this returned `vec![]`
    /// — byte-identical to the healthy "F unspent, nothing to defend" answer.
    #[test]
    fn a_blind_backend_reports_blind_not_idle() {
        let state = watch_pass(&dead_electrum(), &sample_bundle());
        assert!(state.is_blind(), "an unreadable backend must report Blind, got {state:?}");
        assert!(!state.is_idle(), "Blind must never satisfy is_idle — that is the whole finding");
        assert!(
            state.ids().is_empty(),
            "a blind pass acted on nothing, so emptiness alone can never mean 'all quiet'"
        );
        assert!(state.blind_reason().is_some(), "the cause must be reportable to the owner");
    }

    /// The positive control the assertion above needs: `Idle` is reserved for a pass that actually
    /// READ the chain. Nothing in the code can produce it from a failure.
    #[test]
    fn idle_is_only_ever_a_positive_observation() {
        assert!(WatchState::Idle.is_idle() && !WatchState::Idle.is_blind());
        let blind = WatchState::Blind { reason: "backend down".into() };
        assert!(!blind.is_idle() && blind.is_blind());
        assert_ne!(WatchState::Idle, blind);
        // An `Acted` pass that broadcast nothing (every mature tier already out) is ALSO not idle:
        // the coin is triggered and the exit is under way.
        let under_way = WatchState::Acted { ids: vec![], failures: vec!["csv not met".into()] };
        assert!(!under_way.is_idle() && !under_way.is_blind());
        assert_eq!(under_way.failures().len(), 1);
    }

    /// The owner-driven exit must fail LOUD rather than report `complete: false, wait 0` — which an
    /// app renders as a healthy "just wait for the next block".
    #[test]
    fn a_blind_backend_makes_the_exit_pass_an_error_not_progress() {
        let e = exit_pass(&dead_electrum(), &sample_bundle())
            .expect_err("an unreadable backend must not be reported as exit progress");
        assert!(
            e.to_string().contains("chain backend unreadable"),
            "the error must name the blindness, got: {e}"
        );
    }

    /// ... and the wait-time hint must never fabricate a number, nor claim completion.
    #[test]
    fn a_blind_backend_makes_the_wait_hint_an_error_not_a_number() {
        let r = next_exit_tier(&dead_electrum(), &sample_bundle());
        assert!(r.is_err(), "a blind backend must not yield a wait time, got {r:?}");
    }

    /// The classifier that draws the line. A server that ANSWERS "no such transaction" gave a real,
    /// trustworthy negative; anything else — transport failure, retry exhaustion, or a server error
    /// we do not recognise — is blindness. It fails CLOSED, so an unfamiliar message costs an alert,
    /// never a silent "absent".
    #[test]
    fn only_an_explicit_no_such_transaction_counts_as_absence() {
        use electrum_client::Error;
        // The verbatim bitcoind/electrs reply, confirmed against the live regtest backend.
        assert!(is_missing_tx_error(&Error::Protocol(serde_json::json!({
            "code": 2,
            "message": "No such mempool or blockchain transaction. Use gettransaction for wallet transactions."
        }))));
        assert!(is_missing_tx_error(&Error::Protocol(serde_json::json!("transaction not found"))));

        // A server error that is NOT a not-found: the tx may well exist and we simply cannot see it.
        assert!(!is_missing_tx_error(&Error::Protocol(serde_json::json!({
            "code": -32000, "message": "excessive resource usage"
        }))));
        assert!(!is_missing_tx_error(&Error::Protocol(serde_json::json!({
            "code": -32603, "message": "daemon error"
        }))));
        // Transport / client-side failures are never an answer at all.
        assert!(!is_missing_tx_error(&Error::IOError(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset"
        ))));
        assert!(!is_missing_tx_error(&Error::AllAttemptsErrored(vec![])));
        assert!(!is_missing_tx_error(&Error::Message("no idea".into())));
        assert!(!is_missing_tx_error(&Error::CouldntLockReader));
        assert!(!is_missing_tx_error(&Error::Mpsc));
    }

    /// Stored material that cannot be parsed is a failure too, not a quiet skip: the old code
    /// `break`-ed out of the tier loop on a bad txid/hex and returned the tiers it happened to have
    /// broadcast, indistinguishable from a completed pass.
    #[test]
    fn unusable_stored_material_is_surfaced_not_skipped() {
        let mut b = sample_bundle();
        b.f_txid = "not-a-txid".into();
        let state = watch_pass(&dead_electrum(), &b);
        assert!(state.is_blind(), "an unusable funding txid must be reported, got {state:?}");
        assert!(
            state.blind_reason().unwrap().contains("unusable funding txid"),
            "the reason must name the bad material, got {:?}",
            state.blind_reason()
        );
    }
}
