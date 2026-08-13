use std::str::FromStr;

use bitcoin::{hashes::{sha256, Hash}, sighash::{self, SighashCache, TapSighashType}, taproot::TapTweakHash, Address, PrivateKey, Sequence, Transaction, TxOut, Txid};
use secp256k1_zkp::{PublicKey, schnorr::Signature, Secp256k1, Message, XOnlyPublicKey, musig::{MusigPubNonce, BlindingFactor, blinded_musig_pubkey_xonly_tweak_add, MusigAggNonce, MusigSession}, SecretKey, Scalar, KeyPair};
use serde::{Serialize, Deserialize};

use crate::{error::MercuryError, utils::get_network, wallet::{get_previous_outpoint, BackupTx, Coin, CoinStatus, Wallet}};

use super::{TransferMsg, TxOutpoint};

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferUnlockRequestPayload { 
    pub statechain_id: String,
    pub auth_sig: String, // signed_statechain_id
    pub auth_pub_key: Option<String>, // public key for verification
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferReceiverRequestPayload { 
    pub statechain_id: String,
    pub batch_data: Option<String>,
    pub t2: String,
    pub auth_sig: String,
}

/// **[D38/D18] The wire error vocabulary. These strings ARE the interface.**
///
/// Four client profiles across five sites compare them as literals — `clients/libs/nodejs`,
/// `clients/libs/web`, and both generated Kotlin trees — and three other specs' conformance tables
/// point at `ERR-n` entries that index into them. Prose is not an interface; this enum is.
///
/// So every variant carries an explicit `serde(rename)` **equal to its own identifier**. That looks
/// redundant and is not: without it the wire value is a by-product of the Rust name, and a future
/// identifier rename — a refactor nobody would think to call a protocol change — silently moves the
/// interface. With it, the two are separable, and changing the wire value becomes a deliberate act.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TransferReceiverError {
    #[serde(rename = "StatecoinBatchLockedError")]
    StatecoinBatchLockedError,
    #[serde(rename = "ExpiredBatchTimeError")]
    ExpiredBatchTimeError,
    /// The transfer this recipient is trying to claim was CANCELLED (see
    /// `crate::transfer::cancel`). Returned with HTTP 410 and only to a caller that proved it holds
    /// the recorded recipient key, so it is never an existence oracle for third parties.
    ///
    /// This variant exists so the wallet can say "this payment was cancelled" instead of the
    /// silent "nothing arrived" the client's `println!` + `continue` loop produces for every other
    /// mailbox miss — a cancelled payment that looks like an idle mailbox is the exact
    /// silent-degradation shape a recipient must never be shown.
    #[serde(rename = "TransferCancelledError")]
    TransferCancelledError,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferReceiverErrorResponsePayload {
    pub code: TransferReceiverError,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferReceiverPostResponsePayload {
    pub server_pubkey: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct KeyUpdateResponsePayload { 
    pub statechain_id: String,
    pub t2: String,
    pub x1: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct GetMsgAddrResponsePayload {
    pub list_enc_transfer_msg: Vec<String>,
}
 
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StatechainInfo {
    pub statechain_id: String,
    pub server_pubnonce: String,
    pub challenge: String,
    pub tx_n: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StatechainInfoResponsePayload {
    pub enclave_public_key: String,
    pub num_sigs: u32,
    pub statechain_info: Vec<StatechainInfo>,
    pub x1_pub: Option<String>,
    /// [FATAL-B] The AUTHORITATIVE aggregate x-only key the server recorded for this statechain_id at
    /// deposit (owner_share + enclave_share), hex. `None` for coins deposited before the owner-share
    /// binding (old clients/servers). A receiver checks a coin's on-chain aggregate against this instead
    /// of trusting a sender-supplied owner key, closing the split decoy-counter path. `serde(default)`
    /// keeps old clients/servers interoperable.
    #[serde(default)]
    pub aggregate_pubkey: Option<String>,
    /// [D8] BIP-340 Schnorr signature by THIS coin's SE key over
    /// `sha256("utexo/sig_count/v2" || statechain_id || u32_be(num_sigs) || u8(has_budget)
    ///          || u32_be(budget) || nonce32)`, hex.
    ///
    /// `num_sigs` is the right-hand side of the receiver's anti-theft census, and it used to arrive
    /// as a bare integer: a coordinator that under-reported it by `k` hid `k` co-signed rival states
    /// while the exact-equality census still balanced, and the receiver accepted a coin the sender
    /// could still reclaim. This closes that by making the count say who vouched for it.
    ///
    /// `None` means **UNATTESTED**, never "fine" — either the client asked for no nonce, or the
    /// deployment predates the attestation. A verifier must decide which of those it accepts;
    /// silently treating `None` as verified reintroduces the defect.
    #[serde(default)]
    pub sig_count_attestation: Option<String>,
    /// The x-only public key that signs [`Self::sig_count_attestation`], hex. It is THIS coin's SE
    /// key — the same key the receiver already binds to the on-chain tx0 output
    /// (`validate_tx0_output_pubkey`), so the attestation introduces no new trust anchor. A verifier
    /// MUST check it against that chain-anchored value rather than trusting the field, or the
    /// coordinator could sign with a key of its own choosing.
    #[serde(default)]
    pub sig_count_attestation_pubkey: Option<String>,
    /// [D8(i)] Whether the ENCLAVE holds a spend budget for this coin.
    ///
    /// Carried separately from [`Self::sig_budget`] because "no budget" (co-signable indefinitely,
    /// the default) and "budget 0" (terminal, nothing more may be signed) are opposite facts. A
    /// single `Option<u32>` on the wire would let a coordinator turn the second into the first by
    /// omitting a field — so presence is stated, and it is covered by the attestation signature.
    ///
    /// `None` means the enclave reported neither, i.e. it predates the field. A verifier must treat
    /// that as unattested, not as "no budget".
    #[serde(default)]
    pub has_sig_budget: Option<bool>,
    /// The enclave's spend budget for this coin, when it has one. **Not the coordinator's copy** —
    /// the coordinator holds and enforces its own, but a receiver relying on terminality cannot take
    /// its word for it. This value is inside the `utexo/sig_count/v2` preimage, so altering it in
    /// flight breaks verification.
    #[serde(default)]
    pub sig_budget: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct NewKeyInfo {
    pub aggregate_pubkey: String,
    pub aggregate_address: String,
    pub signed_statechain_id: String,
    pub amount: u32,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn duplicate_coin_to_initialized_state(wallet: &Wallet, auth_pubkey: &str) -> Result<Coin, MercuryError> {

    let coin = wallet.coins.iter().find(|coin| coin.auth_pubkey == auth_pubkey.to_string());

    if coin.is_none() {
        return Err(MercuryError::CoinNotFound);
    }

    let coin = coin.unwrap();

    Ok(Coin {
        index: coin.index,
        user_privkey: coin.user_privkey.clone(),
        user_pubkey: coin.user_pubkey.clone(),
        auth_privkey: coin.auth_privkey.clone(),
        auth_pubkey: coin.auth_pubkey.clone(),
        derivation_path: coin.derivation_path.clone(),
        fingerprint: coin.fingerprint.clone(),
        address: coin.address.clone(),
        backup_address: coin. backup_address.clone(),
        server_pubkey: None,
        aggregated_pubkey: None,
        aggregated_address: None,
        utxo_txid: None,
        utxo_vout: None,
        amount: None,
        statechain_id: None,
        signed_statechain_id: None,
        locktime: None,
        secret_nonce: None,
        public_nonce: None,
        blinding_factor: None,
        server_public_nonce: None,
        tx_cpfp: None,
        tx_withdraw: None,
        withdrawal_address: None,
        status: CoinStatus::INITIALISED,
        duplicate_index: coin.duplicate_index,
        single_use: coin.single_use,
        epoch_deadline: coin.epoch_deadline,
    })
}   

pub fn decrypt_transfer_msg(encrypted_message: &str, private_key_wif: &str) -> Result<TransferMsg, MercuryError> {

    let client_auth_key = PrivateKey::from_wif(private_key_wif)?.inner;

    let decoded_enc_message = hex::decode(encrypted_message)?;

    let decrypted_msg = ecies::decrypt(client_auth_key.secret_bytes().as_slice(), decoded_enc_message.as_slice()).unwrap();

    let decrypted_msg_str = String::from_utf8(decrypted_msg).unwrap();

    let transfer_msg: TransferMsg = serde_json::from_str(decrypted_msg_str.as_str()).unwrap();

    Ok(transfer_msg)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn get_tx0_outpoint(backup_transactions: &Vec<BackupTx>) -> Result<TxOutpoint, MercuryError> {

    let mut backup_transactions = backup_transactions.clone();

    backup_transactions.sort_by(|a, b| a.tx_n.cmp(&b.tx_n));

    let bkp_tx1 = backup_transactions.first().ok_or(MercuryError::NoBackupTransactionFound)?;

    get_previous_outpoint(bkp_tx1)
}

pub fn verify_transfer_signature(new_user_pubkey: &str, tx0_outpoint: &TxOutpoint, transfer_msg: &TransferMsg) -> Result<bool, MercuryError> {

    let new_user_pubkey = PublicKey::from_str(new_user_pubkey)?;
    let sender_public_key = PublicKey::from_str(&transfer_msg.user_public_key)?.x_only_public_key().0;

    let input_vout = tx0_outpoint.vout;
    let input_txid = Txid::from_str(&tx0_outpoint.txid)?;

    let signature = Signature::from_str(&transfer_msg.transfer_signature)?;

    let secp = Secp256k1::new();

    let mut data_to_verify = Vec::<u8>::new();
    data_to_verify.extend_from_slice(&input_txid[..]);
    data_to_verify.extend_from_slice(&input_vout.to_le_bytes());
    data_to_verify.extend_from_slice(&new_user_pubkey.serialize()[..]);

    let msg = Message::from_hashed_data::<sha256::Hash>(&data_to_verify);

    Ok(secp.verify_schnorr(&signature, &msg, &sender_public_key).is_ok())
}

/// **[D8] Verify the SE's attestation over a coin's signature count.**
///
/// `num_sigs` is the right-hand side of the receiver's anti-theft census
/// (`se_num_sigs == flat_backups + tiers + superseded`, exact equality). It used to arrive as a bare
/// integer with no signature at any hop, so a coordinator that UNDER-REPORTED it by `k` hid `k`
/// co-signed rival states: the census still balanced exactly, and the receiver accepted a coin the
/// sender could still reclaim. Theft, resting on coordinator honesty alone.
///
/// **The verifying key must be the chain-anchored one.** `attestation_pubkey` as served is just a
/// field the coordinator could fill with a key it controls; pass the `enclave_public_key` this
/// receiver has already bound to the on-chain tx0 output via [`validate_tx0_output_pubkey`], and
/// this function refuses if the two disagree. That is what makes the attestation mean anything.
///
/// `nonce_hex` must be the 32-byte challenge THIS caller generated for THIS request. Without it a
/// coordinator could replay a genuine attestation captured when the count was legitimately lower.
///
/// Returns `Ok(())` only if the signature verifies over the exact preimage the SE signed:
/// `sha256("utexo/sig_count/v2" || statechain_id || u32_be(num_sigs) || u8(has_budget)
///          || u32_be(budget) || nonce32)`.
///
/// `budget` is the coin's SE-enforced spend budget, `None` when it has none. It is part of the SAME
/// preimage rather than a second attestation because the two are only meaningful together: a count
/// says how many co-signatures exist, and only the count PLUS the budget says whether another can
/// ever be issued — which is what a receiver relying on terminality actually needs. Two separate
/// attestations could be mixed across time, pairing a fresh count with a stale budget.
pub fn verify_sig_count_attestation(
    statechain_id: &str,
    num_sigs: u32,
    budget: Option<u32>,
    nonce_hex: &str,
    attestation_hex: &str,
    attestation_pubkey_hex: &str,
    chain_anchored_enclave_pubkey: &str,
) -> Result<(), MercuryError> {
    // The lockbox emits every key it serialises through `key_to_string`, which PREFIXES "0x"
    // (lockbox/src/utils.cpp:56-62). `from_str` does not accept that, so strip it here rather than
    // changing a wire convention the rest of the SE API already uses. Found by testing against the
    // live endpoint — the unit tests generated their own hex and so could not have caught it.
    let strip0x = |s: &str| s.strip_prefix("0x").unwrap_or(s).to_string();

    // 1. The signer must be the key already bound to the on-chain tx0 output. `enclave_public_key`
    //    is a 33-byte compressed key; the attestation carries its 32-byte x-only half.
    let anchored = PublicKey::from_str(&strip0x(chain_anchored_enclave_pubkey))
        .map_err(|_| MercuryError::SigCountAttestationInvalid)?;
    let (anchored_xonly, _) = anchored.x_only_public_key();
    let claimed_xonly = XOnlyPublicKey::from_str(&strip0x(attestation_pubkey_hex))
        .map_err(|_| MercuryError::SigCountAttestationInvalid)?;
    if anchored_xonly != claimed_xonly {
        // The coordinator signed with a key of its own choosing. That is the attack this check is
        // for, so it is a refusal, not a warning.
        return Err(MercuryError::SigCountAttestationInvalid);
    }

    // 2. Rebuild the preimage EXACTLY as the SE built it (lockbox/src/server.cpp, /signature_count).
    //    `num_sigs` big-endian and fixed-width: a decimal string would let two different
    //    (id, count) pairs collide under concatenation.
    let nonce = hex::decode(nonce_hex).map_err(|_| MercuryError::SigCountAttestationInvalid)?;
    if nonce.len() != 32 {
        return Err(MercuryError::SigCountAttestationInvalid);
    }
    let mut preimage: Vec<u8> = Vec::new();
    preimage.extend_from_slice(b"utexo/sig_count/v2");
    preimage.extend_from_slice(statechain_id.as_bytes());
    preimage.extend_from_slice(&num_sigs.to_be_bytes());
    // [D8(i)] The BUDGET travels inside the same signature as the count. A receiver's question is
    // not "how many co-signatures exist" but "can another ever be issued", and only the pair answers
    // it. Presence is an explicit byte because "no budget" (co-signable forever) and "budget 0"
    // (terminal) are opposite facts that must not share an encoding.
    preimage.push(u8::from(budget.is_some()));
    preimage.extend_from_slice(&budget.unwrap_or(0).to_be_bytes());
    preimage.extend_from_slice(&nonce);

    let msg = Message::from_hashed_data::<sha256::Hash>(&preimage);
    let signature = Signature::from_str(&strip0x(attestation_hex))
        .map_err(|_| MercuryError::SigCountAttestationInvalid)?;

    Secp256k1::new()
        .verify_schnorr(&signature, &msg, &claimed_xonly)
        .map_err(|_| MercuryError::SigCountAttestationInvalid)
}

pub fn validate_tx0_output_pubkey(enclave_public_key: &str, transfer_msg: &TransferMsg, tx0_outpoint: &TxOutpoint, tx0_hex: &str, network: &str) -> Result<bool, MercuryError> {

    let network = get_network(&network)?;

    let enclave_public_key = PublicKey::from_str(enclave_public_key).unwrap();
    let sender_public_key = PublicKey::from_str(&transfer_msg.user_public_key)?;

    let transfer_aggregate_pubkey = sender_public_key.combine(&enclave_public_key).unwrap();
    let transfer_aggregate_xonly_pubkey = transfer_aggregate_pubkey.x_only_public_key().0;

    let secp = Secp256k1::new();

    let transfer_aggregate_address = Address::p2tr(&secp, transfer_aggregate_xonly_pubkey, None, network);

    let transfer_aggregate_xonly_pubkey = XOnlyPublicKey::from_slice(transfer_aggregate_address.script_pubkey()[2..].as_bytes()).unwrap();

    let tx0: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx0_hex)?)?;

    let tx0_output = tx0.output[tx0_outpoint.vout as usize].clone();

    let tx0_output_xonly_pubkey = XOnlyPublicKey::from_slice(tx0_output.script_pubkey[2..].as_bytes()).unwrap();

    Ok(transfer_aggregate_xonly_pubkey == tx0_output_xonly_pubkey)
}

pub fn verify_latest_backup_tx_pays_to_user_pubkey(transfer_msg: &TransferMsg, client_pubkey_share: &str, network: &str) -> Result<bool, MercuryError> {

    let client_pubkey_share = PublicKey::from_str(&client_pubkey_share)?;
    
    let network = get_network(&network)?;

    let last_bkp_tx = transfer_msg.backup_transactions.last();

    if last_bkp_tx.is_none() {
        return Err(MercuryError::NoBackupTransactionFound);
    }

    let last_bkp_tx = last_bkp_tx.unwrap();

    let last_tx: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&last_bkp_tx.tx)?)?;

    // Skip the OP_RETURN opret commitment output (present on RGB-enabled coins) and check the
    // spendable output pays to the new owner's key.
    let output = last_tx
        .output
        .iter()
        .find(|o| !o.script_pubkey.is_op_return())
        .ok_or(MercuryError::NoBackupTransactionFound)?;

    let aggregate_address = Address::p2tr(&Secp256k1::new(), client_pubkey_share.x_only_public_key().0, None, network);

    Ok(output.script_pubkey == aggregate_address.script_pubkey())
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn get_output_address_from_tx0(tx0_outpoint: &TxOutpoint, tx0_hex: &str, network: &str) -> Result<String, MercuryError> {

    let network = get_network(&network)?;

    let tx0: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx0_hex)?)?;

    let tx0_output = tx0.output[tx0_outpoint.vout as usize].clone();

    let output_script_pubkey = tx0_output.script_pubkey;

    let address = Address::from_script(&output_script_pubkey.as_script(), network)?;

    Ok(address.to_string())
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn get_amount_from_tx0(tx0_hex: &str, tx0_outpoint: &TxOutpoint,) -> Result<u64, MercuryError> {

    let tx0: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx0_hex)?)?;

    // Audit [24]: return errors instead of panicking on an attacker-shaped tx0/outpoint. A crafted
    // decryptable transfer message could otherwise abort the SSP peek task via an out-of-range vout.
    if tx0_outpoint.txid != tx0.txid().to_string() {
        return Err(MercuryError::ParseError);
    }
    let out = tx0
        .output
        .get(tx0_outpoint.vout as usize)
        .ok_or(MercuryError::OutOfRangeError)?;
    Ok(out.value)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
/// A valid backup ladder decrements by EXACTLY `interval` per hop (INV-5 / receiver check R5): the
/// current owner holds the lowest locktime. Returns false for a wrong gap OR a non-decreasing ladder
/// (`checked_sub` yields `None` when `current > prev`, so a forged increasing ladder is rejected
/// cleanly instead of underflow-panicking).
pub fn ladder_decrements_by_interval(prev_lock_time: u32, current_lock_time: u32, interval: u32) -> bool {
    prev_lock_time.checked_sub(current_lock_time) == Some(interval)
}

/// **[S2] Backup-chain validation for a LADDERED coin.** A laddered (TES-R) coin still conveys the
/// signed-once backup chain AND still feeds its COUNT into `verify_bundle`'s anti-theft equation
/// (`flat_backups`), so those backups must be validated — otherwise a sender can (a) pad the vector
/// with duplicate `tx1`s to inflate the count and absorb a hidden co-signed state, or (b) invert the
/// chain (receiver-paying backup at `L+interval`, own at `L`) so their stale backup matures FIRST.
///
/// This is [`validate_signature_scheme`] **minus the `tx_n`-keyed `statechain_info` lookup and its
/// `verify_blinded_musig_scheme`**, which cannot run for a laddered coin: the SE indexes
/// `statechain_info` per CO-SIGN, and a laddered coin's tiers consume co-sign slots, so a backup's
/// `tx_n` no longer aligns with the SE's index and the lookup returns a *tier's* blinding info.
/// Everything that defends the two attacks above is retained — critically
/// `ladder_decrements_by_interval` (INV-5), which rejects duplicates (decrement 0 ≠ interval) and
/// inversions (increasing locktimes) alike.
///
/// RESIDUAL: the backup chain's blinded-MuSig commitments are not verified for a laddered coin (the
/// tiers are, via `verify_bundle`'s signature check). Closing it needs the SE's `tx_n` ↔ co-sign
/// indexing to distinguish tier co-signs from backup co-signs — tracked in
/// `docs/utexo/history/SPLIT-FINDINGS.md`.
pub fn validate_backup_chain_v2(
    backup_transactions: &Vec<BackupTx>,
    tx0_hex: &str,
    current_blockheight: u32,
    fee_rate_tolerance: f64,
    current_fee_rate_sats_per_byte: f64,
    lockheight_init: u32,
    interval: u32) -> Result<u32, MercuryError> {

    if backup_transactions.is_empty() {
        return Err(MercuryError::NoPreviousLockTimeError);
    }

    let mut previous_lock_time: Option<u32> = None;

    for backup_tx in backup_transactions.iter() {
        verify_transaction_signature(&backup_tx.tx, &tx0_hex, fee_rate_tolerance, current_fee_rate_sats_per_byte)
            .map_err(|_| MercuryError::SignatureSchemeValidationError)?;
        verify_transaction_sequence(&backup_tx.tx)
            .map_err(|_| MercuryError::SignatureSchemeValidationError)?;
        verify_if_locktime_is_reasonable_tx_version_and_output_size(&backup_tx.tx, current_blockheight, lockheight_init)
            .map_err(|_| MercuryError::SignatureSchemeValidationError)?;
        reconstruct_transaction(&backup_tx.tx)
            .map_err(|_| MercuryError::SignatureSchemeValidationError)?;

        let current_lock_time = crate::utils::get_blockheight(&backup_tx)?;
        if let Some(prev_lock_time) = previous_lock_time {
            // INV-5: each hop MUST decrement by EXACTLY `interval`. Rejects duplicate-padding (0) and
            // ladder inversion (increasing) — the two S2 attacks.
            if !ladder_decrements_by_interval(prev_lock_time, current_lock_time, interval) {
                return Err(MercuryError::SignatureSchemeValidationError);
            }
        }
        previous_lock_time = Some(current_lock_time);
    }

    previous_lock_time.ok_or(MercuryError::NoPreviousLockTimeError)
}

pub fn validate_signature_scheme(
    backup_transactions: &Vec<BackupTx>,
    statechain_info: &StatechainInfoResponsePayload,
    tx0_hex: &str,
    current_blockheight: u32,
    fee_rate_tolerance: f64,
    current_fee_rate_sats_per_byte: f64,
    lockheight_init: u32,
    interval: u32) -> Result<u32, MercuryError> {

    let mut previous_lock_time: Option<u32> = None;

    let mut sig_scheme_validation = true;

    for backup_tx in backup_transactions.iter() {

        let statechain_info = statechain_info.statechain_info
            .iter()
            .find(|info| info.tx_n == backup_tx.tx_n);

        if statechain_info.is_none() {
            println!("statechain_info not found");
            sig_scheme_validation = false;
            break;
        }

        let statechain_info = statechain_info.unwrap();

        let is_signature_valid = verify_transaction_signature(&backup_tx.tx, &tx0_hex, fee_rate_tolerance, current_fee_rate_sats_per_byte);
        if is_signature_valid.is_err() {
            println!("{}", is_signature_valid.err().unwrap().to_string());
            sig_scheme_validation = false;
            break;
        }

        let is_blinded_musig_scheme_valid = verify_blinded_musig_scheme(&backup_tx, &tx0_hex, statechain_info);
        if is_blinded_musig_scheme_valid.is_err() {
            println!("{}", is_blinded_musig_scheme_valid.err().unwrap().to_string());
            sig_scheme_validation = false;
            break;
        }

        if verify_transaction_sequence(&backup_tx.tx).is_err() {
            println!("transaction sequence is not correct");
            sig_scheme_validation = false;
            break;
        }

        if verify_if_locktime_is_reasonable_tx_version_and_output_size(&backup_tx.tx, current_blockheight, lockheight_init).is_err() {
            println!("locktime is not reasonable");
            sig_scheme_validation = false;
            break;
        }

        if reconstruct_transaction(&backup_tx.tx).is_err() {
            println!("reconstruction failed");
            sig_scheme_validation = false;
            break;
        }

        if previous_lock_time.is_some() {
            let prev_lock_time = previous_lock_time.unwrap();
            let current_lock_time = crate::utils::get_blockheight(&backup_tx)?;
            // Each hop's backup MUST decrement the locktime by EXACTLY `interval` (INV-5): the
            // current owner's backup is the lowest, so a stale one matures later. `checked_sub`
            // also rejects a non-decreasing ladder without underflow-panicking (the old
            // `prev - current` u32 subtraction would panic in debug on a forged increasing ladder).
            if !ladder_decrements_by_interval(prev_lock_time, current_lock_time, interval) {
                println!("interval is not correct");
                sig_scheme_validation = false;
                break;
            }
        }

        previous_lock_time = Some(crate::utils::get_blockheight(&backup_tx)?);
    }

    if !sig_scheme_validation {
        return Err(MercuryError::SignatureSchemeValidationError);
    }

    if previous_lock_time.is_none() {
        return Err(MercuryError::NoPreviousLockTimeError);
    }

    Ok(previous_lock_time.unwrap())
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn verify_transaction_signature(tx_n_hex: &str, tx0_hex: &str, fee_rate_tolerance: f64, current_fee_rate_sats_per_byte: f64) -> Result<(), MercuryError> {

    let tx_n: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx_n_hex)?)?;

    let witness = tx_n.input[0].witness.clone();

    let witness_data = witness.nth(0).unwrap();

    // the last element is the hash type
    let signature_data = witness_data.split_last().unwrap().1;

    let signature = Signature::from_slice(signature_data).unwrap();

    let tx0: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx0_hex)?)?;

    let vout = tx_n.input[0].previous_output.vout as usize;

    let tx0_output = tx0.output[vout].clone();

    let xonly_pubkey = XOnlyPublicKey::from_slice(tx0_output.script_pubkey[2..].as_bytes()).unwrap();

    let sighash_type = TapSighashType::All;

    let hash = SighashCache::new(tx_n.clone()).taproot_key_spend_signature_hash(
        0,
        &sighash::Prevouts::All(&[TxOut {
            value: tx0_output.value,
            script_pubkey: tx0_output.script_pubkey.clone(),
        }]),
        sighash_type,
    ).unwrap();

    let msg: Message = hash.into();

    // Sum all outputs (the OP_RETURN commitment output, if present, has zero value) so the fee is
    // computed correctly regardless of whether the backup tx carries an RGB commitment.
    let total_output_value: u64 = tx_n.output.iter().map(|o| o.value).sum();
    let fee = tx0_output.value - total_output_value;
    let fee_rate = fee as f64 / tx_n.vsize() as f64;

    if (fee_rate + fee_rate_tolerance) < current_fee_rate_sats_per_byte {
        return Err(MercuryError::FeeTooLow);
    }

    if (fee_rate - fee_rate_tolerance) > current_fee_rate_sats_per_byte {
        return Err(MercuryError::FeeTooHigh);
    }

    if !Secp256k1::new().verify_schnorr(&signature, &msg, &xonly_pubkey).is_ok() {
        return Err(MercuryError::InvalidSignature);
    }

    Ok(())

}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn verify_transaction_sequence(tx_n_hex: &str) -> Result<(), MercuryError> {

    let tx_n: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx_n_hex)?)?;

    if tx_n.input.is_empty() {
        return Err(MercuryError::EmptyInput);
    }

    if tx_n.input.len() > 1 {
        return Err(MercuryError::Tx1HasMoreThanOneInput);
    }

    let input = tx_n.input.first().unwrap();

    if input.sequence != Sequence(0) {
        return Err(MercuryError::TransactionSequenceDifferentThanZeroError);
    }

    Ok(())
}

pub fn verify_if_locktime_is_reasonable_tx_version_and_output_size(tx_n_hex: &str, current_blockheight: u32, lockheight_init:u32) -> Result<(), MercuryError> {

    let tx_n: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx_n_hex)?)?;

    if tx_n.version != 2 {
        return Err(MercuryError::TransactionVersionError);
    }

    // An RGB-enabled backup transaction carries exactly one extra zero-value OP_RETURN output
    // holding the opret commitment. There must be exactly one spendable (non-OP_RETURN) output
    // (paying the owner/recipient) and at most one OP_RETURN commitment output.
    let op_return_outputs = tx_n.output.iter().filter(|o| o.script_pubkey.is_op_return()).count();
    let payment_outputs = tx_n.output.len() - op_return_outputs;

    if payment_outputs != 1 {
        return Err(MercuryError::TxHasMoreThanOneOutput);
    }

    if op_return_outputs > 1 {
        return Err(MercuryError::TxHasMoreThanOneOutput);
    }

    let lock_time = tx_n.lock_time;

    if !(lock_time.is_block_height()) {
        return Err(MercuryError::LocktimeNotBlockHeightError);
    }

    if lock_time.to_consensus_u32() <= current_blockheight {
        return Err(MercuryError::LocktimeTooLow);
    }

    if current_blockheight + lockheight_init < lock_time.to_consensus_u32() {
        return Err(MercuryError::LocktimeTooHigh);
    }

    Ok(())
}

// =================================================================================================
// [P0-1 / P0-2] EXIT-HEADROOM ADMISSION — the epoch a conveyed off-chain coin must fit inside.
//
// THE CLOCK. Every tier of a TES-R tree is timelocked RELATIVELY (BIP-68 CSV, measured from its
// parent's confirmation), so nothing in the tree has a calendar deadline of its own. Exactly ONE
// absolute clock exists anywhere in the structure: the FLAT backup chain over the funding outpoint
// `F`. Those transactions are `nLockTime`d, they are held by the coin's previous owners, and they
// spend the very outpoint the trigger `T` spends. The lowest locktime in that chain belongs to the
// CURRENT owner (INV-5, `ladder_decrements_by_interval`), so `min(locktime)` is the first height at
// which anybody can broadcast a transaction that spends `F` out from under the whole tree. Call it
// the coin's EPOCH EXPIRY. A split child never has flat backups of its own (`CHILD_V2_BASELINE == 0`
// — its funding `SP.out[j]` is un-broadcast and was never deposited), so its epoch is inherited
// wholesale from the root parent's chain, which is why that chain is conveyed with the bundle.
//
// THE REQUIREMENT. The exit is a strictly sequential chain — `T -> X_m -> SP -> (ext, state)* ->
// ext_child -> state_child` — and each tier's relative timelock only starts counting once its parent
// is CONFIRMED. So the wall-clock cost of materialising the coin is
//
//     required = Σ_i (csv_i + conf_i)  with conf_i >= 1 block per tier
//
// which for the mainnet schedule and a depth-`d` child is `2124·d + 2160` blocks of timelock plus
// `3 + 2d` confirmations. `lockheight_init` is 10 000, so a depth-1 child (4 284 + 5) does not fit in
// the last 43% of any epoch, and a depth-4 child (10 656 + 11) does not fit in a WHOLE epoch.
//
// WHY THE FULL CHAIN AND NOT JUST `T`. It is true that once `T` confirms, `F` is spent and every flat
// backup is dead forever — so a holder who force-exits the instant the deadline nears is safe with a
// single block of headroom. That is the WATCHTOWER's rule (`auto_exit_due`), not an ADMISSION rule,
// and the two answer different questions. A coin admitted with less headroom than its own exit takes
// is a coin that can never be idle: it obliges its new owner to be online and to start an irreversible
// unilateral exit inside the remaining window or lose everything, and it cannot be parked across the
// epoch boundary, re-transferred, or split further. That is not the instrument the census and Model A
// promise the payee, and the payee is the party who cannot inspect it after the fact. So the gate is
// the honest one: REFUSE unless the whole exit fits in the epoch the coin was minted in.
// =================================================================================================

/// A conveyed off-chain coin whose unilateral exit does NOT fit in the funding epoch it belongs to.
///
/// Named (not a bare string) because both users of the rule test it: the RECEIVER refuses such a
/// bundle at claim time ([`check_exit_headroom`]) and the SENDER refuses to mint one at split time
/// ([`max_split_depth`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "exit-headroom shortfall: materialising this coin takes {required} blocks ({tiers} exit \
     transactions, {csv_total} blocks of relative timelock plus one confirmation each), but only \
     {available} blocks remain before the funding epoch expires at height {epoch_expiry_height} \
     (chain tip {tip}) — short by {shortfall} blocks. The sender's flat backup can spend the \
     funding outpoint at {epoch_expiry_height} and void the whole tree, so this coin could not be \
     materialised in time and is refused."
)]
pub struct ExitHeadroomShortfall {
    /// Chain tip the check was made against.
    pub tip: u32,
    /// First height at which a flat backup over `F` can be broadcast (lowest conveyed locktime).
    pub epoch_expiry_height: u32,
    /// `epoch_expiry_height − tip`, saturating.
    pub available: u32,
    /// Blocks the exit needs (see [`exit_wait_blocks`]).
    pub required: u32,
    /// `required − available`.
    pub shortfall: u32,
    /// Number of transactions in the exit chain.
    pub tiers: u32,
    /// Sum of the chain's relative timelocks (the `2124·d + 2160` term).
    pub csv_total: u32,
}

/// A split refused because the child it would mint is too deep to survive one funding epoch.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "split depth cap: this split would mint a depth-{depth} child whose unilateral exit takes \
     {required} blocks, more than the {epoch_blocks}-block funding epoch — such a coin can never be \
     parked across an epoch boundary and its holder could never materialise it after a re-anchor. \
     The deepest child this schedule and epoch admit is depth {max_depth}."
)]
pub struct SplitDepthCapExceeded {
    /// Depth of the child the split would have created (a root split child is depth 1).
    pub depth: u32,
    /// Deepest admissible depth under the live schedule and epoch.
    pub max_depth: u32,
    /// Blocks the refused child's exit would need.
    pub required: u32,
    /// The funding epoch length (`lockheight_init`).
    pub epoch_blocks: u32,
}

/// A conveyed bundle whose DECLARED relative timelock disagrees with the one its own SIGNED
/// transaction carries.
///
/// **[B1] WHY THIS EXISTS.** The exit-headroom requirement is a function of the exit chain's
/// timelocks, and a `TesrTier` carries its timelock TWICE: once as a plain serde field (`csv`) that
/// travels with the conveyed bundle, and once — the only copy Bitcoin will ever enforce — inside the
/// signed transaction's `nSequence` (BIP-68). Until this check existed the admission gate read the
/// serde field. A sender therefore declared `csv: 1` on every tier, the gate computed a requirement
/// of a handful of blocks, and the coin it exists to refuse walked straight through it while the
/// chain still made its new owner wait the real thousands of blocks.
///
/// That is the audit's C-1 shape exactly: authority derived from the artifact being checked. The fix
/// is not to prefer one copy silently — a bundle whose two copies disagree did not come from this
/// code, and guessing which half is honest is a decision no verifier should make. Both are read, the
/// signed one is the authority, and disagreement is THIS refusal, by name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "declared-CSV mismatch at exit tier {index} ({tier}): the bundle DECLARES {declared}, but the \
     SIGNED transaction's nSequence encodes {signed} — and nSequence is what Bitcoin enforces. The \
     exit-headroom requirement is computed from the timelocks the chain will actually impose, so a \
     bundle whose declared schedule contradicts its own signatures is refused rather than admitted \
     on either value."
)]
pub struct DeclaredCsvMismatch {
    /// Position in the exit chain, broadcast order.
    pub index: u32,
    /// Which tier that is, in words (`"child extension"`, `"ancestor 0 state"`, …).
    pub tier: String,
    /// What the bundle's serde field claims.
    pub declared: String,
    /// What the signed transaction's `nSequence` encodes.
    pub signed: String,
}

/// How a relative timelock reads in a refusal — `None` is "no relative timelock", never "0", because
/// a tier declaring `csv: Some(0)` and a tier declaring `csv: None` are DIFFERENT claims that happen
/// to cost the same number of blocks, and a refusal that rendered them identically would be unusable
/// for telling which one the sender made.
fn render_csv(csv: Option<u16>) -> String {
    match csv {
        Some(c) => format!("a relative timelock of {c} block(s)"),
        None => "no relative timelock".to_string(),
    }
}

/// **[B1] BIND a declared timelock to the signed one.** Returns the SIGNED value (the authority) when
/// the two agree, and [`DeclaredCsvMismatch`] when they do not.
///
/// Callers pass `signed` straight from the parsed transaction's `nSequence`; this crate deliberately
/// does not parse it here, because the two client crates carry different `bitcoin` re-exports and a
/// second parse is a second chance to disagree with the verifier that already parsed it.
pub fn bind_declared_csv(
    index: usize,
    tier: &str,
    declared: Option<u16>,
    signed: Option<u16>,
) -> Result<Option<u16>, DeclaredCsvMismatch> {
    if declared != signed {
        return Err(DeclaredCsvMismatch {
            index: index as u32,
            tier: tier.to_string(),
            declared: render_csv(declared),
            signed: render_csv(signed),
        });
    }
    Ok(signed)
}

/// Blocks a pre-signed exit chain needs to complete, from broadcasting its first transaction.
///
/// `csvs` is the chain in broadcast order, one entry per transaction, `None` for a transaction with
/// no relative timelock (the trigger). Each tier contributes its own timelock PLUS the one block its
/// parent needs to confirm before that timelock starts counting — the minimum, and the term the
/// `2124·d + 2160` figure in `PARTIAL-PAYMENT-ECONOMICS.md` §1.2 omits.
///
/// ⚠️ Every `csvs` entry MUST come from the SIGNED transaction's `nSequence` (see
/// [`bind_declared_csv`]). Fed the conveyed serde field instead, this function computes whatever
/// requirement the sender wants it to.
pub fn exit_wait_blocks(csvs: &[Option<u16>]) -> u32 {
    csvs.iter().map(|c| c.unwrap_or(0) as u32 + 1).sum()
}

/// Sum of a chain's relative timelocks alone (no confirmations) — the reported `csv_total`.
pub fn exit_csv_total(csvs: &[Option<u16>]) -> u32 {
    csvs.iter().map(|c| c.unwrap_or(0) as u32).sum()
}

/// **[P0-1] THE ADMISSION GATE.** Refuse a conveyed bundle whose exit cannot complete before the
/// funding epoch expires.
///
/// `csvs` is the bundle's ACTUAL exit chain (so the requirement is derived from the bundle's real
/// depth and the live schedule, never a hard-coded constant); `epoch_expiry_height` is the lowest
/// locktime of the parent's validated flat backup chain. Returns the surviving slack in blocks.
///
/// **[B1] Every input must be RECEIVER-DERIVED.** `csvs` must be read off the signed transactions'
/// `nSequence` (never the conveyed `TesrTier::csv` field — see [`bind_declared_csv`]), `tip` from the
/// receiver's own chain backend, and `epoch_expiry_height` from a flat backup chain already validated
/// against the on-chain funding output. Fed a sender-declared term, this function computes exactly
/// the requirement the sender chose for it.
pub fn check_exit_headroom(
    csvs: &[Option<u16>],
    tip: u32,
    epoch_expiry_height: u32,
) -> Result<u32, ExitHeadroomShortfall> {
    let required = exit_wait_blocks(csvs);
    let available = epoch_expiry_height.saturating_sub(tip);
    if available < required {
        return Err(ExitHeadroomShortfall {
            tip,
            epoch_expiry_height,
            available,
            required,
            shortfall: required - available,
            tiers: csvs.len() as u32,
            csv_total: exit_csv_total(csvs),
        });
    }
    Ok(available - required)
}

/// **[D40.3] The minimum slack a conveyed exit walk must be admitted with.**
///
/// [`check_exit_headroom`] admits at `slack == 0`, and a test pins that case. But the slack it
/// returns is the piece's **entire tolerance** for confirmation variance, fee stall and reorg across
/// the whole walk — `exit_wait_blocks` charges each tier exactly one block for its parent's
/// confirmation, which its own doc calls *"the theoretical minimum, not a budget"*. Admitting at zero
/// means admitting a coin whose exit is feasible only if every one of up to 23 transactions confirms
/// in the very next block, forever.
///
/// Worse, **the sender chooses the slack**, by choosing when to convey. Conveying late in the epoch
/// is free for them and spends the payee's entire margin.
///
/// # Why a fraction of the walk, and not a constant
///
/// The margin has to scale with what it protects: a depth-1 walk is 3 transactions and a depth-10
/// walk is 23, so a flat number is either meaningless at the top or prohibitive at the bottom. And
/// it must be derived from terms the RECEIVER already holds. That is the whole reason this shape was
/// chosen over a stress fee rate — a `stress_fee_rate` would add a number nobody can derive to the
/// trust base, and at stress = 20 / depth 10 it puts the minimum admissible piece above 125,000 sat,
/// which kills the payment range it was meant to protect.
///
/// So: **one quarter of the walk's own required wait, and never less than one tier's worth.** Both
/// terms come from `csvs`, which the receiver derived from the conveyed chain and bound against its
/// own preset. Nothing exogenous enters.
///
/// The cost is a liveness cost — a late-in-epoch conveyance is refused and the sender must re-anchor
/// first — and it falls in the direction `ADMISSION-INPUTS.md` already names as the safe one.
pub fn exit_slack_margin(csvs: &[Option<u16>]) -> u32 {
    let required = exit_wait_blocks(csvs);
    let per_tier = if csvs.is_empty() { 0 } else { required / csvs.len() as u32 };
    (required / 4).max(per_tier)
}

/// [`check_exit_headroom`] with the [`exit_slack_margin`] applied — **the admission gate**.
///
/// Returns the slack REMAINING above the margin, so a caller that wants to report headroom still
/// gets a number rather than a boolean. The error is the same `ExitHeadroomShortfall`, with
/// `required` raised to include the margin, so an existing caller's message stays truthful about
/// what was needed.
pub fn check_exit_headroom_with_margin(
    csvs: &[Option<u16>],
    tip: u32,
    epoch_expiry_height: u32,
) -> Result<u32, ExitHeadroomShortfall> {
    let margin = exit_slack_margin(csvs);
    let required = exit_wait_blocks(csvs) + margin;
    let available = epoch_expiry_height.saturating_sub(tip);
    if available < required {
        return Err(ExitHeadroomShortfall {
            tip,
            epoch_expiry_height,
            available,
            required,
            shortfall: required - available,
            tiers: csvs.len() as u32,
            csv_total: exit_csv_total(csvs),
        });
    }
    Ok(available - required)
}

/// **[P0-2] THE DEPTH CAP**, derived rather than written down.
///
/// `base` is the exit chain of a depth-1 child (`T`, `X_m`, `SP`, `ext_child`, `state_child`) and
/// `per_level` is what ONE further split level adds (that level's extension and split state). Returns
/// the deepest child whose exit still fits in an `epoch_blocks`-long funding epoch — `0` when even a
/// depth-1 child does not fit, in which case no in-ladder split is admissible at all.
pub fn max_split_depth(base: &[Option<u16>], per_level: &[Option<u16>], epoch_blocks: u32) -> u32 {
    let base_wait = exit_wait_blocks(base);
    if base_wait > epoch_blocks {
        return 0;
    }
    let level_wait = exit_wait_blocks(per_level);
    if level_wait == 0 {
        // No level cost means no bound from this rule; the caller's other guards still apply.
        return u32::MAX;
    }
    1 + (epoch_blocks - base_wait) / level_wait
}

/// **[P0-3] THE EXIT-CHAIN LENGTH CAP** — the most transactions one unilateral exit may require.
///
/// # Why the latency rule is not enough
///
/// [`exit_wait_blocks`] charges a tier ONE block for its parent's confirmation. That is the
/// theoretical minimum, not a budget. A SPINE tier is signed at `SPINE_CSV = 0`, so it costs exactly
/// **one block** of latency while adding a whole transaction (~168 vB) to the walk. On the mainnet
/// schedule a depth-1 child's chain waits 2 885 blocks, which leaves 7 115 blocks of a
/// 10 000-block funding epoch — i.e. [`check_exit_headroom`] admits **7 115 further spine levels**,
/// a legal 7 120-transaction, ~1.2 MB exit chain that it reports as comfortably inside the epoch.
/// Every receiver of such a piece is credited a coin it cannot realistically materialise, and the
/// sender can mint them in one batch each. Ark's `bark` closed the same hole with a flat
/// `max_vtxo_exit_depth = 100`.
///
/// # The bound is derived, not chosen
///
/// The latency rule already blesses a longest chain: the deepest TWO-TIER child it admits, because a
/// two-tier level is the shape that pays full price in latency. Expressing that same rule in
/// transactions instead of blocks gives the cap:
///
/// ```text
/// max_exit_txs(base, per_level, epoch) = 3 + 2 · max_split_depth(base, per_level, epoch)
/// ```
///
/// where `3` is the parent segment (`T`, `X_m`, `SP`) and each further level costs its two tiers,
/// plus the leaf's own two — i.e. `tesr_exit_txs(d) = 3 + 2d`, the count the SDK's invalidation model
/// already uses.
///
/// * **Mainnet** (`lockheight_init = 10 000`, `E0 = 720`, `D0 = 1440`): `max_split_depth = 10`,
///   so the cap is **23 transactions**.
/// * **Regtest** (`lockheight_init = 1 000`, `E0 = 12`, `D0 = 24`): `max_split_depth = 68`,
///   so the cap is **139 transactions**.
///
/// By construction this refuses NOTHING the latency rule admits on a two-tier chain — the two bounds
/// meet exactly at depth 10 / 23 transactions on mainnet — and it is what turns a spine level from
/// free into priced.
///
/// # The worst-case weight it implies
///
/// At the mainnet cap of 23 tiers: all one-payload uncoloured tiers = 23 · 125 = **2 875 vB**; every
/// ancestor `SP` two-payload uncoloured = 23 · 168 = **3 864 vB**; all coloured two-payload tiers
/// (211 vB) = 23 · 211 = **4 853 vB**. Against the 1 196 160 vB a 7 120-tier chain costs today.
///
/// ⚠️ This is a bound on the CHAIN, not on any single transaction. An `SP`'s width is a free
/// parameter (`build_split_state_from` enforces no output-count limit), so the absolute ceiling is
/// `23 · TRUC_MAX_VBYTES = 230 000 vB` until a companion per-tier bound exists. See the module note
/// in `docs` and the residual-risk list: a v3/TRUC tier above 10 000 vB (≈ 230 payload outputs) is
/// non-standard, never relays, and its pre-signed exit is dead — that is a SEPARATE, still-open
/// finding, not one this cap closes.
///
/// # Floor
///
/// `max_split_depth` returns `0` when even a depth-1 child does not fit, so the cap floors at `3`,
/// which is exactly a minimal root ladder (`T`, `X_0`, `S_0`). No honest coin is ever refused for
/// being shorter than the shortest chain this protocol can build.
pub fn max_exit_txs(base: &[Option<u16>], per_level: &[Option<u16>], epoch_blocks: u32) -> u32 {
    let depth = max_split_depth(base, per_level, epoch_blocks);
    3u32.saturating_add(depth.saturating_mul(2))
}

/// A coin whose unilateral exit needs more TRANSACTIONS than [`max_exit_txs`] admits.
///
/// Distinct from [`ExitHeadroomShortfall`] on purpose: that one fires when the exit does not fit in
/// TIME, this one when it does not fit in WORK. The whole point of the cap is the gap between them —
/// a chain of spine tiers satisfies the latency rule and violates this one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "exit-chain length cap ({lane}): this coin's unilateral exit is {txs} transactions long, more \
     than the {max_txs} this schedule and its {epoch_blocks}-block funding epoch admit (the deepest \
     two-tier child that fits is depth {max_depth}, and a chain is 3 + 2·depth transactions). A \
     SPINE tier costs one block of exit latency but a whole transaction, so the exit-headroom rule \
     alone would pass a chain of thousands of them: it fits in the epoch and is still unusable. \
     Refused rather than admitted as a coin whose owner could never realistically broadcast it."
)]
pub struct ExitChainTooLong {
    /// Which lane refused (`"conveyed child"`, `"in-ladder split"`, `"conveyed root ladder"`, …).
    pub lane: String,
    /// Transactions in the exit chain that was measured.
    pub txs: u32,
    /// The cap ([`max_exit_txs`]).
    pub max_txs: u32,
    /// The deepest two-tier child the epoch admits — the depth the cap is derived from.
    pub max_depth: u32,
    /// The funding epoch length (`lockheight_init`).
    pub epoch_blocks: u32,
}

/// **[P0-3] THE LENGTH GATE.** Refuse an exit chain longer than [`max_exit_txs`]; returns the
/// surviving slack in transactions.
///
/// **Strictly greater, never `>=`.** The mainnet-max latency-bound two-tier child is EXACTLY 23
/// transactions and is honest — `max_split_depth` admits it and [`check_exit_headroom`] admits it
/// (9 383 ≤ 10 000). `>=` would refuse the single deepest shape the builders legitimately produce.
/// Same boundary discipline [`check_exit_headroom`] already keeps: a bundle landing exactly on the
/// limit is admissible.
///
/// **[B1] Every input must be RECEIVER-DERIVED.** `epoch_blocks` is the SE's `lockheight_init` read
/// from `/info/config`; `base`/`per_level` come from the RECEIVER's own network preset
/// (`TesrParams::for_network`), NEVER from the conveyed `bundle.params` — a schedule the sender
/// declares would let it inflate its own cap, which is the C-1 shape (authority derived from the
/// artifact being checked) this file has already been burned by.
///
/// ⚠️ **This is a COUNT, not a value.** It reads no transaction output and no `LinkedPayload`, so it
/// is outside the check-ordering defect class and belongs BEFORE the structural pass, not after: a
/// chain cannot be shortened by omitting a segment (every tier is linked to its parent's outpoint),
/// so the count is trustworthy pre-binding, and refusing early is what keeps the DoS margin.
///
/// ⚠️ **NEVER call this from the exit path.** It is an ADMISSION and BUILD rule only. A coin that is
/// already owned must always be exitable; a length refusal reachable from `exit_pass`,
/// `exit_child_pass`, `child_exit_chain*`, `spine_tip_exit_chain*`, `defend_ladders` or
/// `unilateral_exit` would strand a real coin permanently — strictly worse than the hole it closes.
pub fn check_exit_chain_length(
    lane: &str,
    txs: u32,
    base: &[Option<u16>],
    per_level: &[Option<u16>],
    epoch_blocks: u32,
) -> Result<u32, ExitChainTooLong> {
    let max_txs = max_exit_txs(base, per_level, epoch_blocks);
    if txs > max_txs {
        return Err(ExitChainTooLong {
            lane: lane.to_string(),
            txs,
            max_txs,
            max_depth: max_split_depth(base, per_level, epoch_blocks),
            epoch_blocks,
        });
    }
    Ok(max_txs.saturating_sub(txs))
}

pub fn reconstruct_transaction(tx_n_hex: &str) -> Result<(), MercuryError> {

    let tx_n: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx_n_hex)?)?;

    // this assumes that the transaction has only one input (supposedly checked before). It may have
    // one or two outputs: the recipient/owner output and, for RGB-enabled coins, a zero-value
    // OP_RETURN opret commitment. All outputs are kept, in order, to verify canonical serialization.
    let output = tx_n.output.clone();
    let input = tx_n.input[0].clone();
    let locktime = tx_n.lock_time;

    let new_tx = Transaction {
        version: 2,
        lock_time: locktime,
        input: [input].to_vec(),
        output,
    };

    let serialized_new_tx = hex::encode(bitcoin::consensus::encode::serialize(&new_tx));

    if tx_n_hex != serialized_new_tx {
        return Err(MercuryError::TransactionReconstructionError);
    }

    Ok(())
}

fn get_tx_hash(tx_0: &Transaction, tx_n: &Transaction) -> Result<Message, MercuryError> {

    let witness = tx_n.input[0].witness.clone();

    if witness.nth(0).is_none() {
        return Err(MercuryError::EmptyWitness);
    }

    let witness_data = witness.nth(0).unwrap();

    let vout = tx_n.input[0].previous_output.vout as usize;

    let tx_0_output = tx_0.output[vout].clone();

    if witness_data.last().is_none() {
        return Err(MercuryError::EmptyWitnessData);
    }

    // let sighash_type = TapSighashType::from_consensus_u8(witness_data.last().unwrap().to_owned())?;
    let sighash_type = TapSighashType::All;

    let hash = SighashCache::new(tx_n).taproot_key_spend_signature_hash(
        0,
        &sighash::Prevouts::All(&[TxOut {
            value: tx_0_output.value,
            script_pubkey: tx_0_output.script_pubkey.clone(),
        }]),
        sighash_type,
    )?;

    let msg: Message = hash.into();

    Ok(msg)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn verify_blinded_musig_scheme(backup_tx: &BackupTx, tx0_hex: &str, statechain_info: &StatechainInfo) -> Result<(), MercuryError> {

    let client_public_nonce = MusigPubNonce::from_slice(hex::decode(&backup_tx.client_public_nonce)?.as_slice())?;

    let server_public_nonce = MusigPubNonce::from_slice(hex::decode(&backup_tx.server_public_nonce)?.as_slice())?;

    let client_public_key = PublicKey::from_str(&backup_tx.client_public_key)?;

    let server_public_key = PublicKey::from_str(&backup_tx.server_public_key)?;

    let blinding_factor = BlindingFactor::from_slice(hex::decode(&backup_tx.blinding_factor)?.as_slice())?;

    let secp = Secp256k1::new();

    let aggregate_pubkey = client_public_key.combine(&server_public_key)?;

    let tap_tweak = TapTweakHash::from_key_and_tweak(aggregate_pubkey.x_only_public_key().0, None);
    let tap_tweak_bytes = tap_tweak.as_byte_array();

    let tweak = SecretKey::from_slice(tap_tweak_bytes)?;

    let (_, output_pubkey, out_tweak32) = blinded_musig_pubkey_xonly_tweak_add(&secp, &aggregate_pubkey, tweak);
    
    let aggnonce = MusigAggNonce::new(&secp, &[client_public_nonce, server_public_nonce]);

    let tx_0: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx0_hex)?)?;

    let tx_n: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&backup_tx.tx)?)?;

    let msg = get_tx_hash(&tx_0, &tx_n)?;

    let session = MusigSession::new_blinded_without_key_agg_cache(
        &secp,
        &output_pubkey,
        aggnonce,
        msg,
        None,
        &blinding_factor,
        out_tweak32
    );
    // END repeated code

    let challenge = session.get_challenge_from_session();
    let challenge = hex::encode(challenge);

    if statechain_info.challenge != challenge {
        return Err(MercuryError::IncorrectChallenge);
    }

    Ok(())
}

fn validate_t1pub(t1: &[u8; 32], x1_pub: &PublicKey, sender_public_key: &PublicKey) -> Result<bool, MercuryError> {

    let secret_t1 = SecretKey::from_slice(t1)?;
    let public_t1 = secret_t1.public_key(&Secp256k1::new());

    let result_pubkey = sender_public_key.combine(&x1_pub)?;

    Ok(result_pubkey == public_t1)
}

fn calculate_t2(transfer_msg: &TransferMsg, client_seckey_share: &SecretKey,) -> Result<SecretKey, MercuryError> {

    let t1 = Scalar::from_be_bytes(transfer_msg.t1)?;

    let negated_seckey = client_seckey_share.negate();

    let t2 = negated_seckey.add_tweak(&t1)?;

    Ok(t2)
}

pub fn create_transfer_receiver_request_payload(statechain_info: &StatechainInfoResponsePayload, transfer_msg: &TransferMsg, coin: &Coin) -> Result<TransferReceiverRequestPayload, MercuryError> {

    if statechain_info.x1_pub.is_none() {
        return Err(MercuryError::NoX1Pub);
    }

    let x1_pub = statechain_info.x1_pub.as_ref().unwrap();

    let x1_pub = PublicKey::from_str(x1_pub)?;

    let sender_public_key = PublicKey::from_str(&transfer_msg.user_public_key)?;

    let client_seckey_share = PrivateKey::from_wif(&coin.user_privkey)?.inner;

    let client_auth_key = PrivateKey::from_wif(&coin.auth_privkey)?.inner;

    if !validate_t1pub(&transfer_msg.t1, &x1_pub, &sender_public_key)? {
        return Err(MercuryError::InvalidT1);
    }

    let t2 = calculate_t2(&transfer_msg, &client_seckey_share)?;

    let t2_hex = hex::encode(t2.secret_bytes());

    let secp = Secp256k1::new();

    let client_auth_keypair = KeyPair::from_seckey_slice(&secp, client_auth_key.as_ref())?;
    let msg = Message::from_hashed_data::<sha256::Hash>(t2_hex.as_bytes());
    let auth_sig = secp.sign_schnorr(&msg, &client_auth_keypair);

    let transfer_receiver_request_payload = TransferReceiverRequestPayload {
        statechain_id: transfer_msg.statechain_id.clone(),
        batch_data: None,
        t2: t2_hex,
        auth_sig: auth_sig.to_string(),
    };

    Ok(transfer_receiver_request_payload)

}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn sign_message(message: &str, coin: &Coin) -> Result<String, MercuryError> {

    let client_auth_key = PrivateKey::from_wif(&coin.auth_privkey)?.inner;

    let secp = Secp256k1::new();

    let client_auth_keypair = KeyPair::from_seckey_slice(&secp, client_auth_key.as_ref())?;
    let hashed_msg = Message::from_hashed_data::<sha256::Hash>(message.to_string().as_bytes());
    let signed_message = secp.sign_schnorr(&hashed_msg, &client_auth_keypair);

    Ok(signed_message.to_string())
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn get_new_key_info(server_public_key_hex: &str, coin: &Coin, statechain_id: &str, tx0_outpoint: &TxOutpoint, tx0_hex: &str, network: &str) -> Result<NewKeyInfo, MercuryError> {
    
    let network = get_network(&network)?;

    let client_auth_key = PrivateKey::from_wif(&coin.auth_privkey)?.inner;

    let client_pubkey_share = PublicKey::from_str(&coin.user_pubkey)?;

    let server_pubkey_share = PublicKey::from_str(server_public_key_hex)?;

    let aggregate_pubkey = client_pubkey_share.combine(&server_pubkey_share)?;

    let aggregated_xonly_pubkey = aggregate_pubkey.x_only_public_key().0;

    let secp = Secp256k1::new();

    let aggregate_address = Address::p2tr(&secp, aggregated_xonly_pubkey, None, network);

    let xonly_pubkey = XOnlyPublicKey::from_slice(aggregate_address.script_pubkey()[2..].as_bytes()).unwrap();

    let tx0: Transaction = bitcoin::consensus::encode::deserialize(&hex::decode(&tx0_hex)?)?;

    let tx0_output = tx0.output[tx0_outpoint.vout as usize].clone();

    let tx0_output_xonly_pubkey = XOnlyPublicKey::from_slice(tx0_output.script_pubkey[2..].as_bytes()).unwrap();

    if tx0_output_xonly_pubkey != xonly_pubkey {
        return Err(MercuryError::IncorrectAggregatedPublicKey);
    }

    let p2tr_agg_address = Address::p2tr(&secp, aggregated_xonly_pubkey, None, network);

    let client_auth_keypair = KeyPair::from_seckey_slice(&secp, client_auth_key.as_ref())?;
    let msg = Message::from_hashed_data::<sha256::Hash>(statechain_id.to_string().as_bytes());
    let signed_statechain_id = secp.sign_schnorr(&msg, &client_auth_keypair);

    Ok(NewKeyInfo {
        aggregate_pubkey: aggregate_pubkey.to_string(),
        aggregate_address: p2tr_agg_address.to_string(),
        signed_statechain_id: signed_statechain_id.to_string(),
        amount: tx0_output.value as u32,
    })
}
#[cfg(test)]
mod transfer_signature_tests {
    //! Receiver check R1 (TRUST-MODEL §2): the receiver verifies the SENDER's Schnorr signature
    //! binding the coin's outpoint to the RECEIVER's new pubkey, so a handover message not
    //! authorized by the coin's owner — or replayed to bind a different receiver — is rejected.
    //! This exercises the REJECT paths (the positive path runs on every successful claim E2E).
    use super::*;
    use secp256k1_zkp::KeyPair;

    fn msg_from(sender: &KeyPair, sig: &Signature) -> TransferMsg {
        TransferMsg {
            statechain_id: "sc".to_string(),
            transfer_signature: sig.to_string(),
            backup_transactions: vec![],
            t1: [0u8; 32],
            user_public_key: PublicKey::from_keypair(sender).to_string(),
            branch_txs: vec![],
            terminal_parents: vec![],
            protocol_version: 0,
            tesr_ladder: None,
            child_tesr_bundle: None,
        }
    }

    fn sign_over(secp: &Secp256k1<secp256k1_zkp::All>, sender: &KeyPair, txid: &str, vout: u32, bound_pk: &PublicKey) -> Signature {
        let input_txid = Txid::from_str(txid).unwrap();
        let mut data = Vec::<u8>::new();
        data.extend_from_slice(&input_txid[..]);
        data.extend_from_slice(&vout.to_le_bytes());
        data.extend_from_slice(&bound_pk.serialize()[..]);
        let m = Message::from_hashed_data::<sha256::Hash>(&data);
        secp.sign_schnorr(&m, sender)
    }

    /// **[B.8 / D41] THE TRUNCATION GAP, EXECUTED RATHER THAN ARGUED.**
    ///
    /// `ladder_decrements_by_interval` is a PAIRWISE rule, and this is what that costs: **every
    /// suffix of an honest ladder satisfies it exactly as well as the whole ladder does.** Nothing
    /// in the rule relates a chain to where it began, so dropping rungs off the HEAD is invisible
    /// to it.
    ///
    /// That is the whole of B.8. A sender truncates the head, buys one extra co-signature per
    /// dropped rung, discloses those in `superseded_states`, and the receiver's exact-equality
    /// census (`se_num_sigs == flat_backups + tiers + superseded`) still balances — while the
    /// receiver reads the short chain as a coin with few prior owners.
    ///
    /// The other checks in [`validate_backup_chain_v2`] do not close it either, and the reason is
    /// worth stating because it is counter-intuitive:
    /// [`verify_if_locktime_is_reasonable_tx_version_and_output_size`] bounds each locktime ABOVE
    /// (`<= tip + lockheight_init`). Truncating the head moves the chain's maximum DOWN. An upper
    /// bound cannot detect a value moving down.
    ///
    /// This test asserts the gap so that it is a MEASURED property rather than a claim in a
    /// decision record — and so that whoever closes it has something that goes green.
    #[test]
    fn every_suffix_of_an_honest_ladder_passes_the_pairwise_rule() {
        use super::ladder_decrements_by_interval as ok;
        const INTERVAL: u32 = 100;
        // An honest five-rung chain: L_0 = 10_000 down to L_4 = 9_600.
        let honest: Vec<u32> = (0..5).map(|k| 10_000 - k * INTERVAL).collect();
        assert_eq!(honest, vec![10_000, 9_900, 9_800, 9_700, 9_600]);

        let pairwise_ok = |chain: &[u32]| chain.windows(2).all(|w| ok(w[0], w[1], INTERVAL));
        assert!(pairwise_ok(&honest), "the honest chain must pass — otherwise this proves nothing");

        // EVERY suffix passes too. Each one is the same coin with prior owners hidden.
        for drop in 1..honest.len() {
            let truncated = &honest[drop..];
            assert!(
                pairwise_ok(truncated),
                "a chain truncated by {drop} rung(s) is accepted by the pairwise rule: {truncated:?}"
            );
        }

        // …and the shortest truncation is a ONE-element chain, which the rule accepts vacuously —
        // the shape a receiver reads as "structurally B1-immune".
        assert!(
            pairwise_ok(&honest[4..]),
            "a one-element chain has no adjacent pair, so the pairwise rule is vacuously satisfied"
        );

        // THE FIX, stated as the arithmetic it needs: an anchored head distinguishes them, and
        // nothing available to a receiver today supplies `L_0`. `max(chain)` is the quantity an
        // anchor would pin.
        let l0_honest = *honest.iter().max().unwrap();
        for drop in 1..honest.len() {
            let l0_seen = *honest[drop..].iter().max().unwrap();
            assert!(
                l0_seen < l0_honest,
                "truncation always LOWERS the observable head ({l0_seen} < {l0_honest}), which is \
                 exactly why an upper bound on locktimes cannot catch it"
            );
        }
    }

    // Receiver check R5 (TRUST-MODEL §2): the backup ladder must decrement by EXACTLY the SE's
    // interval per hop — a wrong gap (hidden intermediate owner / forged spacing) or a
    // non-decreasing ladder is rejected, the latter without underflow-panicking.
    #[test]
    fn ladder_interval_check_rejects_wrong_and_increasing() {
        use super::ladder_decrements_by_interval as ok;
        // Exact decrement of `interval` → accepted (deployed 10, defaults 100).
        assert!(ok(1000, 990, 10));
        assert!(ok(10_000, 9_900, 100));
        // Wrong gap → rejected.
        assert!(!ok(1000, 991, 10), "9-block gap must be rejected when interval is 10");
        assert!(!ok(1000, 980, 10), "20-block gap must be rejected when interval is 10");
        // Equal locktimes (no decrement) → rejected.
        assert!(!ok(1000, 1000, 10));
        // Increasing ladder (a forged backup maturing LATER than its parent) → rejected, no panic.
        assert!(!ok(990, 1000, 10), "an increasing ladder must be rejected cleanly (no u32 underflow panic)");
    }

    // ---------------------------------------------------------------------------------------
    // [P0-1 / P0-2] Exit-headroom admission. The numbers below are re-derived from the mainnet
    // schedule in `docs/utexo/PROTOCOL.md` §5.2 (D0 1440, δ 36, d_floor 144, E0 720, δE 36,
    // e_floor 144) and `lockheight_init = 10 000` (`server/src/server_config.rs:82`) — they are
    // asserted here so a schedule change that silently breaks the invariant is caught.
    // ---------------------------------------------------------------------------------------

    /// The **PRE-CATS** exit chain of a depth-`d` in-ladder split child on the mainnet schedule:
    /// `T (none) | X_m 720 | SP 1404 | (d−1)×(720, 1404) | ext_child 720 | state_child 1440`.
    ///
    /// ⚠️ **THIS IS NOT THE SHAPE THE BUILDERS SIGN TODAY, and the label is load-bearing.** The live
    /// builders (`in_ladder_split`, `child_in_ladder_split`, `spine_batch_split`) sign `SP` at
    /// `SPINE_CSV = 0`, not at a state rung — so the CURRENT depth-1 mainnet chain is
    /// `[None, 720, 0, 720, 1440]`, 2 880 blocks of timelock rather than 4 284. `sdk88` measures
    /// exactly that shape live on regtest (`[None, 12, 0, 12, 24]`).
    ///
    /// It is retained DELIBERATELY [D31], for two reasons:
    ///   * it is the "before" half of the labelled before/after in
    ///     `docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md` §1.2, and deleting it makes the multi-year exit
    ///     read as current;
    ///   * every test below that uses it is a MARGIN test, and `3 + 2d ≥ d + 4` — over-counting a
    ///     margin makes a watchtower act EARLIER, which is the safe direction. A margin fixture is
    ///     the one place a conservative shape is correct.
    ///
    /// **Do not read a live figure off this function.** Economics and any published WAIT(d) must use
    /// `tesr_exit_wait_blocks` (`clients/libs/rust-sdk/src/config.rs`), which is shape-aware and
    /// yields `720·d + 2160 + (3 + 2d)` — the source of the published depth-10 figure of 9 383
    /// blocks. This comment previously described this fixture as "the exit chain … on the mainnet
    /// schedule" full stop, which is how a spec author would have published 2124·d as current.
    fn mainnet_chain(d: u32) -> Vec<Option<u16>> {
        let mut v = vec![None, Some(720), Some(1404)];
        for _ in 1..d {
            v.push(Some(720));
            v.push(Some(1404));
        }
        v.push(Some(720));
        v.push(Some(1440));
        v
    }

    /// Pins the PRE-CATS baseline, not a live figure — see [`mainnet_chain`]. The live shape is
    /// `720·d + 2160` (`tesr_exit_wait_blocks`); this asserts the "before" half of the labelled
    /// comparison in `PARTIAL-PAYMENT-ECONOMICS.md` §1.2 so that half cannot drift either.
    #[test]
    fn exit_wait_matches_the_pre_cats_baseline() {
        // §1.2 baseline: WAIT(d) = 2124·d + 2160 blocks of timelock, over 3 + 2d transactions.
        for d in 1..=5u32 {
            let chain = mainnet_chain(d);
            assert_eq!(chain.len() as u32, 3 + 2 * d, "d={d}: 3 + 2d exit transactions");
            assert_eq!(
                super::exit_csv_total(&chain),
                2124 * d + 2160,
                "d={d}: the pre-CATS baseline WAIT(d) timelock total"
            );
            // Plus one confirmation per transaction — a CSV cannot start before its parent confirms.
            assert_eq!(super::exit_wait_blocks(&chain), 2124 * d + 2160 + (3 + 2 * d));
        }
    }

    /// **[B1]** the declared timelock is bound to the signed one, and the requirement is computed
    /// from the SIGNED chain — the whole bypass in one assertion.
    #[test]
    fn declared_csv_must_match_the_signed_nsequence() {
        // Agreement passes through and yields the signed value.
        assert_eq!(super::bind_declared_csv(3, "child extension", Some(2124), Some(2124)), Ok(Some(2124)));
        assert_eq!(super::bind_declared_csv(0, "trigger", None, None), Ok(None));

        // The attack: every tier declares a 1-block lock while the signatures say otherwise.
        let err = super::bind_declared_csv(4, "child state", Some(1), Some(2160)).unwrap_err();
        assert_eq!(err.index, 4);
        assert_eq!(err.tier, "child state");
        let msg = err.to_string();
        assert!(msg.contains("declared-CSV mismatch"), "{msg}");
        assert!(msg.contains("1 block(s)") && msg.contains("2160 block(s)"), "both values named: {msg}");

        // `None` and `Some(0)` cost the same number of blocks but are different claims, so they must
        // not be quietly treated as equal — a tier whose relative lock is DISABLED is not a tier with
        // a zero-block lock.
        assert!(super::bind_declared_csv(1, "parent level 0 extension", Some(0), None).is_err());
        assert!(super::bind_declared_csv(1, "parent level 0 extension", None, Some(0)).is_err());

        // And the point of it all: had the declared chain been used, the doomed coin would have been
        // admitted with 4 289 blocks of exit needing only 10 of headroom.
        let declared: Vec<Option<u16>> = vec![None, Some(1), Some(1), Some(1), Some(1)];
        assert_eq!(super::exit_wait_blocks(&declared), 9);
        assert!(super::check_exit_headroom(&declared, 100_000, 100_010).is_ok());
        assert!(super::check_exit_headroom(&mainnet_chain(1), 100_000, 100_010).is_err());
    }

    /// **[D40.3] The minimum-slack margin, and the case it exists to refuse.**
    ///
    /// The bare gate admits at `slack == 0` — a coin whose exit is feasible only if every one of its
    /// transactions confirms in the very next block, across a walk of up to 23 of them. And the
    /// SENDER picks that slack, by picking when to convey.
    #[test]
    fn the_slack_margin_refuses_a_zero_tolerance_conveyance() {
        let chain = mainnet_chain(1);
        let required = super::exit_wait_blocks(&chain);
        let margin = super::exit_slack_margin(&chain);

        // Derived from the walk, never a constant: a quarter of its own wait, floored at one tier's.
        assert_eq!(margin, required / 4, "the margin is a quarter of the walk's own required wait");
        assert!(margin > 0, "a walk with a real wait must carry a real margin");

        // THE CASE. The bare gate admits exactly at the boundary — that is the defect, and it is
        // pinned by the test below this one, so the two must disagree here or the margin does nothing.
        assert_eq!(super::check_exit_headroom(&chain, 100_000, 100_000 + required), Ok(0));
        assert!(
            super::check_exit_headroom_with_margin(&chain, 100_000, 100_000 + required).is_err(),
            "a zero-slack conveyance must now be REFUSED — this is the whole point of D40.3"
        );

        // …and the refusal states what was actually needed, margin included, so the message stays
        // truthful rather than reporting the bare wait and looking like an off-by-one.
        let err = super::check_exit_headroom_with_margin(&chain, 100_000, 100_000 + required)
            .expect_err("refused");
        assert_eq!(err.required, required + margin);
        assert_eq!(err.shortfall, margin);

        // Exactly at the raised bar: admitted. Never over-refuse.
        assert_eq!(
            super::check_exit_headroom_with_margin(&chain, 100_000, 100_000 + required + margin),
            Ok(0)
        );
        // A fresh epoch is unaffected — the margin costs a late conveyance, not an ordinary one.
        assert!(
            super::check_exit_headroom_with_margin(&chain, 100_000, 100_000 + 10_000).is_ok(),
            "a full mainnet epoch must still admit a depth-1 child"
        );
    }

    /// The margin scales with the walk, which is why it is a fraction rather than a constant: a
    /// depth-1 walk is 3 transactions and a depth-10 walk is 23. A flat number is either meaningless
    /// at the top or prohibitive at the bottom.
    #[test]
    fn the_slack_margin_scales_with_depth() {
        let d1 = super::exit_slack_margin(&mainnet_chain(1));
        let d3 = super::exit_slack_margin(&mainnet_chain(3));
        assert!(d3 > d1, "a deeper walk must carry a larger margin ({d1} vs {d3})");
        // Degenerate input must not panic or divide by zero.
        assert_eq!(super::exit_slack_margin(&[]), 0);
    }

    #[test]
    fn headroom_gate_refuses_a_child_that_cannot_be_materialised_in_time() {
        let chain = mainnet_chain(1);
        let required = super::exit_wait_blocks(&chain); // 4 289
        assert_eq!(required, 4_289);

        // Fresh epoch: a depth-1 child fits with room to spare.
        let slack = super::check_exit_headroom(&chain, 100_000, 100_000 + 10_000)
            .expect("a full epoch admits a depth-1 child");
        assert_eq!(slack, 10_000 - required);

        // Exactly enough: admitted at the boundary (never weaken, never over-refuse).
        assert_eq!(
            super::check_exit_headroom(&chain, 100_000, 100_000 + required),
            Ok(0),
            "a bundle whose exit lands exactly on the expiry height is admissible"
        );

        // One block short: refused, and the refusal states the shortfall.
        let err = super::check_exit_headroom(&chain, 100_000, 100_000 + required - 1)
            .expect_err("one block short of the exit wait must be refused");
        assert_eq!(err.shortfall, 1);
        assert_eq!(err.required, required);
        assert_eq!(err.available, required - 1);
        assert_eq!(err.tiers, 5);
        assert_eq!(err.csv_total, 4_284);
        assert!(err.to_string().contains("short by 1 blocks"), "{err}");

        // THE EXPLOIT (P0-1): the old gate was `lock_time > tip`, which admits everything below.
        // The last 4 289 blocks of a 10 000-block epoch are 42.9% of it.
        let epoch = 10_000u32;
        let bad_window = required;
        assert!(
            (bad_window as f64) / (epoch as f64) > 0.42,
            "the window the missing gate left open is ~43% of every epoch"
        );
        for age in (epoch - bad_window + 1)..epoch {
            let tip = 100_000 + age;
            assert!(
                super::check_exit_headroom(&chain, tip, 100_000 + epoch).is_err(),
                "a coin conveyed {age} blocks into the epoch must be refused"
            );
        }

        // An already-expired epoch is refused, not underflowed.
        assert!(super::check_exit_headroom(&chain, 100_001, 100_000).is_err());
    }

    #[test]
    fn deeper_children_need_proportionally_more_headroom() {
        // The contagion: each further split level costs 2 126 more blocks of headroom.
        let d1 = super::exit_wait_blocks(&mainnet_chain(1));
        let d2 = super::exit_wait_blocks(&mainnet_chain(2));
        assert_eq!(d2 - d1, 2_126);
        // A window that admits a depth-1 child refuses a depth-2 one.
        assert!(super::check_exit_headroom(&mainnet_chain(1), 0, d1).is_ok());
        assert!(super::check_exit_headroom(&mainnet_chain(2), 0, d1).is_err());
    }

    #[test]
    fn depth_cap_is_derived_from_the_schedule_and_the_epoch() {
        let base = mainnet_chain(1);
        let per_level = vec![Some(720u16), Some(1404)];
        // Mainnet, lockheight_init = 10 000: depth 3 fits (10 541), depth 4 does not (12 667).
        let cap = super::max_split_depth(&base, &per_level, 10_000);
        assert_eq!(cap, 3, "mainnet admits at most a depth-3 child");
        assert!(super::exit_wait_blocks(&mainnet_chain(cap)) <= 10_000);
        assert!(
            super::exit_wait_blocks(&mainnet_chain(cap + 1)) > 10_000,
            "the cap must be the LAST depth that fits"
        );
        // The task's figure: WAIT(4) = 10 656 > 10 000 even before confirmations.
        assert_eq!(super::exit_csv_total(&mainnet_chain(4)), 10_656);

        // Regtest schedule (d0 24, δ 6, e0 12) with the deployed 1 000-block epoch.
        let rt_base = vec![None, Some(12u16), Some(18), Some(12), Some(24)];
        let rt_level = vec![Some(12u16), Some(18)];
        assert_eq!(super::exit_wait_blocks(&rt_base), 66 + 5);
        assert_eq!(super::max_split_depth(&rt_base, &rt_level, 1_000), 30);

        // An epoch too short for even a depth-1 child admits NOTHING (fail closed).
        assert_eq!(super::max_split_depth(&base, &per_level, 4_288), 0);
        assert_eq!(super::max_split_depth(&base, &per_level, 4_289), 1);
    }

    #[test]
    fn accepts_valid_rejects_forged_transfer_signature() {
        let secp = Secp256k1::new();
        let mut rng = secp256k1_zkp::rand::thread_rng();
        let sender = KeyPair::new(&secp, &mut rng);
        let receiver_pk = PublicKey::from_secret_key(&secp, &SecretKey::new(&mut rng));
        let txid = "1111111111111111111111111111111111111111111111111111111111111111";
        let vout = 1u32;
        let outpoint = TxOutpoint { txid: txid.to_string(), vout };

        // Valid: signed by the sender over (txid || vout || receiver_pk) → accepted.
        let good = sign_over(&secp, &sender, txid, vout, &receiver_pk);
        let m = msg_from(&sender, &good);
        assert!(verify_transfer_signature(&receiver_pk.to_string(), &outpoint, &m).unwrap(),
            "a correctly-bound sender signature must verify");

        // Reject: replay to a DIFFERENT receiver (the signature no longer binds the new pubkey).
        let other_pk = PublicKey::from_secret_key(&secp, &SecretKey::new(&mut rng));
        assert!(!verify_transfer_signature(&other_pk.to_string(), &outpoint, &m).unwrap(),
            "a signature bound to receiver A must not authorize a handover to receiver B");

        // Reject: same signature against a DIFFERENT outpoint (vout tampered).
        let outpoint2 = TxOutpoint { txid: txid.to_string(), vout: 2 };
        assert!(!verify_transfer_signature(&receiver_pk.to_string(), &outpoint2, &m).unwrap(),
            "a signature over outpoint vout=1 must not authorize spending vout=2");

        // Reject: forged by an ATTACKER key over the correct data, but the message claims the
        // honest sender's pubkey (a malicious sender substituting a signature they don't own).
        let attacker = KeyPair::new(&secp, &mut rng);
        let forged = sign_over(&secp, &attacker, txid, vout, &receiver_pk);
        let m_forged = msg_from(&sender, &forged); // user_public_key = honest sender, sig by attacker
        assert!(!verify_transfer_signature(&receiver_pk.to_string(), &outpoint, &m_forged).unwrap(),
            "a signature not produced by the claimed sender key must be rejected");
    }
}

/// [D8] The attestation that closes the unauthenticated-census hole. These pin the property the
/// whole change exists for: an under-reported count must not be believable, and the coordinator
/// must not be able to sign one itself.
#[cfg(test)]
mod sig_count_attestation_tests {
    use super::*;
    use secp256k1_zkp::{rand, KeyPair, Secp256k1};

    /// Build the preimage exactly as `lockbox/src/server.cpp` does, and sign it.
    fn attest(kp: &KeyPair, sid: &str, count: u32, budget: Option<u32>, nonce: &[u8; 32]) -> String {
        let mut preimage: Vec<u8> = Vec::new();
        preimage.extend_from_slice(b"utexo/sig_count/v2");
        preimage.extend_from_slice(sid.as_bytes());
        preimage.extend_from_slice(&count.to_be_bytes());
        preimage.push(u8::from(budget.is_some()));
        preimage.extend_from_slice(&budget.unwrap_or(0).to_be_bytes());
        preimage.extend_from_slice(nonce);
        let msg = Message::from_hashed_data::<sha256::Hash>(&preimage);
        Secp256k1::new().sign_schnorr(&msg, kp).to_string()
    }

    fn setup() -> (KeyPair, String, String, [u8; 32]) {
        let secp = Secp256k1::new();
        let kp = KeyPair::new(&secp, &mut rand::thread_rng());
        let compressed = PublicKey::from_keypair(&kp).to_string();
        let (xonly, _) = PublicKey::from_keypair(&kp).x_only_public_key();
        (kp, compressed, xonly.to_string(), [7u8; 32])
    }

    #[test]
    fn an_honest_attestation_verifies() {
        let (kp, compressed, xonly, nonce) = setup();
        let sig = attest(&kp, "coin-a", 5, None, &nonce);
        assert!(verify_sig_count_attestation(
            "coin-a", 5, None, &hex::encode(nonce), &sig, &xonly, &compressed).is_ok());
    }

    #[test]
    fn an_under_reported_count_does_not_verify() {
        // THE ATTACK. The SE attested 5; the coordinator forwards 4 to hide one co-signed rival
        // state, so the receiver's exact-equality census balances against a short tally. The
        // signature was made over 5, so it cannot cover 4.
        let (kp, compressed, xonly, nonce) = setup();
        let sig = attest(&kp, "coin-a", 5, None, &nonce);
        assert!(verify_sig_count_attestation(
            "coin-a", 4, None, &hex::encode(nonce), &sig, &xonly, &compressed).is_err(),
            "an under-reported count must NOT verify — this is the theft the census exists to stop");
    }

    #[test]
    fn a_coordinator_signing_with_its_own_key_is_refused() {
        // The coordinator serves a perfectly valid signature — made with a key IT controls — and
        // names that key in `attestation_pubkey`. Only binding to the chain-anchored enclave key
        // catches this; verifying against the served key would accept it.
        let (_kp, compressed, _xonly, nonce) = setup();
        let secp = Secp256k1::new();
        let rogue = KeyPair::new(&secp, &mut rand::thread_rng());
        let (rogue_xonly, _) = PublicKey::from_keypair(&rogue).x_only_public_key();
        let sig = attest(&rogue, "coin-a", 4, None, &nonce);
        assert!(verify_sig_count_attestation(
            "coin-a", 4, None, &hex::encode(nonce), &sig, &rogue_xonly.to_string(), &compressed).is_err(),
            "a signature by a key other than the chain-anchored enclave key must be refused");
    }

    #[test]
    fn a_replayed_attestation_for_another_nonce_is_refused() {
        // Without nonce binding the attestation is static: a coordinator could serve one captured
        // when the count was legitimately lower, forever.
        let (kp, compressed, xonly, nonce) = setup();
        let sig = attest(&kp, "coin-a", 5, None, &nonce);
        let fresh = [9u8; 32];
        assert!(verify_sig_count_attestation(
            "coin-a", 5, None, &hex::encode(fresh), &sig, &xonly, &compressed).is_err(),
            "an attestation answering a different challenge must not satisfy this one");
    }

    #[test]
    fn an_attestation_for_another_coin_is_refused() {
        let (kp, compressed, xonly, nonce) = setup();
        let sig = attest(&kp, "coin-a", 5, None, &nonce);
        assert!(verify_sig_count_attestation(
            "coin-b", 5, None, &hex::encode(nonce), &sig, &xonly, &compressed).is_err(),
            "the statechain id is in the preimage precisely so one coin's count cannot vouch for another");
    }

    #[test]
    fn the_lockbox_wire_format_with_its_0x_prefix_verifies() {
        // REGRESSION. The lockbox serialises every key through `key_to_string`, which prefixes
        // "0x" (lockbox/src/utils.cpp:56-62). The other tests here build their hex with
        // `.to_string()`, which does NOT, so they all passed while the real endpoint's output would
        // have failed to parse. Caught by calling the live /signature_count, not by unit testing.
        let (kp, compressed, xonly, nonce) = setup();
        let sig = attest(&kp, "coin-a", 5, None, &nonce);
        assert!(verify_sig_count_attestation(
            "coin-a", 5, None, &hex::encode(nonce),
            &format!("0x{sig}"), &format!("0x{xonly}"), &format!("0x{compressed}")).is_ok(),
            "the SE's actual on-the-wire encoding must verify");
        // And the bare form must keep working, so both encodings are accepted.
        assert!(verify_sig_count_attestation(
            "coin-a", 5, None, &hex::encode(nonce), &sig, &xonly, &compressed).is_ok());
    }

    // ── [D8(i)] THE BUDGET HALF: terminality, attested ───────────────────────────────────────────

    #[test]
    fn an_attested_budget_verifies_and_a_substituted_one_does_not() {
        let (kp, compressed, xonly, nonce) = setup();
        let sig = attest(&kp, "coin-a", 1, Some(1), &nonce);
        assert!(
            verify_sig_count_attestation(
                "coin-a", 1, Some(1), &hex::encode(nonce), &sig, &xonly, &compressed).is_ok(),
            "the SE attested 1-of-1 — that is a TERMINAL coin and it must verify"
        );
        // THE ATTACK. A coordinator forwards a larger budget so the coin looks like it still has
        // co-signatures left — the reverse of under-reporting the count, and the same theft: the
        // receiver accepts a parent the sender can still spend.
        assert!(
            verify_sig_count_attestation(
                "coin-a", 1, Some(2), &hex::encode(nonce), &sig, &xonly, &compressed).is_err(),
            "an inflated budget must NOT verify — it would make a terminal coin look re-signable"
        );
        assert!(
            verify_sig_count_attestation(
                "coin-a", 1, Some(0), &hex::encode(nonce), &sig, &xonly, &compressed).is_err(),
            "…and a deflated one must not either: the signature covers one exact budget"
        );
    }

    /// **"No budget" and "budget 0" are opposite facts and must not share an encoding.**
    ///
    /// No budget = co-signable indefinitely. Budget 0 = terminal, nothing may ever be signed. If the
    /// preimage could not tell them apart, a coordinator could take an attestation over a terminal
    /// coin and present it as an unrestricted one, or the reverse. The explicit `has_budget` byte is
    /// what stops that, and this is the test that would fail if someone "simplified" it away.
    #[test]
    fn absent_and_zero_budgets_are_not_interchangeable() {
        let (kp, compressed, xonly, nonce) = setup();

        let none_sig = attest(&kp, "coin-a", 0, None, &nonce);
        let zero_sig = attest(&kp, "coin-a", 0, Some(0), &nonce);
        assert_ne!(none_sig, zero_sig, "the two must not produce the SAME signature");

        assert!(verify_sig_count_attestation(
            "coin-a", 0, None, &hex::encode(nonce), &none_sig, &xonly, &compressed).is_ok());
        assert!(verify_sig_count_attestation(
            "coin-a", 0, Some(0), &hex::encode(nonce), &zero_sig, &xonly, &compressed).is_ok());

        // …and neither vouches for the other.
        assert!(
            verify_sig_count_attestation(
                "coin-a", 0, Some(0), &hex::encode(nonce), &none_sig, &xonly, &compressed).is_err(),
            "an unrestricted coin's attestation must not pass as a terminal one"
        );
        assert!(
            verify_sig_count_attestation(
                "coin-a", 0, None, &hex::encode(nonce), &zero_sig, &xonly, &compressed).is_err(),
            "a terminal coin's attestation must not pass as an unrestricted one"
        );
    }

    /// The count and the budget are bound TOGETHER by one signature, so they cannot be mixed across
    /// time — a fresh count paired with a stale (higher) budget is exactly the confusion two
    /// separate attestations would have allowed.
    #[test]
    fn the_count_and_the_budget_cannot_be_recombined() {
        let (kp, compressed, xonly, nonce) = setup();
        let spent = attest(&kp, "coin-a", 2, Some(2), &nonce);   // terminal: 2 of 2
        let fresh = attest(&kp, "coin-a", 0, Some(2), &nonce);   // same coin, earlier: 0 of 2

        assert!(verify_sig_count_attestation(
            "coin-a", 2, Some(2), &hex::encode(nonce), &spent, &xonly, &compressed).is_ok());
        assert!(verify_sig_count_attestation(
            "coin-a", 0, Some(2), &hex::encode(nonce), &fresh, &xonly, &compressed).is_ok());
        // Neither signature can be made to say the other's pair.
        assert!(verify_sig_count_attestation(
            "coin-a", 0, Some(2), &hex::encode(nonce), &spent, &xonly, &compressed).is_err());
        assert!(verify_sig_count_attestation(
            "coin-a", 2, Some(2), &hex::encode(nonce), &fresh, &xonly, &compressed).is_err());
    }

    /// v1 is gone: an attestation built the old way, without the budget bytes, must not verify.
    /// Leaving it acceptable would leave a route to an attestation that says nothing about
    /// terminality, which is the whole gap D8(i) closes.
    #[test]
    fn a_v1_attestation_no_longer_verifies() {
        let (kp, compressed, xonly, nonce) = setup();
        let v1 = {
            let mut preimage: Vec<u8> = Vec::new();
            preimage.extend_from_slice(b"utexo/sig_count/v1");
            preimage.extend_from_slice(b"coin-a");
            preimage.extend_from_slice(&5u32.to_be_bytes());
            preimage.extend_from_slice(&nonce);
            let msg = Message::from_hashed_data::<sha256::Hash>(&preimage);
            Secp256k1::new().sign_schnorr(&msg, &kp).to_string()
        };
        for budget in [None, Some(0), Some(5)] {
            assert!(
                verify_sig_count_attestation(
                    "coin-a", 5, budget, &hex::encode(nonce), &v1, &xonly, &compressed).is_err(),
                "a v1 attestation must not verify under any budget reading (tried {budget:?})"
            );
        }
    }
}
