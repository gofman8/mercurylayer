//! Client-side driver for TES-R (Utexo V2) tier co-signing against the live blind SE.
//!
//! The SE is unchanged: it blind-co-signs whatever sighash the client presents (`/sign/first` +
//! `/sign/second`), so a tier tx (v3, relative-timelock, P2A anchor) round-trips through exactly the
//! same MuSig2 flow as a V1 backup. This module wires [`mercurylib::tesr::cosign_tier_request`] into
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
    /// The value this tier pays forward (its `out[0]`) — the prevout the child tier spends.
    pub out_value: u64,
    /// The relative-timelock (BIP-68 blocks) on this tier's input; `None` for the trigger.
    pub csv: Option<u16>,
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
    /// The relative-timelock schedule this coin runs on (mainnet/regtest preset). Drives the
    /// param-based cadence in [`renew_auto`] / [`rollover_auto`]; defaults to mainnet for bundles
    /// persisted before this field existed.
    #[serde(default)]
    pub params: mercurylib::tesr::TesrParams,
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
        trigger: TesrTier { txid: t.txid, signed_tx: t_signed, out_value: t.out_value, csv: None },
        levels: vec![TesrLevel {
            extension: TesrTier { txid: x.txid, signed_tx: x_signed, out_value: x.out_value, csv: Some(csv_e) },
            state: TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(csv_d) },
        }],
        m: 0,
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
    bundle.levels[last] = TesrLevel {
        extension: TesrTier { txid: x.txid, signed_tx: x_signed, out_value: x.out_value, csv: Some(csv_e_new) },
        state: TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(csv_d) },
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
    let cur_ext = bundle.current().extension.clone();
    let state_csv = bundle.current().state.csv.unwrap_or(csv_d);

    // 1. Re-sign the current level's state as a self-split paying A (so a child level can hang off it).
    let s_roll = mercurylib::tesr::build_state(&cur_ext.txid, cur_ext.out_value, &bundle.agg_address, &bundle.network, state_csv, bundle.fee_rate)?;
    let s_roll_signed = cosign_tier(cc, coin, s_roll.tx_hex.clone(), cur_ext.out_value, &bundle.network).await?;

    // 2. Fresh level off the self-split output: new extension + owner-exit state.
    let x2 = mercurylib::tesr::build_extension(&s_roll.txid, s_roll.out_value, &bundle.agg_address, &bundle.network, csv_e, bundle.fee_rate)?;
    let x2_signed = cosign_tier(cc, coin, x2.tx_hex.clone(), s_roll.out_value, &bundle.network).await?;
    let s2 = mercurylib::tesr::build_state(&x2.txid, x2.out_value, &bundle.owner_exit_address, &bundle.network, csv_d, bundle.fee_rate)?;
    let s2_signed = cosign_tier(cc, coin, s2.tx_hex.clone(), x2.out_value, &bundle.network).await?;

    let last = bundle.levels.len() - 1;
    bundle.levels[last].state = TesrTier { txid: s_roll.txid, signed_tx: s_roll_signed, out_value: s_roll.out_value, csv: Some(state_csv) };
    bundle.levels.push(TesrLevel {
        extension: TesrTier { txid: x2.txid, signed_tx: x2_signed, out_value: x2.out_value, csv: Some(csv_e) },
        state: TesrTier { txid: s2.txid, signed_tx: s2_signed, out_value: s2.out_value, csv: Some(csv_d) },
    });
    bundle.m = 0; // fresh renewal budget at the new level
    Ok(())
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

/// True iff `txid` is known to the chain backend (confirmed or in mempool).
fn tx_known(cc: &ClientConfig, txid: &str) -> bool {
    match electrum_client::bitcoin::Txid::from_str(txid) {
        Ok(t) => cc.electrum_client.transaction_get_raw(&t).is_ok(),
        Err(_) => false,
    }
}

/// True iff `txid:vout` is no longer unspent (its funding UTXO has been consumed).
fn outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> bool {
    let t = match electrum_client::bitcoin::Txid::from_str(txid) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let raw = match cc.electrum_client.transaction_get_raw(&t) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let tx: electrum_client::bitcoin::Transaction = match electrum_client::bitcoin::consensus::deserialize(&raw) {
        Ok(x) => x,
        Err(_) => return false,
    };
    let spk = &tx.output[vout as usize].script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk).unwrap_or_default();
    !listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout)
}

/// **WatchBundle v2 (keyless watchtower).** One reactive pass: if the coin's funding UTXO `F` has
/// been spent — i.e. someone broadcast the trigger — drive the OWNER's unilateral exit by
/// broadcasting each pre-signed tier in order as its relative-timelock matures. Keyless: it holds
/// only the pre-signed [`TesrBundle`] (every tier pays the owner) and NEVER co-signs, so a delegated
/// tower can defend an offline owner without any key material. Idempotent — call once per new block
/// from a tower loop; already-confirmed tiers are skipped and a not-yet-mature tier just retries next
/// pass. Returns the tier txids broadcast this pass.
pub fn watch_pass(cc: &ClientConfig, bundle: &TesrBundle) -> Vec<String> {
    // Defend only once the coin has actually been triggered on-chain — an idle un-broadcast coin
    // never ages, so there is nothing to do until F is spent.
    if !outpoint_spent(cc, &bundle.f_txid, bundle.f_vout) {
        return vec![];
    }
    let mut acted = Vec::new();
    for tier in bundle.exit_tiers() {
        if tx_known(cc, &tier.txid) {
            continue; // already on-chain / in mempool
        }
        let raw = match hex::decode(&tier.signed_tx) {
            Ok(r) => r,
            Err(_) => break,
        };
        match cc.electrum_client.transaction_broadcast_raw(&raw) {
            Ok(_) => acted.push(tier.txid.clone()),
            Err(_) => break, // CSV not met yet / parent unconfirmed — retry on the next pass
        }
    }
    acted
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

/// **Receiver R′ verification (V2-DESIGN §5.11).** Soundly verify a conveyed TES-R ladder before
/// accepting a coin: it must be a valid unilateral-exit chain over the on-chain funding UTXO `F`, and
/// the SE's PUBLIC finalized-signature count must EXACTLY account for its tiers (plus any pre-TES-R
/// V1 backups). Exact equality is the linchpin — it makes a hidden extra co-signed state (a
/// double-spend the receiver can't see) impossible, and prevents padding the ladder with junk. Checks:
///   1. the trigger spends `F` (no relative-timelock) and pays the aggregate key `A`;
///   2. every later tier spends its parent's `out[0]`, carries a BIP-68 block CSV within the coin's
///      schedule bounds, and pays `A` — except the final state, which pays the owner;
///   3. `se_num_sigs == v1_backups + <number of tiers>`.
/// This is a PURE function (no network) so it is unit-testable and reusable by the transfer receiver.
pub fn verify_bundle(bundle: &TesrBundle, se_num_sigs: u32, v1_backups: u32) -> Result<()> {
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
    if t.output.is_empty() || t.output[0].script_pubkey != agg_spk {
        return Err(anyhow::anyhow!("trigger does not pay the aggregate key A"));
    }

    // 2. Each later tier spends its parent's out[0], within schedule bounds, paying A (or owner if final).
    let p = &bundle.params;
    for i in 1..txs.len() {
        let tx = &txs[i];
        if tx.input.len() != 1
            || tx.input[0].previous_output.txid != txs[i - 1].txid()
            || tx.input[0].previous_output.vout != 0
        {
            return Err(anyhow::anyhow!("tier {i} does not spend its parent's output"));
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
        let want = if is_final { &owner_spk } else { &agg_spk };
        if tx.output.is_empty() || &tx.output[0].script_pubkey != want {
            return Err(anyhow::anyhow!("tier {i} pays the wrong output"));
        }
    }

    // 3. The linchpin: the SE's finalized-signature count must EXACTLY account for the ladder.
    let expected = v1_backups + tiers.len() as u32;
    if se_num_sigs != expected {
        return Err(anyhow::anyhow!(
            "num_sigs mismatch: SE issued {se_num_sigs}, ladder+backups account for {expected} — possible hidden state"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod verify_tests {
    use super::*;

    const AGG: &str = "bcrt1p83afnxgnczlsqvd20swjlnr3kcm7hvz9p338dgueetjz2tx6vvjs05rsfy";
    const OWNER: &str = "bcrt1qv23qwf82jw5k68juxnlxx06yz8plu0mrfrqvws";
    const F_TXID: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    // A schedule-conformant single-level bundle (unsigned tiers — verify_bundle checks structure).
    fn sample_bundle() -> TesrBundle {
        let p = mercurylib::tesr::TesrParams::regtest();
        let f_value = 100_000u64;
        let t = mercurylib::tesr::build_trigger(F_TXID, 0, f_value, AGG, "regtest", p.committed_fee_rate).unwrap();
        let x = mercurylib::tesr::build_extension(&t.txid, t.out_value, AGG, "regtest", p.ext_csv(0), p.committed_fee_rate).unwrap();
        let s = mercurylib::tesr::build_state(&x.txid, x.out_value, OWNER, "regtest", p.state_csv(0), p.committed_fee_rate).unwrap();
        TesrBundle {
            version: 1, statechain_id: "sid".into(), network: "regtest".into(),
            fee_rate: p.committed_fee_rate, agg_address: AGG.into(), owner_exit_address: OWNER.into(),
            f_txid: F_TXID.into(), f_vout: 0, f_value,
            trigger: TesrTier { txid: t.txid, signed_tx: t.tx_hex, out_value: t.out_value, csv: None },
            levels: vec![TesrLevel {
                extension: TesrTier { txid: x.txid, signed_tx: x.tx_hex, out_value: x.out_value, csv: Some(p.ext_csv(0)) },
                state: TesrTier { txid: s.txid, signed_tx: s.tx_hex, out_value: s.out_value, csv: Some(p.state_csv(0)) },
            }],
            m: 0, params: p,
        }
    }

    #[test]
    fn accepts_a_sound_ladder() {
        let b = sample_bundle();
        // trigger + extension + state = 3 tiers, 0 V1 backups.
        assert!(verify_bundle(&b, 3, 0).is_ok());
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
        b.levels[0].extension = TesrTier { txid: bogus.txid, signed_tx: bogus.tx_hex, out_value: bogus.out_value, csv: Some(p.ext_csv(0)) };
        assert!(verify_bundle(&b, 3, 0).is_err(), "extension not linked to the trigger must be rejected");
    }

    #[test]
    fn rejects_final_state_not_paying_owner() {
        let p = mercurylib::tesr::TesrParams::regtest();
        let mut b = sample_bundle();
        // Rebuild the final state paying A instead of the owner.
        let x = &b.levels[0].extension;
        let s = mercurylib::tesr::build_state(&x.txid, x.out_value, AGG, "regtest", p.state_csv(0), p.committed_fee_rate).unwrap();
        b.levels[0].state = TesrTier { txid: s.txid, signed_tx: s.tx_hex, out_value: s.out_value, csv: Some(p.state_csv(0)) };
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

    let server_partial_sig =
        sign_second(client_config, &partial.partial_signature_request_payload).await?;

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
