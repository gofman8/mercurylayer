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
    let mut output_map = std::collections::HashMap::new();
    output_map.insert(0u32, rgb_amount);
    let (colored_psbt_b64, consignment) =
        rgb.color(&unsigned_psbt_b64, contract_id, output_map, blinding)?;

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
