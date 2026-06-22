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
use bitcoin::psbt::Psbt;
use electrum_client::ElectrumApi;
use mercurylib::transaction::{
    create_signature, get_partial_sig_request_for_colored_tx, get_unsigned_backup_psbt,
    new_backup_transaction,
};
use mercurylib::wallet::Coin;
use mercury_rgb::RgbWallet;

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
    /// non-OP_RETURN output: vout 1 when rgb-lib placed the OP_RETURN first (taproot recipient), or
    /// vout 0 when it appended the OP_RETURN last (non-taproot recipient).
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

    // The recipient/owner output is the (single) non-OP_RETURN output. rgb-lib places the OP_RETURN
    // commitment first when a taproot output is present (so the recipient is at vout 1), but appends
    // it last for non-taproot recipients (recipient stays at vout 0) - so compute it, don't assume.
    let recipient_vout = colored_unsigned_tx
        .output
        .iter()
        .position(|o| !o.script_pubkey.is_op_return())
        .unwrap_or(0) as u32;

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

/// RGB-over-statechain status annotation, layered on top of the Mercury `CoinStatus`.
/// See `docs/rgb_anchor_refresh.md` for the full state machine.
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
/// Flow (see `docs/rgb_anchor_refresh.md`): `new_transfer_address` (same wallet) ->
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
    // the self output (anchor self-refresh, rgb07); `Some(recipient_id)` assigns to a *receiver's*
    // blinded seal (off-chain P2P transfer, rgb08) - the asset moves, the sats do not.
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
