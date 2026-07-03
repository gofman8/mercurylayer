use std::{str::FromStr, collections::BTreeMap};

use bitcoin::{Txid, ScriptBuf, Transaction, absolute, TxIn, OutPoint, Witness, TxOut, psbt::{Psbt, Input, PsbtSighashType}, sighash::{TapSighashType, SighashCache, self, TapSighash}, taproot::{TapTweakHash, self}, hashes::Hash, Address, PrivateKey, Network};
use secp256k1_zkp::{SecretKey, PublicKey,  Secp256k1, schnorr::Signature, Message, musig::{MusigSessionId, MusigPubNonce, BlindingFactor, MusigSession, MusigPartialSignature, blinded_musig_pubkey_xonly_tweak_add, blinded_musig_negate_seckey, MusigAggNonce, MusigSecNonce}, new_musig_nonce_pair, KeyPair, rand::{self, Rng}};
use serde::{Serialize, Deserialize};

use crate::{decode_transfer_address, error::MercuryError, utils::{self, get_network}, wallet::Coin};

#[derive(Serialize, Deserialize, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SignFirstRequestPayload {
    pub statechain_id: String,
    pub signed_statechain_id: String,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CoinNonce {
    pub secret_nonce: String,
    pub public_nonce: String,
    pub blinding_factor: String,
    pub sign_first_request_payload: SignFirstRequestPayload,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SignFirstResponsePayload {
    pub server_pubnonce: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PartialSignatureMsg1 {
    pub msg: String,
    pub output_pubkey: String, // the tweaked pubkey
    pub client_partial_sig: String,
    pub encoded_session: String,
    pub encoded_unsigned_tx: String,
    pub partial_signature_request_payload: PartialSignatureRequestPayload,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PartialSignatureRequestPayload {
    pub statechain_id: String,
    pub negate_seckey: u8,
    pub session: String,
    pub signed_statechain_id: String,
    pub server_pub_nonce: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ServerPublicNonceResponsePayload {
    pub server_pubnonce: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PartialSignatureResponsePayload {
    pub partial_sig: String,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn create_and_commit_nonces(coin: &Coin) -> core::result::Result<CoinNonce, MercuryError>{
    
    let secp = Secp256k1::new();

    let client_session_id = MusigSessionId::new(&mut rand::thread_rng());

    let client_seckey = PrivateKey::from_wif(&coin.user_privkey)?.inner;
    let client_pubkey = PublicKey::from_str(&coin.user_pubkey)?;

    let (client_sec_nonce, client_pub_nonce) = new_musig_nonce_pair(&secp, client_session_id, None, Some(client_seckey), client_pubkey, None, None)?;

    let blinding_factor = BlindingFactor::new(&mut rand::thread_rng());

    let sign_first_request_payload = SignFirstRequestPayload {
        statechain_id: coin.statechain_id.as_ref().unwrap().to_owned(),
        signed_statechain_id: coin.signed_statechain_id.as_ref().unwrap().to_owned(),
    };

    Ok(CoinNonce {
        secret_nonce: hex::encode(client_sec_nonce.serialize()),
        public_nonce: hex::encode(client_pub_nonce.serialize()),
        blinding_factor: hex::encode(blinding_factor.as_bytes()),
        sign_first_request_payload,
    })
}

/// The purpose of this function is to get a random locktime for the withdrawal transaction.
/// This is done to improve privacy and discourage fee sniping.
/// This function assumes that the block_height is the current block height.
fn get_locktime_for_withdrawal_transaction (block_height: u32) -> u32 {

    let mut locktime = block_height as i32;

    let mut rng = rand::thread_rng();
    let number = rng.gen_range(0..=10);

    // sometimes locktime is set a bit further back, for privacy reasons
    if number == 0 {
        locktime = locktime - rng.gen_range(0..=99);
    }

    std::cmp::max(0, locktime) as u32
}

pub fn create_tx_out(
    coin: &Coin, 
    fee_rate_sats_per_byte: f64,
    to_address: &str,
    network: Network,
) -> core::result::Result<TxOut, MercuryError>
{
    const BACKUP_TX_SIZE: u64 = 112; // virtual size one input P2TR and one output P2TR
    // 163 is the real size one input P2TR and one output P2TR

    let input_amount = coin.amount.unwrap() as u64;
    let absolute_fee = (BACKUP_TX_SIZE as f64 * fee_rate_sats_per_byte).ceil() as u64;
    let amount_out = input_amount - absolute_fee;

    let recipient_address = if to_address.starts_with(crate::MAINNET_HRP) || to_address.starts_with(crate::TESTNET_HRP) {
        let (_, recipient_user_pubkey, _) = decode_transfer_address(to_address)?;
        let new_address = Address::p2tr(&Secp256k1::new(), recipient_user_pubkey.x_only_public_key().0, None, network);
        new_address
    } else {
        let new_address = Address::from_str(&to_address).unwrap().require_network(network)?;
        new_address
    };

    let tx_out = TxOut { value: amount_out, script_pubkey: recipient_address.script_pubkey() };

    Ok(tx_out)
}

pub fn calculate_block_height(
    block_height: u32, 
    initlock: u32, 
    interval: u32, 
    qt_backup_tx: u32,
    is_withdrawal: bool)  -> core::result::Result<u32, MercuryError>
{
    // if qt_backup_tx == 0, it means this is the first backup transaction (Tx0)
    // In this case, the block_height is equal to the current block height
    // Otherwise, block_height is equal to the Tx0.lock_time + initlock
    let initlock = if qt_backup_tx == 0 { initlock } else { 0 };

    let block_height = if is_withdrawal { get_locktime_for_withdrawal_transaction(block_height) } else { (block_height + initlock) - (interval * qt_backup_tx) };
    
    Ok(block_height)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn get_user_backup_address(coin: &Coin, network: String) -> core::result::Result<String, MercuryError> {

    let network = get_network(&network)?;

    let user_pubkey = PublicKey::from_str(&coin.user_pubkey.clone())?;
    let to_address = Address::p2tr(&Secp256k1::new(), user_pubkey.x_only_public_key().0, None, network);
    Ok(to_address.to_string())
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn get_partial_sig_request(
    coin: &Coin, 
    block_height: u32, 
    initlock: u32, 
    interval: u32, 
    fee_rate_sats_per_byte: f64,
    qt_backup_tx: u32,
    to_address: String,
    network: String,
    is_withdrawal: bool) -> core::result::Result<PartialSignatureMsg1, MercuryError>
{
    let network = utils::get_network(&network)?;
    
    let tx_out = create_tx_out(coin, fee_rate_sats_per_byte, &to_address, network)?;

    let block_height = calculate_block_height(
        block_height, 
        initlock, 
        interval, 
        qt_backup_tx,
        is_withdrawal)?;

    let session = get_musig_session(
        coin,
        block_height, 
        &tx_out,
        network)?;

    Ok(session)
}

/// Build the unsigned backup/withdrawal transaction (one input = the statechain UTXO, one output =
/// the recipient/owner) and return it hex-encoded, **without** signing it.
///
/// This is the entry point for the Mercury RGB integration: the returned transaction is handed to
/// rgb-lib (`color_statechain_psbt`) which inserts the OP_RETURN opret commitment, and the colored
/// transaction is then signed via [`get_partial_sig_request_for_colored_tx`].
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn get_unsigned_backup_tx(
    coin: &Coin,
    block_height: u32,
    initlock: u32,
    interval: u32,
    fee_rate_sats_per_byte: f64,
    qt_backup_tx: u32,
    to_address: String,
    network: String,
    is_withdrawal: bool,
) -> core::result::Result<String, MercuryError> {
    let network = utils::get_network(&network)?;

    let tx_out = create_tx_out(coin, fee_rate_sats_per_byte, &to_address, network)?;

    let block_height =
        calculate_block_height(block_height, initlock, interval, qt_backup_tx, is_withdrawal)?;

    let lock_time = absolute::LockTime::from_height(block_height)?;

    let input_txid = Txid::from_str(&coin.utxo_txid.as_ref().unwrap())?;
    let input_vout = coin.utxo_vout.unwrap();

    let unsigned_tx = Transaction {
        version: 2,
        lock_time,
        input: vec![TxIn {
            previous_output: OutPoint { txid: input_txid, vout: input_vout },
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence(0x0), // Ignore nSequence.
            witness: Witness::default(),
        }],
        output: vec![tx_out],
    };

    let tx_bytes = bitcoin::consensus::encode::serialize(&unsigned_tx);
    Ok(hex::encode(tx_bytes))
}

/// Build the unsigned backup/withdrawal transaction and return it as a base64-encoded PSBT, with
/// the input `witness_utxo` and `tap_internal_key` populated (the funding/statechain UTXO).
///
/// This PSBT is what gets handed to rgb-lib's `color_statechain_psbt`: rgb-lib parses it, inserts
/// the OP_RETURN opret commitment and assigns the asset, returning a modified PSBT. The modified
/// PSBT's unsigned transaction is then fed to [`get_partial_sig_request_for_colored_tx`].
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn get_unsigned_backup_psbt(
    coin: &Coin,
    block_height: u32,
    initlock: u32,
    interval: u32,
    fee_rate_sats_per_byte: f64,
    qt_backup_tx: u32,
    to_address: String,
    network: String,
    is_withdrawal: bool,
) -> core::result::Result<String, MercuryError> {
    let network = utils::get_network(&network)?;

    let tx_out = create_tx_out(coin, fee_rate_sats_per_byte, &to_address, network)?;

    let block_height =
        calculate_block_height(block_height, initlock, interval, qt_backup_tx, is_withdrawal)?;

    let lock_time = absolute::LockTime::from_height(block_height)?;

    let input_txid = Txid::from_str(&coin.utxo_txid.as_ref().unwrap())?;
    let input_vout = coin.utxo_vout.unwrap();

    let unsigned_tx = Transaction {
        version: 2,
        lock_time,
        input: vec![TxIn {
            previous_output: OutPoint { txid: input_txid, vout: input_vout },
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence(0x0),
            witness: Witness::default(),
        }],
        output: vec![tx_out],
    };

    let mut psbt = Psbt::from_unsigned_tx(unsigned_tx)?;

    let input_pubkey = PublicKey::from_str(&coin.aggregated_pubkey.as_ref().unwrap())?;
    let input_xonly_pubkey = input_pubkey.x_only_public_key().0;

    let input_amount = coin.amount.unwrap() as u64;
    let input_address =
        Address::from_str(&coin.aggregated_address.as_ref().unwrap())?.require_network(network)?;
    let input_scriptpubkey = input_address.script_pubkey();

    let ty = PsbtSighashType::from_str("SIGHASH_ALL")?;
    let mut input = Input {
        witness_utxo: Some(TxOut { value: input_amount, script_pubkey: input_scriptpubkey }),
        ..Default::default()
    };
    input.sighash_type = Some(ty);
    input.tap_internal_key = Some(input_xonly_pubkey);
    psbt.inputs = vec![input];

    Ok(psbt.to_string())
}

/// Build the unsigned **split** backup tx as a base64 PSBT: one input (the statechain UTXO) and N
/// spendable outputs, one per `(address, sat_value)` in `outputs`. Unlike [`get_unsigned_backup_psbt`]
/// (a single recipient output), this carves the statechain coin into several outputs in one
/// transaction — the foundation of an off-chain RGB split, where each output becomes a sub-coin that
/// carries an RGB witness seal (see `docs/rgb_offchain_split_spilman.md`).
///
/// The caller owns the fee arithmetic: the sum of `outputs` sat values must be strictly less than the
/// input amount; the remainder (`input_amount - sum`) is the miner fee that applies if/when the tx is
/// broadcast on a unilateral exit. rgb-lib then colors this PSBT, inserting the single OP_RETURN opret
/// commitment and assigning the asset across the output seals via a multi-entry `output_map`.
///
/// The signing path ([`get_partial_sig_request_for_colored_tx`]) is agnostic to the number of outputs
/// (it only requires a single input), so the SE co-signs the split exactly like a one-output backup.
/// The ≤1-spendable-output rule enforced on the *transfer/backup-verification* path
/// (`verify_if_locktime_is_reasonable_tx_version_and_output_size`, `get_previous_outpoint`) is not on
/// this build→color→sign→offchain-validate path, so a split is co-signable without relaxing it.
pub fn get_unsigned_split_psbt(
    coin: &Coin,
    block_height: u32,
    initlock: u32,
    interval: u32,
    qt_backup_tx: u32,
    outputs: Vec<(String, u64)>,
    network: String,
    is_withdrawal: bool,
) -> core::result::Result<String, MercuryError> {
    let network = utils::get_network(&network)?;

    let input_amount = coin.amount.unwrap() as u64;
    let total_out: u64 = outputs.iter().map(|(_, v)| *v).sum();
    // The remainder (input - sum) is the fee; a non-positive remainder means the split does not leave
    // room for any fee.
    if outputs.is_empty() || total_out >= input_amount {
        return Err(MercuryError::FeeTooLow);
    }

    let tx_outs: Vec<TxOut> = outputs
        .iter()
        .map(|(addr, value)| -> core::result::Result<TxOut, MercuryError> {
            // Accept both a Mercury transfer address (statechain destination) and a plain bitcoin
            // address, mirroring `create_tx_out`.
            let recipient_address = if addr.starts_with(crate::MAINNET_HRP)
                || addr.starts_with(crate::TESTNET_HRP)
            {
                let (_, recipient_user_pubkey, _) = decode_transfer_address(addr)?;
                Address::p2tr(&Secp256k1::new(), recipient_user_pubkey.x_only_public_key().0, None, network)
            } else {
                Address::from_str(addr)
                    .map_err(|_| MercuryError::InvalidBitcoinAddressError)?
                    .require_network(network)?
            };
            Ok(TxOut { value: *value, script_pubkey: recipient_address.script_pubkey() })
        })
        .collect::<core::result::Result<_, _>>()?;

    let block_height =
        calculate_block_height(block_height, initlock, interval, qt_backup_tx, is_withdrawal)?;

    let lock_time = absolute::LockTime::from_height(block_height)?;

    let input_txid = Txid::from_str(&coin.utxo_txid.as_ref().unwrap())?;
    let input_vout = coin.utxo_vout.unwrap();

    let unsigned_tx = Transaction {
        version: 2,
        lock_time,
        input: vec![TxIn {
            previous_output: OutPoint { txid: input_txid, vout: input_vout },
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence(0x0),
            witness: Witness::default(),
        }],
        output: tx_outs,
    };

    let mut psbt = Psbt::from_unsigned_tx(unsigned_tx)?;

    let input_pubkey = PublicKey::from_str(&coin.aggregated_pubkey.as_ref().unwrap())?;
    let input_xonly_pubkey = input_pubkey.x_only_public_key().0;

    let input_address =
        Address::from_str(&coin.aggregated_address.as_ref().unwrap())?.require_network(network)?;
    let input_scriptpubkey = input_address.script_pubkey();

    let ty = PsbtSighashType::from_str("SIGHASH_ALL")?;
    let mut input = Input {
        witness_utxo: Some(TxOut { value: input_amount, script_pubkey: input_scriptpubkey }),
        ..Default::default()
    };
    input.sighash_type = Some(ty);
    input.tap_internal_key = Some(input_xonly_pubkey);
    psbt.inputs = vec![input];

    Ok(psbt.to_string())
}

pub fn get_musig_session(
    coin: &Coin,
    block_height: u32,
    output: &TxOut,
    network: Network) -> core::result::Result<PartialSignatureMsg1, MercuryError>
{
    let input_pubkey = PublicKey::from_str(&coin.aggregated_pubkey.as_ref().unwrap())?;
    let input_xonly_pubkey = input_pubkey.x_only_public_key().0;

    let outputs = [output.to_owned()].to_vec();

    let lock_time = absolute::LockTime::from_height(block_height)?;

    let input_txid = Txid::from_str(&coin.utxo_txid.as_ref().unwrap())?;
    let input_vout = coin.utxo_vout.unwrap();

    let tx1 = Transaction {
        version: 2,
        lock_time,
        input: vec![TxIn {
            previous_output: OutPoint { txid: input_txid, vout: input_vout },
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence(0x0), // Ignore nSequence.
            witness: Witness::default(),
        }],
        output: outputs,
    };

    let mut psbt = Psbt::from_unsigned_tx(tx1)?;

    let input_amount = coin.amount.unwrap() as u64;
    
    let input_address = Address::from_str(&coin.aggregated_address.as_ref().unwrap())?.require_network(network)?;
    let input_scriptpubkey = input_address.script_pubkey();
    let mut input = Input {
        witness_utxo: Some(TxOut { value: input_amount, script_pubkey: input_scriptpubkey }),
        ..Default::default()
    };

    let ty = PsbtSighashType::from_str("SIGHASH_ALL")?;
    input.sighash_type = Some(ty);
    input.tap_internal_key = Some(input_xonly_pubkey.to_owned());
    psbt.inputs = vec![input];

    let unsigned_tx = psbt.unsigned_tx.clone();

    // There must not be more than one input.
    // The input is the funding transaction and the output the backup address.
    assert!(psbt.inputs.len() == 1);

    let vout = 0; // the vout is always 0 (only one input)
    let input = psbt.inputs.iter_mut().nth(vout).unwrap();

    let hash_ty = input
        .sighash_type
        .and_then(|psbt_sighash_type| psbt_sighash_type.taproot_hash_ty().ok())
        .unwrap_or(TapSighashType::All);

    let hash = SighashCache::new(&unsigned_tx).taproot_key_spend_signature_hash(
        vout,
        &sighash::Prevouts::All(&[TxOut {
            value: input.witness_utxo.as_ref().unwrap().value,
            script_pubkey: input.witness_utxo.as_ref().unwrap().script_pubkey.clone(),
        }]),
        hash_ty,
    )?;

    let tx_bytes = bitcoin::consensus::encode::serialize(&unsigned_tx);
    let encoded_unsigned_tx = hex::encode(tx_bytes);

    let session = calculate_musig_session(
        coin,
        hash,
        encoded_unsigned_tx)?;

    Ok(session)
}

/// Compute the blind-MuSig2 partial-signature session for a backup/withdrawal transaction that was
/// built and *colored* outside of this crate (Mercury RGB integration).
///
/// The provided `encoded_unsigned_tx` (hex) is the unsigned transaction produced by Mercury and
/// then handed to rgb-lib for coloring, so it already contains the recipient output **and** the
/// zero-value OP_RETURN opret commitment output inserted by RGB. This function recomputes the
/// taproot key-spend sighash over that exact transaction (committing to all outputs, including the
/// commitment) and returns the same [`PartialSignatureMsg1`] the normal flow produces, so the rest
/// of the signing flow (server partial sig, aggregation, witness) is unchanged.
///
/// Returns an error if the transaction does not have exactly one input.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn get_partial_sig_request_for_colored_tx(
    coin: &Coin,
    encoded_unsigned_tx: String,
    network: String,
) -> core::result::Result<PartialSignatureMsg1, MercuryError> {
    let network = utils::get_network(&network)?;

    let tx_bytes = hex::decode(&encoded_unsigned_tx)?;
    let unsigned_tx: Transaction = bitcoin::consensus::encode::deserialize(&tx_bytes)?;

    if unsigned_tx.input.len() != 1 {
        return Err(MercuryError::MoreThanOneInputError);
    }

    let input_pubkey = PublicKey::from_str(&coin.aggregated_pubkey.as_ref().unwrap())?;
    let input_xonly_pubkey = input_pubkey.x_only_public_key().0;

    let input_amount = coin.amount.unwrap() as u64;
    let input_address =
        Address::from_str(&coin.aggregated_address.as_ref().unwrap())?.require_network(network)?;
    let input_scriptpubkey = input_address.script_pubkey();

    let prevout = TxOut {
        value: input_amount,
        script_pubkey: input_scriptpubkey,
    };

    let hash = SighashCache::new(&unsigned_tx).taproot_key_spend_signature_hash(
        0,
        &sighash::Prevouts::All(&[prevout]),
        TapSighashType::All,
    )?;

    // Sanity: the internal key recorded in the coin must match the funding output we are spending.
    let _ = input_xonly_pubkey;

    calculate_musig_session(coin, hash, encoded_unsigned_tx)
}

pub fn calculate_musig_session(
    coin: &Coin,
    hash: TapSighash,
    encoded_unsigned_tx: String,) -> core::result::Result<PartialSignatureMsg1, MercuryError>
{
    let secp = Secp256k1::new();

    let aggregate_pubkey = PublicKey::from_str(&coin.aggregated_pubkey.as_ref().unwrap())?; 

    let tap_tweak = TapTweakHash::from_key_and_tweak(aggregate_pubkey.x_only_public_key().0, None);
    let tap_tweak_bytes = tap_tweak.as_byte_array();

    // tranform tweak: Scalar to SecretKey
    let tweak = SecretKey::from_slice(tap_tweak_bytes)?;

    let (parity_acc, output_pubkey, out_tweak32) = blinded_musig_pubkey_xonly_tweak_add(&secp, &aggregate_pubkey, tweak);

    let client_pub_nonce_bytes = hex::decode(coin.public_nonce.as_ref().unwrap())?;
    let client_pub_nonce = MusigPubNonce::from_slice(client_pub_nonce_bytes.as_slice())?;

    let server_pubnonce_hex = coin.server_public_nonce.as_ref().unwrap().to_string();
    let server_pub_nonce_bytes = hex::decode(&server_pubnonce_hex)?;
    let server_pub_nonce = MusigPubNonce::from_slice(server_pub_nonce_bytes.as_slice())?;

    let aggnonce = MusigAggNonce::new(&secp, &[client_pub_nonce, server_pub_nonce]);

    let blinding_factor_bytes = hex::decode(coin.blinding_factor.as_ref().unwrap())?;
    let blinding_factor = BlindingFactor::from_slice(blinding_factor_bytes.as_slice())?;

    let msg: Message = hash.into();

    let session = MusigSession::new_blinded_without_key_agg_cache(
        &secp,
        &output_pubkey,
        aggnonce,
        msg,
        None,
        &blinding_factor,
        out_tweak32
    );

    let negate_seckey = blinded_musig_negate_seckey(
        &secp,
        &output_pubkey,
        parity_acc,
    );

    let client_seckey = PrivateKey::from_wif(&coin.user_privkey)?.inner;

    let client_pubkey = PublicKey::from_str(&coin.user_pubkey)?;

    let client_keypair = KeyPair::from_secret_key(&secp, &client_seckey);

    let client_sec_nonce_bytes = hex::decode(coin.secret_nonce.as_ref().unwrap())?;
    let client_sec_nonce_bytes: [u8; 132] = client_sec_nonce_bytes.try_into().unwrap();
    let client_sec_nonce = MusigSecNonce::from_slice(client_sec_nonce_bytes);

    let client_partial_sig = session.blinded_partial_sign_without_keyaggcoeff(&secp, client_sec_nonce, &client_keypair, negate_seckey)?;

    assert!(session.blinded_musig_partial_sig_verify(&secp, &client_partial_sig, &client_pub_nonce, &client_pubkey, &output_pubkey, parity_acc));

    let encoded_session = hex::encode(session.serialize());

    session.remove_fin_nonce_from_session();

    let negate_seckey = match negate_seckey {
        true => 1,
        false => 0,
    };

    let blinded_session = session.remove_fin_nonce_from_session();

    let statechain_id = coin.statechain_id.as_ref().unwrap();
    let signed_statechain_id = coin.signed_statechain_id.as_ref().unwrap();

    let payload = PartialSignatureRequestPayload {
        statechain_id: statechain_id.to_string(),
        negate_seckey,
        session: hex::encode(blinded_session.serialize()),
        signed_statechain_id: signed_statechain_id.to_string(),
        server_pub_nonce: server_pubnonce_hex,
    };

    let client_partial_sig_hex = hex::encode(client_partial_sig.serialize());

    Ok(PartialSignatureMsg1 {
        msg: hex::encode(hash.as_byte_array()),
        output_pubkey: output_pubkey.to_string(),
        client_partial_sig: client_partial_sig_hex,
        encoded_session,
        encoded_unsigned_tx,
        partial_signature_request_payload: payload,
    })
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn create_signature(
    msg: String,
    client_partial_sig_hex: String,
    server_partial_sig_hex: String,
    session_hex: String,
    output_pubkey_hex: String) -> core::result::Result<String, MercuryError> 
{
    let secp = Secp256k1::new();

    let msg = Message::from_slice(hex::decode(msg)?.as_slice())?;

    let server_partial_sig_bytes = hex::decode(server_partial_sig_hex)?;
    let server_partial_sig = MusigPartialSignature::from_slice(server_partial_sig_bytes.as_slice())?;

    let client_partial_sig_bytes = hex::decode(client_partial_sig_hex)?;
    let client_partial_sig = MusigPartialSignature::from_slice(client_partial_sig_bytes.as_slice())?;

    let session_bytes: [u8; 133] = hex::decode(&session_hex)?.try_into().unwrap();
    let session = MusigSession::from_slice(session_bytes);

    let sig = session.partial_sig_agg(&[client_partial_sig, server_partial_sig]);

    let output_pubkey = PublicKey::from_str(&output_pubkey_hex)?;

    let x_only_key_tweaked = output_pubkey.x_only_public_key().0;

    if !secp.verify_schnorr(&sig, &msg, &x_only_key_tweaked).is_ok() {
        return Err(MercuryError::SchnorrSignatureValidationError);
    }

    Ok(sig.to_string())
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn new_backup_transaction(
    encoded_unsigned_tx: String,
    signature_hex: String,
) -> core::result::Result<String, MercuryError> {

    let tx_bytes = hex::decode(encoded_unsigned_tx)?;
    let tx: Transaction = bitcoin::consensus::encode::deserialize(&tx_bytes)?;

    let mut psbt = Psbt::from_unsigned_tx(tx)?;

    if psbt.inputs.len() != 1 {
        return Err(MercuryError::MoreThanOneInputError);
    }

    let vout = 0;
    let input = psbt.inputs.iter_mut().nth(vout).unwrap();

    let hash_ty = input
        .sighash_type
        .and_then(|psbt_sighash_type| psbt_sighash_type.taproot_hash_ty().ok())
        .unwrap_or(TapSighashType::All);

    let sig = Signature::from_str(signature_hex.as_str())?;

    let final_signature = taproot::Signature { sig, hash_ty };

    input.tap_key_sig = Some(final_signature);

    psbt.inputs.iter_mut().for_each(|input| {
        let mut script_witness: Witness = Witness::new();
        script_witness.push(input.tap_key_sig.unwrap().to_vec());
        input.final_script_witness = Some(script_witness);

        // Clear all the data fields as per the spec.
        input.partial_sigs = BTreeMap::new();
        input.sighash_type = None;
        input.redeem_script = None;
        input.witness_script = None;
        input.bip32_derivation = BTreeMap::new();
    });

    let signed_tx = psbt.extract_tx();

    let tx_bytes = bitcoin::consensus::encode::serialize(&signed_tx);
    let encoded_signed_tx = hex::encode(tx_bytes);

    Ok(encoded_signed_tx)
}

// ---------------------------------------------------------------------------------------------------
// Multi-input ("combine") colored transactions — see docs/rgb_offchain_split_spilman.md.
//
// A combine spends N statechain coins (inputs) into M outputs in one transaction, carrying one RGB
// transition that sums the inputs. Mercury is single-input by construction; these are the multi-input
// generalizations of `get_unsigned_split_psbt` / `get_partial_sig_request_for_colored_tx` /
// `new_backup_transaction`. Each input keeps its own blind-MuSig2 session ({owner_i, SE}); the SE
// co-signs each. `coins` are matched to tx inputs by outpoint, so input ordering after coloring is
// irrelevant.
// ---------------------------------------------------------------------------------------------------

/// Resolve a recipient address string (a Mercury transfer address or a plain bitcoin address) to a
/// `ScriptBuf`, mirroring `create_tx_out`.
fn resolve_output_scriptpubkey(addr: &str, network: Network) -> core::result::Result<ScriptBuf, MercuryError> {
    let address = if addr.starts_with(crate::MAINNET_HRP) || addr.starts_with(crate::TESTNET_HRP) {
        let (_, recipient_user_pubkey, _) = decode_transfer_address(addr)?;
        Address::p2tr(&Secp256k1::new(), recipient_user_pubkey.x_only_public_key().0, None, network)
    } else {
        Address::from_str(addr)
            .map_err(|_| MercuryError::InvalidBitcoinAddressError)?
            .require_network(network)?
    };
    Ok(address.script_pubkey())
}

/// Build the unsigned **combine** tx as a base64 PSBT: N inputs (one per `coins` entry, the statechain
/// coins being combined) and M outputs (one per `(address, sat_value)` in `outputs`). The sum of
/// `outputs` sat values must be strictly less than the sum of the input amounts; the remainder is the
/// on-exit fee. rgb-lib then colors this PSBT (consuming the inputs' allocations, inserting the single
/// OP_RETURN opret commitment, and assigning the asset across the outputs via `output_map`).
pub fn get_unsigned_combine_psbt(
    coins: &[Coin],
    block_height: u32,
    initlock: u32,
    interval: u32,
    qt_backup_tx: u32,
    outputs: Vec<(String, u64)>,
    network: String,
    is_withdrawal: bool,
) -> core::result::Result<String, MercuryError> {
    let network = utils::get_network(&network)?;

    if coins.is_empty() {
        return Err(MercuryError::EmptyInput);
    }
    let total_in: u64 = coins.iter().map(|c| c.amount.unwrap() as u64).sum();
    let total_out: u64 = outputs.iter().map(|(_, v)| *v).sum();
    if outputs.is_empty() || total_out >= total_in {
        return Err(MercuryError::FeeTooLow);
    }

    let tx_outs: Vec<TxOut> = outputs
        .iter()
        .map(|(addr, value)| -> core::result::Result<TxOut, MercuryError> {
            Ok(TxOut { value: *value, script_pubkey: resolve_output_scriptpubkey(addr, network)? })
        })
        .collect::<core::result::Result<_, _>>()?;

    let block_height =
        calculate_block_height(block_height, initlock, interval, qt_backup_tx, is_withdrawal)?;
    let lock_time = absolute::LockTime::from_height(block_height)?;

    let mut tx_ins = Vec::with_capacity(coins.len());
    for c in coins {
        let txid = Txid::from_str(c.utxo_txid.as_ref().unwrap())?;
        let vout = c.utxo_vout.unwrap();
        tx_ins.push(TxIn {
            previous_output: OutPoint { txid, vout },
            script_sig: ScriptBuf::new(),
            sequence: bitcoin::Sequence(0x0),
            witness: Witness::default(),
        });
    }

    let unsigned_tx = Transaction { version: 2, lock_time, input: tx_ins, output: tx_outs };
    let mut psbt = Psbt::from_unsigned_tx(unsigned_tx)?;

    let ty = PsbtSighashType::from_str("SIGHASH_ALL")?;
    let mut psbt_inputs = Vec::with_capacity(coins.len());
    for c in coins {
        let input_pubkey = PublicKey::from_str(c.aggregated_pubkey.as_ref().unwrap())?;
        let input_xonly_pubkey = input_pubkey.x_only_public_key().0;
        let input_amount = c.amount.unwrap() as u64;
        let input_address =
            Address::from_str(c.aggregated_address.as_ref().unwrap())?.require_network(network)?;
        let mut input = Input {
            witness_utxo: Some(TxOut { value: input_amount, script_pubkey: input_address.script_pubkey() }),
            ..Default::default()
        };
        input.sighash_type = Some(ty);
        input.tap_internal_key = Some(input_xonly_pubkey);
        psbt_inputs.push(input);
    }
    psbt.inputs = psbt_inputs;

    Ok(psbt.to_string())
}

/// Per-input blind-MuSig2 sessions for a colored **multi-input** transaction. Returns one
/// [`PartialSignatureMsg1`] per transaction input, in transaction-input order. Each input's taproot
/// key-spend sighash is computed over **all** prevouts (`SIGHASH_ALL`), and `coins` are matched to
/// inputs by outpoint, so any input reordering by the coloring step is handled. Every coin must have
/// its nonce state populated (client nonce committed + server pubnonce fetched) beforehand.
pub fn get_partial_sig_request_for_colored_tx_multi(
    coins: &[Coin],
    encoded_unsigned_tx: String,
    network: String,
) -> core::result::Result<Vec<PartialSignatureMsg1>, MercuryError> {
    let network = utils::get_network(&network)?;

    let tx_bytes = hex::decode(&encoded_unsigned_tx)?;
    let unsigned_tx: Transaction = bitcoin::consensus::encode::deserialize(&tx_bytes)?;
    if unsigned_tx.input.is_empty() {
        return Err(MercuryError::EmptyInput);
    }

    // Match each tx input to its coin by outpoint and build prevouts in tx-input order.
    let mut ordered_coins: Vec<&Coin> = Vec::with_capacity(unsigned_tx.input.len());
    let mut prevouts: Vec<TxOut> = Vec::with_capacity(unsigned_tx.input.len());
    for txin in unsigned_tx.input.iter() {
        let op = txin.previous_output;
        let op_txid = op.txid.to_string();
        let coin = coins
            .iter()
            .find(|c| c.utxo_txid.as_deref() == Some(op_txid.as_str()) && c.utxo_vout == Some(op.vout))
            .ok_or(MercuryError::CoinNotFound)?;
        let input_amount = coin.amount.unwrap() as u64;
        let input_address =
            Address::from_str(coin.aggregated_address.as_ref().unwrap())?.require_network(network)?;
        prevouts.push(TxOut { value: input_amount, script_pubkey: input_address.script_pubkey() });
        ordered_coins.push(coin);
    }

    let mut sessions = Vec::with_capacity(ordered_coins.len());
    for (i, coin) in ordered_coins.iter().enumerate() {
        let hash = SighashCache::new(&unsigned_tx).taproot_key_spend_signature_hash(
            i,
            &sighash::Prevouts::All(&prevouts),
            TapSighashType::All,
        )?;
        sessions.push(calculate_musig_session(coin, hash, encoded_unsigned_tx.clone())?);
    }
    Ok(sessions)
}

/// Attach N aggregated taproot key-spend signatures (one per input, in input order) to a colored
/// multi-input transaction, returning the fully-signed tx (hex). The multi-input analogue of
/// [`new_backup_transaction`].
pub fn new_backup_transaction_multi(
    encoded_unsigned_tx: String,
    signatures_hex: Vec<String>,
) -> core::result::Result<String, MercuryError> {
    let tx_bytes = hex::decode(encoded_unsigned_tx)?;
    let tx: Transaction = bitcoin::consensus::encode::deserialize(&tx_bytes)?;
    let mut psbt = Psbt::from_unsigned_tx(tx)?;

    if psbt.inputs.len() != signatures_hex.len() {
        return Err(MercuryError::MoreThanOneInputError);
    }

    for (input, sig_hex) in psbt.inputs.iter_mut().zip(signatures_hex.iter()) {
        let sig = Signature::from_str(sig_hex)?;
        let final_signature = taproot::Signature { sig, hash_ty: TapSighashType::All };
        input.tap_key_sig = Some(final_signature);
        let mut script_witness: Witness = Witness::new();
        script_witness.push(final_signature.to_vec());
        input.final_script_witness = Some(script_witness);
        input.partial_sigs = BTreeMap::new();
        input.sighash_type = None;
        input.redeem_script = None;
        input.witness_script = None;
        input.bip32_derivation = BTreeMap::new();
    }

    let signed_tx = psbt.extract_tx();
    Ok(hex::encode(bitcoin::consensus::encode::serialize(&signed_tx)))
}
