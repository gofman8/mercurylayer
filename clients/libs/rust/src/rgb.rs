//! RGB-over-statechain orchestration.
//!
//! Glue between the Mercury Layer client ([`mercurylib`] / this crate) and [`mercury_rgb`] (which
//! wraps rgb-lib). The asset stays bound to the statechain UTXO; every Mercury transfer / withdrawal
//! produces a *colored* backup transaction whose RGB transition re-assigns the asset to the new
//! owner's output and commits to it via an OP_RETURN opret output. The transition becomes
//! on-chain-valid only when a witness transaction is broadcast (cooperative withdrawal, or the
//! latest backup transaction on a unilateral exit).
//!
//! All Bitcoin transaction handling stays on the Mercury side (`bitcoin 0.30`); the only thing that
//! crosses into rgb-lib is strings (base64 PSBT in, base64 PSBT + base64 consignment out), which
//! keeps the two `bitcoin` crate versions from clashing.

use std::str::FromStr;

use anyhow::{anyhow, Result};
use bitcoin::hashes::{sha256, Hash as BitcoinHash, HashEngine};
use bitcoin::psbt::{Input as PsbtInput, Psbt, PsbtSighashType};
use bitcoin::{
    absolute, Address, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};
use electrum_client::ElectrumApi;
use mercurylib::tesr::{
    p2a_script, payee_address, P2A_SCRIPT_BYTES, P2A_VALUE, P2TR_OUT_VBYTES,
};
use mercurylib::transaction::{
    create_signature, get_partial_sig_request_for_colored_tx,
    get_partial_sig_request_for_colored_tx_multi, get_unsigned_backup_psbt,
    get_unsigned_combine_psbt, get_unsigned_split_psbt, new_backup_transaction,
    new_backup_transaction_multi,
};
use mercurylib::utils::get_network;
use mercurylib::wallet::Coin;
use mercury_rgb::{consignment_witness_txids, RgbWallet};
use serde::{Deserialize, Serialize};

use crate::client_config::ClientConfig;
use crate::transaction::{sign_first, sign_second};

/// Outcome of building a colored backup/withdrawal transaction.
pub struct ColoredBackupTx {
    /// Fully signed (blind MuSig2) transaction, hex-encoded. For a transfer this is kept as a
    /// backup; for a withdrawal it is broadcast.
    pub signed_tx: String,
    /// Txid of the signed transaction (the RGB witness txid the receiver accepts against).
    pub txid: String,
    /// Index of the spendable output paying the recipient/owner (the asset seal). It is the single
    /// PAYLOAD output — neither the OP_RETURN commitment nor a P2A anchor (see
    /// [`colored_payload_vouts`]): vout 1 when rgb-lib placed the OP_RETURN first (taproot
    /// recipient), or vout 0 when it appended the OP_RETURN last (non-taproot recipient). Derived
    /// and asserted, never assumed.
    pub recipient_vout: u32,
    /// RGB consignment (base64) proving the transition. Relayed in-band to the receiver.
    pub consignment: String,
    /// Seal blinding used while coloring (the receiver needs it to accept the consignment).
    pub blinding: u64,
}

/// Build, color (RGB) and blind-MuSig2-sign a backup/withdrawal transaction for an RGB-enabled coin.
///
/// The flow is identical to [`crate::transaction::new_transaction`] except that, after building the
/// unsigned transaction, it is handed to rgb-lib for coloring (which inserts the OP_RETURN opret
/// commitment and assigns `rgb_amount` of `contract_id` to the recipient output) *before* the
/// taproot key-spend sighash is computed and signed.
#[allow(clippy::too_many_arguments)]
pub async fn create_colored_backup_tx(
    client_config: &ClientConfig,
    rgb: &RgbWallet,
    coin: &mut Coin,
    contract_id: &str,
    rgb_amount: u64,
    to_address: &str,
    qt_backup_tx: u32,
    is_withdrawal: bool,
    block_height: Option<u32>,
    network: &str,
    fee_rate_sats_per_byte: f64,
    initlock: u32,
    interval: u32,
    blinding: u64,
    blinded: Option<&[(String, u64)]>,
) -> Result<ColoredBackupTx> {
    // 1. Generate and commit nonces, then fetch the server's public nonce (sign/first).
    let coin_nonce = mercurylib::transaction::create_and_commit_nonces(coin)?;
    coin.secret_nonce = Some(coin_nonce.secret_nonce.clone());
    coin.public_nonce = Some(coin_nonce.public_nonce.clone());
    coin.blinding_factor = Some(coin_nonce.blinding_factor.clone());

    let server_public_nonce = sign_first(client_config, &coin_nonce.sign_first_request_payload).await?;
    coin.server_public_nonce = Some(server_public_nonce);

    let block_height = match block_height {
        Some(block_height) => block_height,
        None => {
            let block_header = client_config.electrum_client.block_headers_subscribe_raw()?;
            block_header.height as u32
        }
    };

    // 2. Build the unsigned backup tx as a base64 PSBT (one input = statechain UTXO, one output =
    //    recipient/owner). The recipient output is at index 0 *before* coloring.
    let unsigned_psbt_b64 = get_unsigned_backup_psbt(
        coin,
        block_height,
        initlock,
        interval,
        fee_rate_sats_per_byte,
        qt_backup_tx,
        to_address.to_string(),
        network.to_string(),
        is_withdrawal,
    )?;

    // 3. Color with rgb-lib: assign `rgb_amount` to the pre-coloring output index 0. rgb-lib inserts
    //    the OP_RETURN opret commitment (at index 0, shifting the recipient to index 1) and returns
    //    the modified PSBT plus the consignment.
    let (colored_psbt_b64, consignment) = if let Some(blinded) = blinded {
        // statechain->statechain: assign to blinded seals (receiver's statechain UTXO + change to a
        // free statechain UTXO). The tx just spends the statechain UTXO to `to_address` ("to himself")
        // and commits the transition via OP_RETURN; no asset goes to a witness vout of this tx.
        rgb.color_blinded(&unsigned_psbt_b64, contract_id, blinded.to_vec(), blinding)?
    } else {
        let mut output_map = std::collections::HashMap::new();
        output_map.insert(0u32, rgb_amount);
        rgb.color(&unsigned_psbt_b64, contract_id, output_map, blinding)?
    };

    // 4. Extract the colored unsigned transaction (bitcoin 0.30) and hex-encode it.
    let colored_psbt = Psbt::from_str(&colored_psbt_b64)
        .map_err(|e| anyhow!("could not parse colored psbt: {e}"))?;
    let colored_unsigned_tx = colored_psbt.unsigned_tx.clone();
    let colored_tx_hex =
        hex::encode(bitcoin::consensus::encode::serialize(&colored_unsigned_tx));

    // The recipient/owner output is the (single) PAYLOAD output: neither the RGB `opret` commitment
    // nor a P2A anchor. rgb-lib places the OP_RETURN commitment first when a taproot output is
    // present (so the recipient is at vout 1), but appends it last for non-taproot recipients
    // (recipient stays at vout 0) - so compute it, don't assume.
    //
    // ⚠️ Hardened per `docs/utexo/CTESR-GATE.md` §4.3. The previous derivation
    // (`position(|o| !o.script_pubkey.is_op_return()).unwrap_or(0)`) was correct only *by accident*
    // for the one-payload case: it returns the wrong vout for any tx with a second non-opret output
    // (a P2A anchor, a change output, a multi-payload split), and its `unwrap_or(0)` turned "there
    // is no spendable output at all" into "vout 0" — which, on a coloured tx, is the opret. It is
    // now an explicit derivation with an asserted cardinality, so any such shape fails CLOSED.
    let payload_vouts = colored_payload_vouts(&colored_unsigned_tx);
    if payload_vouts.len() != 1 {
        return Err(anyhow!(
            "colored backup tx has {} payload outputs (expected exactly 1); \
             payload vouts {:?} of {} outputs",
            payload_vouts.len(),
            payload_vouts,
            colored_unsigned_tx.output.len()
        ));
    }
    let recipient_vout = payload_vouts[0];

    // 5. Compute the blind-MuSig2 partial-signature session over the colored transaction (which now
    //    commits to the OP_RETURN), get the server's partial signature, and aggregate.
    let partial_sig_request =
        get_partial_sig_request_for_colored_tx(coin, colored_tx_hex.clone(), network.to_string())?;

    let server_partial_sig =
        sign_second(client_config, &partial_sig_request.partial_signature_request_payload).await?;

    let signature = create_signature(
        partial_sig_request.msg,
        partial_sig_request.client_partial_sig,
        hex::encode(server_partial_sig.serialize()),
        partial_sig_request.encoded_session,
        partial_sig_request.output_pubkey,
    )?;

    // 6. Attach the signature to the colored transaction.
    let signed_tx = new_backup_transaction(colored_tx_hex, signature)?;

    let tx: bitcoin::Transaction =
        bitcoin::consensus::encode::deserialize(&hex::decode(&signed_tx)?)?;
    let txid = tx.txid().to_string();

    Ok(ColoredBackupTx {
        signed_tx,
        txid,
        recipient_vout,
        consignment,
        blinding,
    })
}

/// A split output: `(address, sat_value, rgb_amount)` — the bitcoin address that receives the
/// sub-coin, the sats it carries, and the RGB amount assigned to its witness seal.
pub type SplitOutput = (String, u64, u64);

/// Outcome of building a colored **split** transaction (off-chain RGB split, Stage 1 of
/// `docs/rgb_offchain_split_spilman.md`).
pub struct ColoredSplitTx {
    /// Fully signed (blind-MuSig2) split transaction, hex-encoded. NOT broadcast in the off-chain
    /// flow — it is the unilateral-exit escape hatch that materializes the sub-coins on-chain.
    pub signed_tx: String,
    /// Txid of the signed split transaction (the RGB witness txid the sub-coin owners validate against).
    pub txid: String,
    /// Post-coloring vout of each split output, in the same order as the `splits` argument. rgb-lib
    /// inserts the OP_RETURN opret commitment, so these are computed from the colored tx, not assumed.
    pub output_vouts: Vec<u32>,
    /// RGB consignment (base64) proving the split transition (covers every sub-coin seal).
    pub consignment: String,
    /// Seal blinding used while coloring (a sub-coin owner needs it to validate/accept).
    pub blinding: u64,
}

/// Build, color and blind-MuSig2-sign a **split** of an RGB-enabled statechain coin: spend the coin
/// into several colored **witness** outputs (the sub-coins) in one transaction, committing a single
/// RGB transition that assigns each `rgb_amount` to the corresponding output's seal.
///
/// This is the chaining primitive behind the off-chain split (pseudo-Spilman leaves): unlike
/// [`create_colored_backup_tx`] (one recipient output, replacement model) it carves N sub-coins as
/// the spend's own outputs, so a split costs **no** pre-funded statechain coins. The tx is co-signed
/// by the SE exactly like a one-output backup ([`get_partial_sig_request_for_colored_tx`] is
/// output-count agnostic) and is **not** broadcast — sub-coin owners validate it off-chain
/// ([`mercury_rgb::RgbWallet::validate_offchain`]); broadcasting it is the unilateral exit.
///
/// `splits` is the list of `(address, sat_value, rgb_amount)` outputs. The sum of `sat_value`s must
/// be less than the coin's sats (the remainder is the on-exit miner fee); the sum of `rgb_amount`s
/// must equal the coin's full RGB allocation (RGB value conservation — validated by rgb-lib).
#[allow(clippy::too_many_arguments)]
pub async fn create_colored_split_tx(
    client_config: &ClientConfig,
    rgb: &RgbWallet,
    coin: &mut Coin,
    contract_id: &str,
    splits: &[SplitOutput],
    qt_backup_tx: u32,
    is_withdrawal: bool,
    block_height: Option<u32>,
    network: &str,
    initlock: u32,
    interval: u32,
    blinding: u64,
) -> Result<ColoredSplitTx> {
    if splits.is_empty() {
        return Err(anyhow!("split must have at least one output"));
    }

    // 1. Nonce commitment + server first-round nonce (identical to create_colored_backup_tx).
    let coin_nonce = mercurylib::transaction::create_and_commit_nonces(coin)?;
    coin.secret_nonce = Some(coin_nonce.secret_nonce.clone());
    coin.public_nonce = Some(coin_nonce.public_nonce.clone());
    coin.blinding_factor = Some(coin_nonce.blinding_factor.clone());
    let server_public_nonce = sign_first(client_config, &coin_nonce.sign_first_request_payload).await?;
    coin.server_public_nonce = Some(server_public_nonce);

    let block_height = match block_height {
        Some(h) => h,
        None => client_config.electrum_client.block_headers_subscribe_raw()?.height as u32,
    };

    // 2. Build the unsigned multi-output split PSBT (one input = the statechain coin, one output per
    //    sub-coin). Pre-coloring output index i carries `splits[i]`.
    let outputs: Vec<(String, u64)> = splits.iter().map(|(a, sat, _)| (a.clone(), *sat)).collect();
    let unsigned_psbt_b64 = get_unsigned_split_psbt(
        coin,
        block_height,
        initlock,
        interval,
        qt_backup_tx,
        outputs,
        network.to_string(),
        is_withdrawal,
    )?;

    // 3. Color: assign each rgb_amount to its pre-coloring witness vout. rgb-lib inserts the single
    //    OP_RETURN opret commitment and returns one consignment covering all sub-coin seals.
    let mut output_map = std::collections::HashMap::new();
    for (i, (_, _, rgb_amount)) in splits.iter().enumerate() {
        // rgb_amount == 0 marks a sats-only output (e.g. the change of an exact-allocation token
        // transfer) - leave it uncolored rather than assigning a zero allocation.
        if *rgb_amount > 0 {
            output_map.insert(i as u32, *rgb_amount);
        }
    }
    let (colored_psbt_b64, consignment) = rgb.color(&unsigned_psbt_b64, contract_id, output_map, blinding)?;

    let colored_psbt = Psbt::from_str(&colored_psbt_b64)
        .map_err(|e| anyhow!("could not parse colored psbt: {e}"))?;
    let colored_unsigned_tx = colored_psbt.unsigned_tx.clone();
    let colored_tx_hex = hex::encode(bitcoin::consensus::encode::serialize(&colored_unsigned_tx));

    // The PAYLOAD outputs, in tx order, are the sub-coins in the same order as `splits` (rgb-lib
    // preserves output order and only inserts the OP_RETURN). Map each split -> its vout. Note this
    // lane never carries a P2A anchor today, so excluding it is a no-op here — but it is the correct
    // derivation, and the `!= splits.len()` guard below still fails CLOSED either way.
    let output_vouts: Vec<u32> = colored_payload_vouts(&colored_unsigned_tx);
    if output_vouts.len() != splits.len() {
        return Err(anyhow!(
            "colored split tx has {} spendable outputs, expected {}",
            output_vouts.len(),
            splits.len()
        ));
    }

    // 4. Blind-MuSig2 partial-sign the colored tx (output-count agnostic on the SE side) and aggregate.
    let partial_sig_request =
        get_partial_sig_request_for_colored_tx(coin, colored_tx_hex.clone(), network.to_string())?;
    let server_partial_sig =
        sign_second(client_config, &partial_sig_request.partial_signature_request_payload).await?;
    let signature = create_signature(
        partial_sig_request.msg,
        partial_sig_request.client_partial_sig,
        hex::encode(server_partial_sig.serialize()),
        partial_sig_request.encoded_session,
        partial_sig_request.output_pubkey,
    )?;

    let signed_tx = new_backup_transaction(colored_tx_hex, signature)?;
    let tx: bitcoin::Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&signed_tx)?)?;
    let txid = tx.txid().to_string();

    Ok(ColoredSplitTx { signed_tx, txid, output_vouts, consignment, blinding })
}

/// Outcome of building a colored **combine** transaction — N statechain coins spent into M outputs in
/// one un-broadcast, blind-MuSig2 co-signed transaction (the "combine + separate" half of the
/// forest/DAG model in `docs/rgb_offchain_split_spilman.md`).
pub struct ColoredCombineTx {
    /// Fully signed (blind-MuSig2 per input) combine transaction, hex-encoded. NOT broadcast off-chain.
    pub signed_tx: String,
    /// Txid of the signed combine transaction (the RGB witness txid receivers validate against).
    pub txid: String,
    /// Post-coloring vout of each output, in the same order as the `outputs` argument.
    pub output_vouts: Vec<u32>,
    /// The consumed input outpoints (`txid:vout`), in tx-input order — the receiver passes these to
    /// `validate_offchain_chain` so each un-broadcast input witness resolves from the consignment.
    pub input_outpoints: Vec<String>,
    /// RGB consignment (base64) proving the combine transition (sums all inputs across the outputs).
    pub consignment: String,
    /// Seal blinding used while coloring.
    pub blinding: u64,
}

/// Build, color and blind-MuSig2-sign a **combine** transaction: spend `coins` (N registered
/// statechain coins, each holding an allocation of `contract_id`) into the colored `outputs` (M
/// sub-coins), committing one RGB transition that conserves `sum(inputs) = sum(outputs)`. The SE
/// co-signs **each** input (Mercury's single-input rule is lifted via
/// [`get_partial_sig_request_for_colored_tx_multi`] / [`new_backup_transaction_multi`]). The tx is
/// **not** broadcast; receivers validate it off-chain ([`mercury_rgb::RgbWallet::validate_offchain_chain`]
/// passing `input_outpoints`); broadcasting it is the unilateral exit.
///
/// Each coin in `coins` must be registered (`register_statechain`) so rgb-lib knows its input
/// allocation, and must carry fresh nonce state — handled here (a `sign_first` round per input).
#[allow(clippy::too_many_arguments)]
pub async fn create_colored_combine_tx(
    client_config: &ClientConfig,
    rgb: &RgbWallet,
    coins: &mut [Coin],
    contract_id: &str,
    outputs: &[SplitOutput],
    qt_backup_tx: u32,
    is_withdrawal: bool,
    block_height: Option<u32>,
    network: &str,
    initlock: u32,
    interval: u32,
    blinding: u64,
) -> Result<ColoredCombineTx> {
    if coins.is_empty() {
        return Err(anyhow!("combine needs at least one input coin"));
    }
    if outputs.is_empty() {
        return Err(anyhow!("combine needs at least one output"));
    }

    let block_height = match block_height {
        Some(h) => h,
        None => client_config.electrum_client.block_headers_subscribe_raw()?.height as u32,
    };

    // 1. Per-input nonce commitment + server first-round nonce.
    for coin in coins.iter_mut() {
        let coin_nonce = mercurylib::transaction::create_and_commit_nonces(coin)?;
        coin.secret_nonce = Some(coin_nonce.secret_nonce.clone());
        coin.public_nonce = Some(coin_nonce.public_nonce.clone());
        coin.blinding_factor = Some(coin_nonce.blinding_factor.clone());
        let server_public_nonce = sign_first(client_config, &coin_nonce.sign_first_request_payload).await?;
        coin.server_public_nonce = Some(server_public_nonce);
    }

    // 2. Build the unsigned multi-input PSBT (N inputs = the coins, M outputs).
    let out_pairs: Vec<(String, u64)> = outputs.iter().map(|(a, sat, _)| (a.clone(), *sat)).collect();
    let unsigned_psbt_b64 = get_unsigned_combine_psbt(
        coins,
        block_height,
        initlock,
        interval,
        qt_backup_tx,
        out_pairs,
        network.to_string(),
        is_withdrawal,
    )?;

    // 3. Color: rgb-lib consumes every input allocation and assigns each rgb_amount to its output vout.
    let mut output_map = std::collections::HashMap::new();
    for (i, (_, _, rgb_amount)) in outputs.iter().enumerate() {
        output_map.insert(i as u32, *rgb_amount);
    }
    let (colored_psbt_b64, consignment) = rgb.color(&unsigned_psbt_b64, contract_id, output_map, blinding)?;

    let colored_psbt = Psbt::from_str(&colored_psbt_b64)
        .map_err(|e| anyhow!("could not parse colored psbt: {e}"))?;
    let colored_unsigned_tx = colored_psbt.unsigned_tx.clone();
    let colored_tx_hex = hex::encode(bitcoin::consensus::encode::serialize(&colored_unsigned_tx));

    let output_vouts: Vec<u32> = colored_payload_vouts(&colored_unsigned_tx);
    if output_vouts.len() != outputs.len() {
        return Err(anyhow!(
            "colored combine tx has {} spendable outputs, expected {}",
            output_vouts.len(),
            outputs.len()
        ));
    }

    let input_outpoints: Vec<String> = colored_unsigned_tx
        .input
        .iter()
        .map(|i| format!("{}:{}", i.previous_output.txid, i.previous_output.vout))
        .collect();

    // 4. Per-input blind-MuSig2: sessions (in tx-input order) -> sign_second -> aggregate. One sig per input.
    let sessions =
        get_partial_sig_request_for_colored_tx_multi(coins, colored_tx_hex.clone(), network.to_string())?;
    let mut signatures = Vec::with_capacity(sessions.len());
    for session in sessions.iter() {
        let server_partial_sig =
            sign_second(client_config, &session.partial_signature_request_payload).await?;
        let signature = create_signature(
            session.msg.clone(),
            session.client_partial_sig.clone(),
            hex::encode(server_partial_sig.serialize()),
            session.encoded_session.clone(),
            session.output_pubkey.clone(),
        )?;
        signatures.push(signature);
    }

    let signed_tx = new_backup_transaction_multi(colored_tx_hex, signatures)?;
    let tx: bitcoin::Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&signed_tx)?)?;
    let txid = tx.txid().to_string();

    Ok(ColoredCombineTx { signed_tx, txid, output_vouts, input_outpoints, consignment, blinding })
}

/// RGB-over-statechain status annotation, layered on top of the Mercury `CoinStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RgbStatechainStatus {
    /// The RGB allocation is assigned to the statechain funding outpoint X.
    RgbAssignedToStatecoin,
    /// A new RGB transition + colored backup tx were built locally.
    RgbAnchorRefreshPrepared,
    /// The SE blind-MuSig2 co-signed the new colored backup tx.
    RgbAnchorRefreshSigned,
    /// Self-transfer completed, key-share rotated, new colored backup tx is the latest state.
    RgbAnchorRefreshAccepted,
    /// The latest colored backup/exit tx was broadcast.
    ExitBroadcasted,
    /// Bitcoin confirmed the exit tx and RGB validated against the confirmed witness tx.
    OnchainConfirmed,
    /// An invariant failed.
    Rejected,
}

/// Outcome of an RGB anchor refresh performed via a statechain self-transfer.
///
/// The asset's funding outpoint X is unchanged; what changed is the *latest server-co-signed exit
/// transaction*: it now spends X with a lower `nLockTime` and commits a new RGB transition. Nothing
/// was broadcast, so this is "statechain-accepted" RGB, not "Bitcoin-confirmed" RGB.
pub struct RgbAnchorRefresh {
    /// Funding outpoint X (`txid:vout`) - identical before and after the refresh.
    pub funding_outpoint: String,
    /// `tx_n` of the previous latest backup tx.
    pub previous_tx_n: u32,
    /// `tx_n` of the new latest (colored) backup tx.
    pub new_tx_n: u32,
    /// `nLockTime` of the previous latest backup tx.
    pub previous_nlocktime: u32,
    /// `nLockTime` of the new latest backup tx (must be strictly lower).
    pub new_nlocktime: u32,
    /// Txid of the new latest colored backup/exit tx (the RGB witness the receiver accepts against).
    pub new_backup_txid: String,
    /// The new RGB commitment's consignment (base64).
    pub rgb_commitment_consignment: String,
    /// Auth pubkey of the owner before the refresh (key-share rotation: this differs from the new one).
    pub previous_owner_auth_pubkey: String,
    /// RGB-over-statechain status after the refresh.
    pub status: RgbStatechainStatus,
}

/// Refresh the RGB anchor of a statechain coin by performing a **self-transfer** (owner -> owner):
/// build a new colored backup/exit transaction that spends the same funding outpoint X, commits a
/// new RGB transition (OP_RETURN opret), is blind-MuSig2 co-signed by the SE, and rotate the
/// owner key-share - all via the standard Mercury transfer protocol, **without broadcasting**.
///
/// Flow: `new_transfer_address` (same wallet) ->
/// `transfer/sender` (x1) -> colored backup tx via [`create_colored_backup_tx`] paying the
/// self-transfer address -> `transfer/update_msg` -> [`crate::transfer_receiver::execute`]
/// (key-share update + backup-tx/consignment validation).
///
/// Preconditions: the RGB asset is assigned to X (see `register_statechain_utxo`) and `coin` is the
/// `duplicate_index == 0`, CONFIRMED/IN_TRANSFER coin for `statechain_id`.
#[allow(clippy::too_many_arguments)]
pub async fn refresh_rgb_anchor_self_transfer(
    client_config: &ClientConfig,
    rgb: &RgbWallet,
    wallet_name: &str,
    statechain_id: &str,
    contract_id: &str,
    rgb_amount: u64,
    blinding: u64,
    network: &str,
    beneficiary: Option<&str>,
) -> Result<RgbAnchorRefresh> {
    use crate::sqlite_manager::{get_backup_txs, get_wallet, update_backup_txs, update_wallet};
    use crate::utils::info_config;
    use mercurylib::wallet::{get_previous_outpoint, BackupTx, CoinStatus};

    // 1. Fresh owner/auth key for the SAME wallet (the "self" recipient). This is what makes the
    //    refresh a real statechain transfer with a rotated key-share, not just another signature.
    let self_transfer_address =
        crate::transfer_receiver::new_transfer_address(client_config, wallet_name).await?;

    // 2. Load the coin to refresh (duplicate_index 0, spendable, lowest locktime = latest state).
    let mut wallet = get_wallet(&client_config.pool, wallet_name).await?;
    let coin = wallet
        .coins
        .iter()
        .filter(|c| {
            c.statechain_id.as_deref() == Some(statechain_id)
                && (c.status == CoinStatus::CONFIRMED || c.status == CoinStatus::IN_TRANSFER)
                && c.duplicate_index == 0
        })
        .min_by_key(|c| c.locktime.unwrap_or(u32::MAX))
        .ok_or_else(|| anyhow!("no spendable coin for statechain_id {statechain_id}"))?
        .clone();
    let funding_outpoint = format!(
        "{}:{}",
        coin.utxo_txid.as_ref().ok_or_else(|| anyhow!("coin has no utxo_txid"))?,
        coin.utxo_vout.ok_or_else(|| anyhow!("coin has no utxo_vout"))?
    );
    let previous_owner_auth_pubkey = coin.auth_pubkey.clone();

    // 3. Prior backup-tx history for X (kept for audit; the new one appends with the next tx_n).
    let all_backups = get_backup_txs(&client_config.pool, wallet_name, statechain_id).await?;
    let mut coin_backups: Vec<BackupTx> = all_backups
        .into_iter()
        .filter(|b| {
            get_previous_outpoint(b)
                .map(|o| Some(o.txid) == coin.utxo_txid && Some(o.vout) == coin.utxo_vout)
                .unwrap_or(false)
        })
        .collect();
    coin_backups.sort_by(|a, b| a.tx_n.cmp(&b.tx_n));
    let previous_tx_n = coin_backups.last().map(|b| b.tx_n).unwrap_or(0);
    let previous_nlocktime = match coin_backups.last() {
        Some(b) => tx_nlocktime(&b.tx)?,
        None => return Err(anyhow!("no existing backup tx for the coin")),
    };
    let qt_backup_tx = coin_backups.len() as u32;

    // 4. /transfer/sender -> x1 (server records the new auth key for this statechain_id).
    let signed_statechain_id = coin
        .signed_statechain_id
        .as_ref()
        .ok_or_else(|| anyhow!("coin has no signed_statechain_id"))?
        .clone();
    let (_, _, recipient_auth_pubkey) =
        mercurylib::decode_transfer_address(&self_transfer_address)?;
    let x1 = crate::transfer_sender::get_new_x1(
        client_config,
        statechain_id,
        &signed_statechain_id,
        &recipient_auth_pubkey.to_string(),
        None,
    )
    .await?;

    // 5. Authorize the (self-)transfer of X.
    let transfer_signature = mercurylib::transfer::sender::create_transfer_signature(
        &self_transfer_address,
        coin.utxo_txid.as_ref().unwrap(),
        coin.utxo_vout.unwrap(),
        coin.user_privkey.as_ref(),
    )?;

    // 6. Build the NEW colored backup tx: spends X, pays the self-transfer address, OP_RETURN commits
    //    the new RGB transition, blind-MuSig2 co-signed. NOT broadcast. (is_withdrawal=false -> a
    //    transfer-style backup tx with a decremented nLockTime.)
    let si = info_config(client_config).await?;
    // The backup-tx locktime is computed from the FIRST backup tx's locktime (Tx1), decremented by
    // `interval * qt_backup_tx` (see `calculate_block_height`), exactly as the normal transfer does.
    // Passing the current chain height instead would make the new locktime far too low.
    let first_backup_locktime = mercurylib::utils::get_blockheight(&coin_backups[0])?;
    let mut coin_for_color = coin.clone();
    // The bitcoin output always pays the sender's own (self-transfer) address, so the sats stay with
    // the sender. The RGB asset assignment is set by the OP_RETURN: `beneficiary = None` assigns to
    // the self output (anchor self-refresh); `Some(recipient_id)` assigns to a *receiver's*
    // blinded seal (off-chain P2P transfer) - the asset moves, the sats do not.
    let blinded_vec: Vec<(String, u64)>;
    let blinded = match beneficiary {
        Some(rid) => {
            blinded_vec = vec![(rid.to_string(), rgb_amount)];
            Some(blinded_vec.as_slice())
        }
        None => None,
    };
    let colored = create_colored_backup_tx(
        client_config,
        rgb,
        &mut coin_for_color,
        contract_id,
        rgb_amount,
        &self_transfer_address,
        qt_backup_tx,
        false,
        Some(first_backup_locktime),
        network,
        si.fee_rate_sats_per_byte,
        si.initlock,
        si.interval,
        blinding,
        blinded,
    )
    .await?;
    let new_nlocktime = tx_nlocktime(&colored.signed_tx)?;
    let new_tx_n = previous_tx_n + 1;

    // 7. Append the colored backup tx (carrying the consignment) and post the transfer update msg.
    let new_backup = BackupTx {
        tx_n: new_tx_n,
        tx: colored.signed_tx.clone(),
        client_public_nonce: coin_for_color.public_nonce.clone().unwrap(),
        server_public_nonce: coin_for_color.server_public_nonce.clone().unwrap(),
        client_public_key: coin_for_color.user_pubkey.clone(),
        server_public_key: coin_for_color.server_pubkey.clone().unwrap(),
        blinding_factor: coin_for_color.blinding_factor.clone().unwrap(),
        rgb_consignment: Some(colored.consignment.clone()),
        rgb_blinding: Some(colored.blinding),
    };
    let mut backup_transactions = coin_backups.clone();
    backup_transactions.push(new_backup);
    backup_transactions.sort_by(|a, b| a.tx_n.cmp(&b.tx_n));

    let update_msg = mercurylib::transfer::sender::create_transfer_update_msg(
        &x1,
        &self_transfer_address,
        &coin,
        &transfer_signature,
        &backup_transactions,
    )?;
    let client = client_config.get_reqwest_client()?;
    let status = client
        .post(&format!("{}/transfer/update_msg", client_config.statechain_entity))
        .json(&update_msg)
        .send()
        .await?
        .status();
    if !status.is_success() {
        return Err(anyhow!("transfer/update_msg failed: {status}"));
    }
    update_backup_txs(&client_config.pool, wallet_name, statechain_id, &backup_transactions).await?;
    if let Some(c) = wallet.coins.iter_mut().find(|c| {
        c.statechain_id.as_deref() == Some(statechain_id) && c.duplicate_index == 0
    }) {
        c.status = CoinStatus::IN_TRANSFER;
    }
    update_wallet(&client_config.pool, &wallet).await?;

    // 8. Receive the self-transfer: rotates the key-share and validates the colored backup tx +
    //    consignment. This finalizes the new latest state. Still nothing broadcast on Bitcoin.
    crate::transfer_receiver::execute(client_config, wallet_name).await?;

    // 9. Verify invariants (the rest - key rotation, not-broadcast - are asserted by the caller/test).
    if new_nlocktime >= previous_nlocktime {
        return Ok(RgbAnchorRefresh {
            funding_outpoint,
            previous_tx_n,
            new_tx_n,
            previous_nlocktime,
            new_nlocktime,
            new_backup_txid: colored.txid,
            rgb_commitment_consignment: colored.consignment,
            previous_owner_auth_pubkey,
            status: RgbStatechainStatus::Rejected,
        });
    }

    Ok(RgbAnchorRefresh {
        funding_outpoint,
        previous_tx_n,
        new_tx_n,
        previous_nlocktime,
        new_nlocktime,
        new_backup_txid: colored.txid,
        rgb_commitment_consignment: colored.consignment,
        previous_owner_auth_pubkey,
        status: RgbStatechainStatus::RgbAnchorRefreshAccepted,
    })
}

/// Extract the `nLockTime` of a hex-encoded transaction.
fn tx_nlocktime(tx_hex: &str) -> Result<u32> {
    let tx: bitcoin::Transaction =
        bitcoin::consensus::encode::deserialize(&hex::decode(tx_hex)?)?;
    Ok(tx.lock_time.to_consensus_u32())
}

// =================================================================================================
// CTES-R — per-tier seal blinding, and the coloured tier builder
//
// Gate decision: `docs/utexo/CTESR-GATE.md` §3.1 (blinding), §3.4 (fee), §3.5 (builder), §4.3
// (hardened vout derivation). Nothing here is wired into the live TES-R ladder — this is the
// unit-level foundation the "colour a live T over a real F" commit is built on.
// =================================================================================================

/// Domain-separation tag for the CTES-R seal-blinding derivation.
///
/// If the derivation inputs ever change, bump the `/v1` suffix — never edit the bytes in place. An
/// old and a new derivation must not be able to produce the same blinding for different tiers, and
/// a version bump is what makes a sender/receiver mismatch loud (the receiver's `accept` fails)
/// instead of silent.
const SEAL_BLINDING_TAG: &[u8] = b"mercury/utexo/ctesr/seal-blinding/v1";

/// What a coloured transition IS, structurally — one byte of the seal-blinding domain.
///
/// Rival transitions over the SAME parent output are the NORMAL case in CTES-R: a renewal replaces
/// `X_m` over `T`'s payload output, a transfer replaces `S_k` over `X`'s payload output. The role is
/// the first thing that separates them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TierRole {
    /// The on-chain deposit transition that binds an allocation to a fresh statechain UTXO `F`.
    Funding,
    /// TRIGGER `T` — spends `F`, no relative timelock.
    Trigger,
    /// EXTENSION `X_m` — spends `T`'s payload output; `tier_index` is the renewal epoch `m`.
    Extension,
    /// STATE `S_k` — spends `X_m`'s payload output; `tier_index` is the state counter `k`.
    State,
    /// SPLIT STATE `SP` — a state tier with N payload outputs (the in-ladder split).
    SplitState,
    /// Cooperative DE-TRIGGER — a fresh, timelock-disabled spend of `T`'s payload output.
    Detrigger,
    /// Un-laddered colored backup / withdrawal transaction (the legacy carrier lane).
    Backup,
    /// Un-laddered colored split (the legacy token-piece lane).
    Split,
    /// Un-laddered colored combine (N carriers -> M pieces).
    Combine,
    /// CHILD EXTENSION — a split child's own extension, spending `SP.out[j]` under the CHILD's
    /// aggregate. Distinct from [`TierRole::Extension`] because a child's ladder is headless (no
    /// trigger) and lives under a different aggregate, so its seals must never collide with the
    /// parent's even when `tier_index` and `rung` coincide.
    ChildExtension,
    /// CHILD STATE — a split child's owner state, spending its child extension's payload output.
    ChildState,
}

impl TierRole {
    /// STABLE wire tag. **Never renumber.** A changed tag silently changes every blinding derived
    /// from it, which desynchronises sender and receiver on coins that are already in flight.
    pub const fn tag(self) -> u8 {
        match self {
            TierRole::Funding => 0x01,
            TierRole::Trigger => 0x02,
            TierRole::Extension => 0x03,
            TierRole::State => 0x04,
            TierRole::SplitState => 0x05,
            TierRole::Detrigger => 0x06,
            TierRole::Backup => 0x07,
            TierRole::Split => 0x08,
            TierRole::Combine => 0x09,
            TierRole::ChildExtension => 0x0A,
            TierRole::ChildState => 0x0B,
        }
    }
}

/// The identity of ONE coloured transition's seal — everything the blinding is derived from.
///
/// **Why this exists.** rgb-lib commits the seal blinding into the `OpId`
/// (`Assign::conceal -> SecretSeal -> AssignmentCommitment.seal`), and a `TransitionBundle`'s id
/// commits *only* its `input_map`. So two transitions over the same parent outpoint, with the same
/// amounts and the same blinding, collapse to ONE `OpId` and ONE `BundleId`; rgb-lib then resolves
/// that BundleId to whichever rival witness has the numerically smallest **internal** (LE) txid — an
/// arbitrary hash lottery uncorrelated with recency. The loser's consignment embeds the rival's
/// witness and **no branch the receiver can try will validate**
/// (`docs/utexo/CTESR-GATE.md` §2.2). The single global `TOKEN_BLINDING = 777` armed exactly that.
///
/// **The uniqueness obligation.** Uniqueness must hold over the whole
/// `(parent outpoint, role, index)` space: two tiers sharing a parent output must NEVER share a
/// blinding. Within one coin `(role, tier_index, rung)` separates rivals; across coins
/// `statechain_id` does.
///
/// **Receiver derivability.** The receiver needs the identical value for
/// `RgbWallet::accept(txid, vout, blinding)`, so every input must be data it already holds from the
/// transfer message: the coin's `statechain_id`, and the tier's role/index/rung, which are implied
/// by the disclosed tier the consignment is about. It must **derive**, never trust a sender-supplied
/// blinding field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierSeal {
    /// The `statechain_id` of the coin whose ladder this transition belongs to.
    ///
    /// Exception, and the only one: a [`TierRole::Funding`] transition runs *before* the coin has a
    /// statechain_id (the deposit has not confirmed yet), so it passes the unique deposit address it
    /// is funding instead. Nothing derives a funding blinding on the receiving side.
    pub statechain_id: String,
    /// What the transition structurally is.
    pub role: TierRole,
    /// The tier's counter within its role: the renewal epoch `m` for an extension, the state counter
    /// `k` for a state, the spend generation for a legacy split/backup.
    pub tier_index: u32,
    /// Free discriminator for transitions that would otherwise share
    /// `(statechain_id, role, tier_index)` — the child index `j` of an in-ladder split, the ladder
    /// generation after an on-chain re-anchor, or a transition's payload arity. `0` when there is
    /// nothing to discriminate.
    pub rung: u32,
}

impl TierSeal {
    pub fn new(statechain_id: impl Into<String>, role: TierRole, tier_index: u32, rung: u32) -> Self {
        Self { statechain_id: statechain_id.into(), role, tier_index, rung }
    }

    /// The seal blinding for this tier. Deterministic; both parties derive it independently.
    pub fn blinding(&self) -> u64 {
        tier_blinding(&self.statechain_id, self.role, self.tier_index, self.rung)
    }
}

/// `blinding = BE_u64(SHA256(tag ‖ len(id) ‖ id ‖ role ‖ tier_index ‖ rung)[..8])`.
///
/// The length prefix on `id` is load-bearing: without it `("ab", role, …)` and `("a", …)` could be
/// made to hash the same byte string, which is precisely the collision this derivation exists to
/// prevent. All integers are big-endian so the derivation is byte-for-byte reproducible by any other
/// implementation (the receiver may not be this crate).
pub fn tier_blinding(statechain_id: &str, role: TierRole, tier_index: u32, rung: u32) -> u64 {
    let mut engine = sha256::Hash::engine();
    engine.input(SEAL_BLINDING_TAG);
    engine.input(&(statechain_id.len() as u64).to_be_bytes());
    engine.input(statechain_id.as_bytes());
    engine.input(&[role.tag()]);
    engine.input(&tier_index.to_be_bytes());
    engine.input(&rung.to_be_bytes());
    let digest = sha256::Hash::from_engine(engine).to_byte_array();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(head)
}

/// True for the standard Pay-to-Anchor script `OP_1 <0x4e73>`.
fn is_p2a(script_pubkey: &ScriptBuf) -> bool {
    script_pubkey.as_bytes() == P2A_SCRIPT_BYTES
}

/// The PAYLOAD (value-carrying) output indices of a coloured transaction, in ascending vout order:
/// every output that is **neither** the RGB `opret` commitment **nor** the P2A anchor.
///
/// `docs/utexo/CTESR-GATE.md` §3.5/§4.3: the P2A anchor script is *not* an OP_RETURN
/// (`is_op_return = false`, `is_v1_p2tr = false`, `is_witness_program = true`), so the older
/// `!is_op_return` filter counts it as a payload — observed tripping `create_colored_split_tx`'s own
/// `output_vouts.len() != splits.len()` guard at both n=1 and n=3. Callers must still assert the
/// cardinality they expect; this function only classifies.
pub fn colored_payload_vouts(tx: &Transaction) -> Vec<u32> {
    tx.output
        .iter()
        .enumerate()
        .filter(|(_, o)| !o.script_pubkey.is_op_return() && !is_p2a(&o.script_pubkey))
        .map(|(vout, _)| vout as u32)
        .collect()
}

/// **The signed virtual size of a one-payload coloured tier — MEASURED, not modelled.**
///
/// Byte for byte, over the shape [`build_colored_tier`] emits and
/// [`mercurylib::transaction::new_backup_transaction`] finalises:
///
/// | part                                          | bytes |
/// |-----------------------------------------------|-------|
/// | nVersion (3) + nLockTime                      | 8     |
/// | input count + output count                    | 2     |
/// | input (32 txid + 4 vout + 1 scriptSig len + 4 nSequence) | 41 |
/// | RGB `opret` out (8 value + 1 len + 34 `6a20`‖32-byte MPC commitment) | 43 |
/// | P2TR payload out (8 + 1 + 34)                 | 43    |
/// | P2A anchor out (8 + 1 + 4 = `OP_1 <0x4e73>`)  | 13    |
///
/// base = **150 B** ⟹ weight = `4·150 + 2` (segwit marker+flag) `+ 67` (witness: 1 item-count varint
/// + 1 length varint + a **65-byte** signature) = **669 WU** ⟹ vsize = `⌈669/4⌉` = **168 vB**.
///
/// **The 65th signature byte is the whole of [D4].** A taproot key-spend signature is 64 bytes
/// *only* under `SIGHASH_DEFAULT`. TES-R does not sign that way: `mercurylib::tesr::cosign_tier_request`
/// hashes with `TapSighashType::All`, and `new_backup_transaction{,_multi}` serialise
/// `taproot::Signature { hash_ty: All }`, which appends the explicit `0x01` sighash byte. Modelling a
/// 64-byte witness therefore understates EVERY tier by exactly 1 vB (verified: at 2 sat/vB the
/// previous model committed 334 sat to a transaction that relays at 168 vB — 1.988 sat/vB, short of
/// the rate the pre-signed tier is supposed to be able to confirm at with no P2A child attached).
///
/// **Both halves are now corrected.** The uncoloured `mercurylib::tesr::TIER_VBYTES` carried the same
/// 1-vB understatement (124 on the `SIGHASH_DEFAULT` model; its true signed size is **125**) and has
/// been moved to 125, so the two lanes differ by exactly one [`P2TR_OUT_VBYTES`] — the RGB `opret`
/// output and nothing else:
///
/// ```text
/// TIER_VBYTES (125) + P2TR_OUT_VBYTES (43) == COLORED_TIER_VBYTES (168)
/// ```
///
/// That identity is what `SDK_E2E=74` asserts on a live coloured ladder
/// (`colored_committed_fee(1, r) − committed_fee(r) == 43·r`); fixing one half only is what made it
/// red. It is pinned here by [`ctesr_tests::the_coloured_surcharge_is_exactly_one_opret_output`] —
/// MEASURED on both shapes through the production finaliser, not restated.
pub const COLORED_TIER_VBYTES: u64 = 168;

/// Signed vsize of a coloured tier carrying `n_payload` value outputs (plus the `opret` and the P2A
/// anchor). Each payload past the first adds exactly one P2TR output, i.e. `P2TR_OUT_VBYTES`.
///
/// Measured: 168 / 211 / 254 vB at n = 1 / 2 / 3. Pinned against real, production-finalised
/// transactions by [`ctesr_tests::the_coloured_fee_matches_a_measured_signed_tier`], and re-checked
/// against the *actual* rgb-lib output of every single build by [`build_colored_tier`] step 6b.
pub fn colored_tier_vbytes(n_payload: usize) -> u64 {
    COLORED_TIER_VBYTES + (n_payload.saturating_sub(1)) as u64 * P2TR_OUT_VBYTES
}

/// The committed fee a **coloured** tier must bake in: `⌈colored_tier_vbytes(n_payload) · rate⌉`.
///
/// `docs/utexo/CTESR-GATE.md` §3.4, corrected by [D4]. Two separate costs over the uncoloured tier:
///
/// 1. the RGB `opret` output serialises to exactly 43 bytes, which is exactly `P2TR_OUT_VBYTES`, so
///    the coloured tier is one whole extra P2TR output wide — that part was already right;
/// 2. the explicit `SIGHASH_ALL` byte on the taproot signature, worth 1 more vB — that part was
///    **not**, and is what this function now includes. See [`COLORED_TIER_VBYTES`].
///
/// The fee exists so a pre-signed tier **relays and confirms standalone, without its P2A anchor**.
/// A fee sized to 167 vB on a 168-vB transaction is not that guarantee, so the model moved to the
/// transaction rather than the transaction to the model: at 2 sat/vB `n=1` is now 336 sat (was 334)
/// and `n=3` is 508 (was 506).
///
/// Cost (2) is not a coloured-lane cost at all — it applies to every TES-R tier — so with
/// `mercurylib::tesr::TIER_VBYTES` corrected to 125 the relation to the uncoloured fee is now EXACT
/// and carries no residue: `colored_committed_fee(n, r) == committed_fee_for_outputs(n + 1, r)`.
pub fn colored_committed_fee(n_payload: usize, fee_rate_sats_per_vb: f64) -> u64 {
    (colored_tier_vbytes(n_payload) as f64 * fee_rate_sats_per_vb).ceil() as u64
}

/// Total value available to the payload outputs of a coloured tier = parent value − the coloured
/// committed fee − the P2A anchor. `None` when the parent cannot carry another coloured tier.
pub fn colored_tier_out_total(
    prev_value: u64,
    n_payload: usize,
    fee_rate_sats_per_vb: f64,
) -> Option<u64> {
    prev_value.checked_sub(colored_committed_fee(n_payload, fee_rate_sats_per_vb) + P2A_VALUE)
}

/// [`colored_tier_out_total`] for the ordinary one-payload tier (`T` / `X_m` / `S_k`).
pub fn colored_tier_out_value(prev_value: u64, fee_rate_sats_per_vb: f64) -> Option<u64> {
    colored_tier_out_total(prev_value, 1, fee_rate_sats_per_vb)
}

/// One payload output of a coloured tier, as built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColoredPayloadOut {
    /// **Post-colouring** output index. Derived, never assumed — see [`colored_payload_vouts`].
    pub vout: u32,
    /// Sats on the output.
    pub value: u64,
    /// Its scriptPubKey, hex. A child tier spends `(txid, vout)` and must see this script.
    pub script_pubkey_hex: String,
}

/// What to colour. One input (the parent tier's payload output), N payload outputs, one P2A anchor.
pub struct ColoredTierSpec<'a> {
    /// RGB contract whose allocation rides this ladder.
    pub contract_id: &'a str,
    /// Parent outpoint: the payload output of the tier above (or the funding UTXO `F` for `T`).
    pub prev_txid: &'a str,
    pub prev_vout: u32,
    /// Value of the parent output. Load-bearing twice: the fee arithmetic, and the taproot sighash
    /// that will later co-sign this tier.
    pub prev_value: u64,
    /// scriptPubKey of the parent output, hex.
    pub prev_spk_hex: &'a str,
    /// Raw nSequence: `TRIGGER_SEQUENCE` for `T`/de-trigger, `csv_blocks(csv)` for `X_m`/`S_k`/`SP`.
    pub sequence: u32,
    /// Payload outputs, in order: `(address, sats, rgb_amount)`. `rgb_amount == 0` leaves an output
    /// uncoloured (a sats-only child). `Σ sats` MUST equal
    /// [`colored_tier_out_total`]`(prev_value, payloads.len(), fee_rate)`.
    pub payloads: &'a [(String, u64, u64)],
    pub network: &'a str,
    pub fee_rate: f64,
    /// Optional second separator for rival transitions (`ColoringInfo.nonce`). Belt-and-braces only;
    /// the [`TierSeal`] is what the receiver re-derives.
    pub nonce: Option<u64>,
}

/// A coloured, **unsigned**, un-broadcast tier.
pub struct ColoredTier {
    /// The coloured unsigned transaction, hex. Feed this to `mercurylib::tesr::cosign_tier_request`.
    pub tx_hex: String,
    /// The coloured PSBT, base64 (what rgb-lib returned).
    pub psbt_base64: String,
    /// Txid — stable across signing (a key-spend adds only witness data), so a child tier can be
    /// built on it before this one is co-signed.
    pub txid: String,
    /// Payload outputs with their EXPLICIT post-colouring vouts, in the order given in the spec.
    pub payloads: Vec<ColoredPayloadOut>,
    /// Index of the RGB `opret` commitment output.
    pub opret_vout: u32,
    /// Index of the P2A anchor output.
    pub p2a_vout: u32,
    /// The RGB consignment (base64) proving this transition.
    pub consignment: String,
    /// The seal blinding actually used — derived from the [`TierSeal`], never supplied by a caller.
    pub blinding: u64,
    /// Fee baked into the transaction (`prev_value − Σ outputs`).
    pub committed_fee: u64,
    /// vsize of this transaction once its taproot key-spend witness is attached — the size it will
    /// actually relay at. The witness is one [`TAPROOT_SIGHASH_ALL_WITNESS_BYTES`]-byte signature,
    /// which is what the production finaliser serialises; modelling the 64-byte `SIGHASH_DEFAULT`
    /// form here is what made this field read 1 vB low ([D4]). `committed_fee / signed_vsize` is the
    /// effective rate, and the two together are what §3.4 pins to exactly 2.000 sat/vB.
    ///
    /// Always equals [`colored_tier_vbytes`]`(payloads.len())` — enforced at build time (step 6b).
    pub signed_vsize: u64,
}

impl ColoredTier {
    /// Post-colouring vouts of the payload outputs, in spec order.
    pub fn payload_vouts(&self) -> Vec<u32> {
        self.payloads.iter().map(|p| p.vout).collect()
    }
}

/// Number of witness bytes one TES-R taproot key-spend signature occupies: a 64-byte Schnorr
/// signature **plus the explicit `SIGHASH_ALL` byte**.
///
/// Not 64. `mercurylib::tesr::cosign_tier_request` hashes under `TapSighashType::All`, and
/// `mercurylib::transaction::new_backup_transaction{,_multi}` push
/// `taproot::Signature { hash_ty: TapSighashType::All }.to_vec()`, which is 65 bytes — the 64-byte
/// form is `SIGHASH_DEFAULT` only. Modelling 64 here is what made the coloured committed fee 1 vB
/// short of the transaction it funds ([D4]).
const TAPROOT_SIGHASH_ALL_WITNESS_BYTES: usize = 65;

/// vsize of a tier tx once its taproot key-spend witness (one signature per input) is on it.
///
/// The signature width is [`TAPROOT_SIGHASH_ALL_WITNESS_BYTES`], i.e. what the production finaliser
/// actually serialises — this function must never be cheaper than reality, because the committed fee
/// is derived from it.
fn signed_vsize(tx: &Transaction) -> u64 {
    let mut probe = tx.clone();
    for input in probe.input.iter_mut() {
        let mut witness = Witness::new();
        witness.push(vec![0u8; TAPROOT_SIGHASH_ALL_WITNESS_BYTES]);
        input.witness = witness;
    }
    probe.vsize() as u64
}

/// The scriptPubKey a tier output pays for `address` — mirrors `mercurylib::tesr`'s own
/// (private) `spk_from_address` via the public [`payee_address`], so a Mercury transfer address
/// resolves to the recipient's derived `P2TR(recipient_user_pubkey)` exactly as an uncoloured tier
/// would pay it.
fn tier_out_spk(address: &str, network: &str) -> Result<ScriptBuf> {
    let plain = payee_address(address, network).map_err(|e| anyhow!("bad tier payee: {e:?}"))?;
    let net = get_network(network).map_err(|e| anyhow!("bad network: {e:?}"))?;
    Ok(Address::from_str(&plain)
        .map_err(|e| anyhow!("tier payee {plain} is not an address: {e}"))?
        .require_network(net)
        .map_err(|e| anyhow!("tier payee {plain} is on the wrong network: {e}"))?
        .script_pubkey())
}

/// **[CTES-R] The §3.3 stock probe: is `amount` of `contract_id` still spendable out of
/// `(txid, vout)`?**
///
/// The health check `docs/utexo/CTESR-GATE.md` §3.3 mandates and the only one that works. It builds
/// a throwaway one-input, one-output spend of the outpoint and hands it to
/// [`mercury_rgb::RgbWallet::probe_spendable`], which runs rgb-lib's **read-only** `color_psbt`:
/// nothing is consumed, no transfer row is written, no witness is resolved, and the coin is
/// untouched — safe against a live coin, including one mid-exit.
///
/// `Ok(())` means the stash still binds the allocation to that outpoint. `Err` is the answer that
/// matters: a stash whose ladder has been invalidated answers
/// `InvalidColoringInfo { "total amount in output_map (N) greater than available (0)" }` while
/// `get_asset_balance` and `list_unspents` both keep reporting a full, settled, spendable balance
/// (measured — E7). Never assert carrier liveness on the balance.
///
/// `value`/`spk_hex` describe the outpoint being spent (the witness utxo rgb-lib needs to place the
/// opret); `payee` is any valid address on `network` and is pure scaffolding — the probe transaction
/// is never signed, never broadcast and never leaves this function.
pub fn probe_allocation(
    rgb: &RgbWallet,
    contract_id: &str,
    txid: &str,
    vout: u32,
    value: u64,
    spk_hex: &str,
    payee: &str,
    network: &str,
    amount: u64,
) -> Result<()> {
    let prev_txid =
        Txid::from_str(txid).map_err(|e| anyhow!("bad probe outpoint txid {txid}: {e}"))?;
    let probe = Transaction {
        version: 2,
        lock_time: absolute::LockTime::from_consensus(0),
        input: vec![TxIn {
            previous_output: OutPoint { txid: prev_txid, vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence(0xFFFF_FFFD),
            witness: Witness::default(),
        }],
        // One P2TR output: enough for rgb-lib to have somewhere to assign, and P2TR is what makes
        // the fork place the opret FIRST — i.e. the probe has the same shape as a real tier.
        output: vec![TxOut { value, script_pubkey: tier_out_spk(payee, network)? }],
    };
    let mut psbt = Psbt::from_unsigned_tx(probe)
        .map_err(|e| anyhow!("could not build the probe psbt: {e}"))?;
    psbt.inputs = vec![PsbtInput {
        witness_utxo: Some(TxOut {
            value,
            script_pubkey: ScriptBuf::from(
                hex::decode(spk_hex).map_err(|e| anyhow!("bad probe scriptPubKey hex: {e}"))?,
            ),
        }),
        ..Default::default()
    }];
    let mut output_map = std::collections::HashMap::new();
    output_map.insert(0u32, amount);
    // The blinding is irrelevant to the probe (nothing is published), but it must be SOME value, and
    // a per-probe one keeps this call from ever looking like a real transition to a future reader.
    rgb.probe_spendable(&psbt.to_string(), contract_id, output_map, PROBE_BLINDING)
}

/// Seal blinding used by [`probe_allocation`]. Deliberately not a [`TierSeal`]-derived value: the
/// probe's transition is built in memory and discarded, so it must be impossible to mistake for a
/// real tier's seal.
const PROBE_BLINDING: u64 = 0;

/// **Build and colour ONE TES-R tier**, returning EXPLICIT payload vouts.
///
/// This is the CTES-R tier builder mandated by `docs/utexo/CTESR-GATE.md` §3.5. It deliberately does
/// **not** reuse [`create_colored_split_tx`]: that function's `!is_op_return` vout filter counts the
/// P2A anchor as a payload and trips its own length guard (observed at n=1 and n=3).
///
/// It handles both tier shapes — a one-payload `T`/`X_m`/`S_k` and an N-payload split state `SP` —
/// because they differ only in `payloads.len()`.
///
/// Four things it enforces, all of them fail-closed:
///
/// 1. **Value conservation.** `Σ payload sats` must equal
///    [`colored_tier_out_total`], i.e. the fee is exactly [`colored_committed_fee`]`(n, rate)` =
///    `committed_fee_for_outputs(n + 1, rate)`. A caller cannot silently mint, burn or underpay.
/// 2. **Explicit payload vouts.** Derived from the coloured transaction by excluding BOTH the opret
///    and the P2A anchor, then cross-checked output-by-output against what was asked for (script and
///    value, in order). Nothing positional is assumed — "the opret is index 0" is a consequence of
///    the fork's `opreturn_first`, not a rule, and the P2A anchor is not what triggers it.
/// 3. **Per-tier seal blinding.** The blinding comes from the [`TierSeal`]; there is no way to pass
///    a constant.
/// 4. **The build-time collision assert.** The returned consignment MUST carry a bundle whose
///    `pub_witness` is this tier's own txid. If it does not, the blinding collided with a rival
///    transition over the same parent output and rgb-lib kept the rival with the smaller internal
///    txid — so this consignment is unvalidatable by anyone. Fail here, loudly, rather than handing
///    it to a receiver who can only report "not known to the resolver".
///
/// The tier comes back **unsigned and un-broadcast**; co-signing is the caller's next step.
pub fn build_colored_tier(
    rgb: &RgbWallet,
    spec: &ColoredTierSpec<'_>,
    seal: &TierSeal,
) -> Result<ColoredTier> {
    if spec.payloads.is_empty() {
        return Err(anyhow!("a coloured tier needs at least one payload output"));
    }
    let n_payload = spec.payloads.len();

    // 1. Value conservation, against the COLOURED fee (§3.4).
    let available = colored_tier_out_total(spec.prev_value, n_payload, spec.fee_rate).ok_or_else(
        || {
            anyhow!(
                "parent value {} cannot carry a coloured {n_payload}-payload tier at {} sat/vB \
                 (needs {} fee + {} anchor)",
                spec.prev_value,
                spec.fee_rate,
                colored_committed_fee(n_payload, spec.fee_rate),
                P2A_VALUE
            )
        },
    )?;
    let requested: u64 = spec.payloads.iter().map(|(_, sats, _)| *sats).sum();
    if requested != available {
        return Err(anyhow!(
            "coloured tier value conservation: payloads sum to {requested} sat but the parent \
             affords exactly {available} sat (fee {} + anchor {})",
            colored_committed_fee(n_payload, spec.fee_rate),
            P2A_VALUE
        ));
    }

    // 2. Build the uncoloured tier: [payload…, P2A]. nVersion 3 (TRUC), no absolute locktime —
    //    the same shape `mercurylib::tesr::build_tier_tx` / `build_split_state` produce.
    let prev_txid = Txid::from_str(spec.prev_txid)
        .map_err(|e| anyhow!("bad parent txid {}: {e}", spec.prev_txid))?;
    let mut output = Vec::with_capacity(n_payload + 1);
    for (address, sats, _) in spec.payloads {
        output.push(TxOut { value: *sats, script_pubkey: tier_out_spk(address, spec.network)? });
    }
    output.push(TxOut { value: P2A_VALUE, script_pubkey: p2a_script() });
    let tier = Transaction {
        version: 3,
        lock_time: absolute::LockTime::from_consensus(0),
        input: vec![TxIn {
            previous_output: OutPoint { txid: prev_txid, vout: spec.prev_vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence(spec.sequence),
            witness: Witness::default(),
        }],
        output,
    };

    // 3. PSBT with the parent's witness_utxo (rgb-lib needs the prevout to place the opret and to
    //    know which allocation is being spent).
    let mut psbt = Psbt::from_unsigned_tx(tier.clone())
        .map_err(|e| anyhow!("could not build the tier psbt: {e}"))?;
    let mut input = PsbtInput {
        witness_utxo: Some(TxOut {
            value: spec.prev_value,
            script_pubkey: ScriptBuf::from(
                hex::decode(spec.prev_spk_hex)
                    .map_err(|e| anyhow!("bad parent scriptPubKey hex: {e}"))?,
            ),
        }),
        ..Default::default()
    };
    input.sighash_type = Some(
        PsbtSighashType::from_str("SIGHASH_ALL").map_err(|e| anyhow!("sighash type: {e}"))?,
    );
    psbt.inputs = vec![input];

    // 4. Colour. The blinding is DERIVED — a caller cannot pass a constant.
    let blinding = seal.blinding();
    let mut output_map = std::collections::HashMap::new();
    for (i, (_, _, rgb_amount)) in spec.payloads.iter().enumerate() {
        if *rgb_amount > 0 {
            output_map.insert(i as u32, *rgb_amount);
        }
    }
    if output_map.is_empty() {
        return Err(anyhow!("a coloured tier must assign a non-zero amount to some payload output"));
    }
    let (colored_psbt_b64, consignment) = rgb.color_with_nonce(
        &psbt.to_string(),
        spec.contract_id,
        output_map,
        blinding,
        spec.nonce,
    )?;

    let colored_psbt = Psbt::from_str(&colored_psbt_b64)
        .map_err(|e| anyhow!("could not parse the coloured tier psbt: {e}"))?;
    let colored = colored_psbt.unsigned_tx.clone();
    let txid = colored.txid().to_string();

    // 5. EXPLICIT payload vouts — excluding both the opret and the P2A anchor — then cross-checked
    //    against what was asked for, in order. Nothing positional is assumed.
    let opret_vouts: Vec<u32> = colored
        .output
        .iter()
        .enumerate()
        .filter(|(_, o)| o.script_pubkey.is_op_return())
        .map(|(v, _)| v as u32)
        .collect();
    if opret_vouts.len() != 1 {
        return Err(anyhow!(
            "coloured tier {txid} carries {} OP_RETURN outputs, expected exactly 1",
            opret_vouts.len()
        ));
    }
    let p2a_vouts: Vec<u32> = colored
        .output
        .iter()
        .enumerate()
        .filter(|(_, o)| is_p2a(&o.script_pubkey))
        .map(|(v, _)| v as u32)
        .collect();
    if p2a_vouts.len() != 1 {
        return Err(anyhow!(
            "coloured tier {txid} carries {} P2A anchor outputs, expected exactly 1",
            p2a_vouts.len()
        ));
    }
    let payload_vouts = colored_payload_vouts(&colored);
    if payload_vouts.len() != n_payload {
        return Err(anyhow!(
            "coloured tier {txid} has {} payload outputs, expected {n_payload} (opret at {:?}, \
             P2A at {:?}, {} outputs total)",
            payload_vouts.len(),
            opret_vouts,
            p2a_vouts,
            colored.output.len()
        ));
    }
    let mut payloads = Vec::with_capacity(n_payload);
    for (i, vout) in payload_vouts.iter().enumerate() {
        let got = &colored.output[*vout as usize];
        let want = &tier.output[i];
        if got.script_pubkey != want.script_pubkey || got.value != want.value {
            return Err(anyhow!(
                "coloured tier {txid} reordered or rewrote payload {i}: expected {} sat to {}, \
                 found {} sat to {} at vout {vout}",
                want.value,
                hex::encode(want.script_pubkey.as_bytes()),
                got.value,
                hex::encode(got.script_pubkey.as_bytes())
            ));
        }
        payloads.push(ColoredPayloadOut {
            vout: *vout,
            value: got.value,
            script_pubkey_hex: hex::encode(got.script_pubkey.as_bytes()),
        });
    }

    // 6. Fee, re-derived from the coloured transaction rather than trusted.
    let out_total: u64 = colored.output.iter().map(|o| o.value).sum();
    let committed_fee = spec
        .prev_value
        .checked_sub(out_total)
        .ok_or_else(|| anyhow!("coloured tier {txid} spends more than its parent holds"))?;
    let expected_fee = colored_committed_fee(n_payload, spec.fee_rate);
    if committed_fee != expected_fee {
        return Err(anyhow!(
            "coloured tier {txid} committed {committed_fee} sat of fee, expected {expected_fee} \
             (= ceil(colored_tier_vbytes({n_payload}) {} vB * {} sat/vB))",
            colored_tier_vbytes(n_payload),
            spec.fee_rate
        ));
    }

    // 6b. [D4] THE SELF-FUNDING ASSERT — measured on THIS transaction, not on the formula.
    //
    // The committed fee exists for exactly one property: a pre-signed tier must relay and confirm
    // STANDALONE, with no P2A child attached. That property is a statement about the fee rate over
    // the vsize the tier will actually have on the wire — so it is checked against the vsize rgb-lib
    // just produced, with the witness the production finaliser will actually put on it
    // (TAPROOT_SIGHASH_ALL_WITNESS_BYTES), and not against the arithmetic that produced the fee.
    //
    // This runs on EVERY coloured tier the product ever builds, which is the point: a future change
    // to the tier shape (an extra output, a different sighash, a second input) cannot silently
    // desynchronise the model from the transaction the way the missing SIGHASH_ALL byte did.
    let measured_vsize = signed_vsize(&colored);
    let modelled_vsize = colored_tier_vbytes(n_payload);
    if measured_vsize != modelled_vsize {
        return Err(anyhow!(
            "coloured tier {txid} measures {measured_vsize} vB signed but colored_tier_vbytes\
             ({n_payload}) models {modelled_vsize} vB — the committed fee {committed_fee} sat is \
             sized to the model, so the tier would relay at {:.3} sat/vB instead of {} and could \
             stall without its P2A anchor. Fix COLORED_TIER_VBYTES, not this check.",
            committed_fee as f64 / measured_vsize as f64,
            spec.fee_rate
        ));
    }
    let min_fee = (measured_vsize as f64 * spec.fee_rate).ceil() as u64;
    if committed_fee < min_fee {
        return Err(anyhow!(
            "coloured tier {txid} committed {committed_fee} sat over a measured {measured_vsize} vB \
             = {:.3} sat/vB, short of the {} sat/vB it must relay standalone at (needs {min_fee})",
            committed_fee as f64 / measured_vsize as f64,
            spec.fee_rate
        ));
    }

    // 7. THE BUILD-TIME COLLISION ASSERT (§3.1). The consignment must carry this tier's own witness.
    let witnesses = consignment_witness_txids(&consignment)?;
    if !witnesses.iter().any(|w| w == &txid) {
        return Err(anyhow!(
            "coloured tier {txid}: its OWN witness is absent from the consignment it just produced \
             (bundled witnesses: {witnesses:?}). The seal blinding {blinding} derived from \
             {seal:?} collided with a rival transition over the same parent output {}:{} — rgb-lib \
             merged them into one BundleId and kept the rival with the smaller internal txid. This \
             consignment is unvalidatable by ANY receiver; fix the TierSeal derivation.",
            spec.prev_txid,
            spec.prev_vout
        ));
    }

    Ok(ColoredTier {
        tx_hex: hex::encode(bitcoin::consensus::encode::serialize(&colored)),
        psbt_base64: colored_psbt_b64,
        txid,
        payloads,
        opret_vout: opret_vouts[0],
        p2a_vout: p2a_vouts[0],
        consignment,
        blinding,
        committed_fee,
        signed_vsize: measured_vsize,
    })
}

#[cfg(test)]
mod ctesr_tests {
    use super::*;
    // Only the fee-parity assertion needs this; importing it at module scope made every non-test
    // build warn.
    use mercurylib::tesr::committed_fee_for_outputs;
    use std::collections::HashSet;

    // Every role, and the list must STAY complete: it drives the wire-tag disjointness and the
    // per-role blinding-uniqueness assertions below, so a role missing here is a role whose seals
    // were never proved distinct from anyone else's. `ChildExtension`/`ChildState` arrived with the
    // coloured in-ladder split and were absent from this list, which is exactly the shape of hole
    // that ships a seal collision (`docs/utexo/CTESR-GATE.md` §2.2).
    const ROLES: [TierRole; 11] = [
        TierRole::Funding,
        TierRole::Trigger,
        TierRole::Extension,
        TierRole::State,
        TierRole::SplitState,
        TierRole::Detrigger,
        TierRole::Backup,
        TierRole::Split,
        TierRole::Combine,
        TierRole::ChildExtension,
        TierRole::ChildState,
    ];

    #[test]
    fn the_blinding_is_deterministic() {
        let a = tier_blinding("sid-1", TierRole::Extension, 3, 0);
        let b = tier_blinding("sid-1", TierRole::Extension, 3, 0);
        assert_eq!(a, b, "both parties must derive the identical blinding");
        assert_eq!(TierSeal::new("sid-1", TierRole::Extension, 3, 0).blinding(), a);
    }

    #[test]
    fn role_tags_are_distinct() {
        let tags: HashSet<u8> = ROLES.iter().map(|r| r.tag()).collect();
        assert_eq!(tags.len(), ROLES.len(), "two roles share a wire tag");
    }

    /// The whole `(role, tier_index, rung)` space for ONE coin must be collision-free — that is the
    /// space rival tiers over the same parent output live in.
    #[test]
    fn every_tier_of_one_coin_gets_its_own_blinding() {
        let mut seen: HashSet<u64> = HashSet::new();
        let mut n = 0usize;
        for role in ROLES {
            for index in 0..64u32 {
                for rung in 0..8u32 {
                    assert!(
                        seen.insert(tier_blinding("statechain-id-under-test", role, index, rung)),
                        "blinding collision at {role:?} index={index} rung={rung}"
                    );
                    n += 1;
                }
            }
        }
        assert_eq!(seen.len(), n);
    }

    #[test]
    fn different_coins_never_share_a_blinding() {
        let mut seen: HashSet<u64> = HashSet::new();
        for i in 0..512u32 {
            let sid = format!("sid-{i}");
            assert!(
                seen.insert(tier_blinding(&sid, TierRole::State, 0, 0)),
                "two coins share the blinding of their first state tier"
            );
        }
    }

    /// The length prefix is what stops `id ‖ role` from being reachable two ways.
    #[test]
    fn the_id_is_length_prefixed() {
        assert_ne!(
            tier_blinding("ab", TierRole::Extension, 0, 0),
            tier_blinding("a", TierRole::Extension, 0, 0)
        );
        // 'b' == 0x62; no role tag is 0x62, but pin the general property anyway.
        assert_ne!(
            tier_blinding("ab", TierRole::Trigger, 0, 0),
            tier_blinding("a", TierRole::Trigger, 0, 0)
        );
    }

    /// A coloured tier of the exact shape [`build_colored_tier`] emits: nVersion 3, no locktime, one
    /// P2TR key-spend input, the RGB `opret` FIRST (the fork's `opreturn_first`, triggered by the
    /// P2TR payloads), `n` P2TR payload outputs, the 240-sat P2A anchor last.
    fn tier_shape(n_payload: usize) -> Transaction {
        let payload_spk =
            ScriptBuf::from(hex::decode(format!("5120{}", "22".repeat(32))).unwrap());
        // `6a20` ‖ 32 bytes — exactly what rgb-psbt-utils' `ScriptBuf::new_op_return(commitment)`
        // produces for a 32-byte MPC commitment.
        let opret_spk = ScriptBuf::from(hex::decode(format!("6a20{}", "11".repeat(32))).unwrap());
        let mut output = vec![TxOut { value: 0, script_pubkey: opret_spk }];
        for _ in 0..n_payload {
            output.push(TxOut { value: 10_000, script_pubkey: payload_spk.clone() });
        }
        output.push(TxOut { value: P2A_VALUE, script_pubkey: p2a_script() });
        Transaction {
            version: 3,
            lock_time: absolute::LockTime::from_consensus(0),
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_str(&"11".repeat(32)).unwrap(),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence(0xFFFF_FFFD),
                witness: Witness::default(),
            }],
            output,
        }
    }

    /// The UNCOLOURED counterpart of [`tier_shape`], byte-identical to what
    /// `mercurylib::tesr::build_tier_tx` / `build_split_state` emit: no `opret`, `n` P2TR payload
    /// outputs, the 240-sat P2A anchor last. The two shapes differ by exactly one output, which is
    /// the whole claim the surcharge identity makes.
    fn plain_tier_shape(n_payload: usize) -> Transaction {
        let mut tx = tier_shape(n_payload);
        assert!(tx.output[0].script_pubkey.is_op_return(), "tier_shape puts the opret first");
        tx.output.remove(0);
        if n_payload == 1 {
            // Not a derivative: the base case must be byte-identical to what the REAL uncoloured
            // builder emits, or "strip the opret" would be a claim about this helper rather than
            // about the protocol.
            let real = mercurylib::tesr::build_tier_tx(
                Txid::from_str(&"11".repeat(32)).unwrap(),
                0,
                Sequence(0xFFFF_FFFD),
                ScriptBuf::from(hex::decode(format!("5120{}", "22".repeat(32))).unwrap()),
                10_000,
            );
            assert_eq!(tx, real, "the plain shape must be exactly mercurylib's build_tier_tx output");
        }
        tx
    }

    /// vsize of `tx` after the **production** finaliser has put a real TES-R witness on it.
    ///
    /// This deliberately routes through `mercurylib::transaction::new_backup_transaction_multi` —
    /// the same function that finalises every co-signed tier — rather than constructing a witness
    /// here. That is what makes this a measurement instead of a restatement: if the sighash type or
    /// the witness layout ever changes, this number moves on its own.
    fn finalised_vsize(tx: &Transaction) -> u64 {
        let unsigned_hex = hex::encode(bitcoin::consensus::encode::serialize(tx));
        // Schnorr signatures are fixed-width, so any 64 bytes parse; only the SERIALISED width
        // matters to vsize, and that is decided by the finaliser's `hash_ty`, not by these bytes.
        let signed_hex = mercurylib::transaction::new_backup_transaction_multi(
            unsigned_hex,
            vec!["01".repeat(64)],
        )
        .expect("the production finaliser must accept a tier-shaped transaction");
        let signed: Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(signed_hex).unwrap()).unwrap();
        assert_eq!(
            signed.input[0].witness.iter().next().unwrap().len(),
            TAPROOT_SIGHASH_ALL_WITNESS_BYTES,
            "TES-R signs SIGHASH_ALL, so the witness item is 64 sig bytes + 1 sighash byte"
        );
        signed.vsize() as u64
    }

    /// **[D4] The model and the transaction must agree — proved by MEASURING, at every arity.**
    ///
    /// The committed fee exists so a pre-signed tier relays and confirms standalone, WITHOUT its P2A
    /// anchor. That property is `committed_fee / real_vsize >= rate`, so it is only worth anything if
    /// `real_vsize` is the size the transaction actually has once signed. This test therefore builds
    /// the tier shape, hands it to the production finaliser, measures `Transaction::vsize()`, and
    /// requires the model to match it exactly.
    ///
    /// Counterfactual: restore the pre-[D4] model (`COLORED_TIER_VBYTES = 167`, or a 64-byte witness
    /// in `signed_vsize`) and this fails at n=1 with 334 != 336 — the previous constants sized a
    /// 167-vB transaction that measures 168 vB and would have relayed at 1.988 sat/vB.
    #[test]
    fn the_coloured_fee_matches_a_measured_signed_tier() {
        let rate = 2.0;
        for n_payload in 1..=4usize {
            let tx = tier_shape(n_payload);
            let measured = finalised_vsize(&tx);

            assert_eq!(
                colored_tier_vbytes(n_payload),
                measured,
                "n={n_payload}: the vsize model must equal the finalised transaction"
            );
            // `signed_vsize` is what `build_colored_tier` records on every real tier; it must be the
            // same measurement, not a second opinion.
            assert_eq!(
                signed_vsize(&tx),
                measured,
                "n={n_payload}: signed_vsize must model the production witness exactly"
            );
            // The property the fee is FOR: standalone relay at the target rate, no anchor.
            let fee = colored_committed_fee(n_payload, rate);
            assert!(
                fee >= (measured as f64 * rate).ceil() as u64,
                "n={n_payload}: {fee} sat over {measured} vB = {:.3} sat/vB < {rate}",
                fee as f64 / measured as f64
            );
            // At an integral rate the fee is exact — no silent over-payment either.
            assert_eq!(fee, measured * 2, "n={n_payload}: a coloured tier must pay EXACTLY {rate}");
        }
    }

    /// §3.4, as corrected by [D4] **on both halves**: a coloured tier is an uncoloured tier plus ONE
    /// `opret` output and nothing else, so the coloured fee is exactly the uncoloured fee for one
    /// MORE output — no residue.
    ///
    /// The `+ 1 vB` term this assertion used to carry was never a coloured-lane cost: it was the
    /// explicit `SIGHASH_ALL` byte, which is on EVERY TES-R tier, and it appeared here only because
    /// `mercurylib::tesr::TIER_VBYTES` still modelled a 64-byte `SIGHASH_DEFAULT` witness. With that
    /// constant corrected 124 → 125 the term is gone and the relation is exact — a TIGHTENING: any
    /// future 1-vB drift between the two lanes now fails here instead of cancelling out.
    #[test]
    fn the_coloured_fee_is_exactly_the_uncoloured_fee_for_one_more_output() {
        let rate = 2.0;
        for n_payload in 1..=4usize {
            assert_eq!(
                colored_committed_fee(n_payload, rate),
                committed_fee_for_outputs(n_payload + 1, rate),
                "n={n_payload}: the opret is exactly one P2TR output and costs exactly that"
            );
        }
        // 168 vB * 2 = 336 over the MEASURED 168 vB coloured 1-payload tier (was 334 / 167 vB).
        assert_eq!(colored_committed_fee(1, rate), 336);
        // (168 + 2*43) = 254 vB * 2 = 508 over the MEASURED 254 vB coloured 3-payload split state.
        assert_eq!(colored_committed_fee(3, rate), 508);
        assert!(
            colored_committed_fee(1, rate) > mercurylib::tesr::committed_fee(rate),
            "a coloured tier must pay MORE than an uncoloured one, or it cannot relay standalone"
        );
    }

    /// **THE SURCHARGE IDENTITY, MEASURED ON BOTH LANES** — the constant-level statement of what
    /// `SDK_E2E=74` asserts on a live coloured ladder:
    /// `colored_committed_fee(1, r) − committed_fee(r) == 43·r`.
    ///
    /// Both sides are finalised through the **production** finaliser and measured with
    /// `Transaction::vsize()`; nothing here restates a constant. That is what makes this evidence:
    /// [D4] was found by measurement and was fixed on the coloured half only, which broke this
    /// identity in the direction that LOOKS harmless (the coloured lane over-paid relative to the
    /// plain one) while leaving every uncoloured tier under-paying its own standalone relay. Fixing
    /// one half alone now fails here.
    #[test]
    fn the_coloured_surcharge_is_exactly_one_opret_output() {
        // MEASURED, both lanes, same finaliser.
        let colored_measured = finalised_vsize(&tier_shape(1));
        let plain_measured = finalised_vsize(&plain_tier_shape(1));
        assert_eq!(colored_measured, 168, "coloured 1-payload tier measures 168 vB");
        assert_eq!(plain_measured, 125, "uncoloured 1-payload tier measures 125 vB");
        assert_eq!(
            colored_measured - plain_measured,
            P2TR_OUT_VBYTES,
            "the only difference between the lanes is the opret output"
        );

        // Both models must equal their own measurement...
        assert_eq!(colored_tier_vbytes(1), colored_measured);
        assert_eq!(mercurylib::tesr::TIER_VBYTES, plain_measured);
        // ...at every arity a split state can reach.
        for n in 1..=4usize {
            assert_eq!(colored_tier_vbytes(n), finalised_vsize(&tier_shape(n)), "coloured n={n}");
            assert_eq!(
                mercurylib::tesr::TIER_VBYTES + (n as u64 - 1) * P2TR_OUT_VBYTES,
                finalised_vsize(&plain_tier_shape(n)),
                "uncoloured n={n}"
            );
        }

        // ...and therefore the FEE surcharge is the opret's 43 vB at the committed rate — the
        // `SDK_E2E=74` assertion verbatim, at several rates.
        for rate in [1.0f64, 2.0, 5.0, 10.0] {
            assert_eq!(
                colored_committed_fee(1, rate) - mercurylib::tesr::committed_fee(rate),
                (P2TR_OUT_VBYTES as f64 * rate).ceil() as u64,
                "rate {rate}: coloured surcharge must be exactly the opret's 43 vB"
            );
        }
        assert_eq!(colored_committed_fee(1, 2.0) - mercurylib::tesr::committed_fee(2.0), 43 * 2);
        // And the plain fee is what the plain transaction actually needs: 250 sat over 125 vB.
        assert_eq!(mercurylib::tesr::committed_fee(2.0), 250);
        assert_eq!(mercurylib::tesr::committed_fee(2.0), plain_measured * 2);
    }

    #[test]
    fn the_coloured_budget_leaves_room_for_the_anchor_and_the_fee() {
        let rate = 2.0;
        let parent = 50_000u64;
        let total = colored_tier_out_total(parent, 3, rate).unwrap();
        assert_eq!(total + colored_committed_fee(3, rate) + P2A_VALUE, parent);
        assert_eq!(colored_tier_out_value(parent, rate), colored_tier_out_total(parent, 1, rate));
        // A parent that cannot even cover the coloured fee must say so rather than underflow.
        assert!(colored_tier_out_total(100, 1, rate).is_none());
    }

    #[test]
    fn payload_vouts_exclude_both_the_opret_and_the_anchor() {
        let opret = TxOut {
            value: 0,
            script_pubkey: ScriptBuf::from(hex::decode(format!("6a20{}", "11".repeat(32))).unwrap()),
        };
        assert!(opret.script_pubkey.is_op_return());
        let payload = TxOut {
            value: 10_000,
            script_pubkey: ScriptBuf::from(hex::decode(format!("5120{}", "22".repeat(32))).unwrap()),
        };
        let anchor = TxOut { value: P2A_VALUE, script_pubkey: p2a_script() };
        let tx = Transaction {
            version: 3,
            lock_time: absolute::LockTime::from_consensus(0),
            input: vec![],
            output: vec![opret, payload.clone(), payload.clone(), anchor],
        };
        assert_eq!(colored_payload_vouts(&tx), vec![1, 2]);
        // The accident CTESR-GATE §4.3 names: the naive filter counts the anchor as a payload.
        let naive: Vec<u32> = tx
            .output
            .iter()
            .enumerate()
            .filter(|(_, o)| !o.script_pubkey.is_op_return())
            .map(|(v, _)| v as u32)
            .collect();
        assert_eq!(naive, vec![1, 2, 3], "the P2A anchor is not an OP_RETURN");
        assert!(!p2a_script().is_op_return());
    }
}

