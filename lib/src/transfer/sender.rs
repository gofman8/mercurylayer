use std::str::FromStr;

use bitcoin::{secp256k1, hashes::sha256, Txid, PrivateKey};
use secp256k1_zkp::{Secp256k1, Message, Scalar};
use serde::{Serialize, Deserialize};
use serde_json::json;

use crate::{decode_transfer_address, error::MercuryError, wallet::{BackupTx, Coin}};

use super::TransferMsg;

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ExternalPaymentHashRequestPayload {
    pub statechain_id: String,
    pub auth_sig: String,
    pub batch_id: String,
    /// 32-byte hex payment hash (e.g. from a BOLT11 invoice).
    pub payment_hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UnlockByPreimageRequestPayload {
    pub batch_id: String,
    /// Hex preimage whose sha256 matches the latch's payment hash.
    pub preimage: String,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PaymentHashRequestPayload {
    pub statechain_id: String,
    pub auth_sig: String, // signed_statechain_id
    pub batch_id: String,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PaymentHashResponsePayload {
    pub hash: String,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferSenderRequestPayload {
    pub statechain_id: String,
    pub auth_sig: String, // signed_statechain_id
    pub new_user_auth_key: String,
    pub batch_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferSenderResponsePayload {
    pub x1: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferUpdateMsgRequestPayload {
    pub statechain_id: String,
    pub auth_sig: String, // signed_statechain_id
    pub new_user_auth_key: String,
    pub enc_transfer_msg: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferPreimageRequestPayload {
    pub statechain_id: String,
    pub auth_sig: String, // signed_statechain_id
    pub previous_user_auth_key: String,
    pub batch_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferPreimageResponsePayload {
    pub preimage: String,// signed_statechain_id
}

// Step 7. Owner 1 then concatinates the Tx0 outpoint with the Owner 2 public key (O2) and signs it with their key o1 to generate SC_sig_1.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn create_transfer_signature(recipient_address: &str, input_txid: &str, input_vout: u32, client_seckey: &str) ->  Result<String, MercuryError> {

    // new_user_pubkey: PublicKey, input_txid: &Txid, input_vout: u32, client_seckey: &SecretKey

    let (_, recipient_user_pubkey, _) = decode_transfer_address(recipient_address)?;

    let input_txid = Txid::from_str(&input_txid)?;
    let client_seckey = PrivateKey::from_wif(client_seckey)?.inner;

    let secp = Secp256k1::new();
    let keypair = secp256k1::KeyPair::from_seckey_slice(&secp, client_seckey.as_ref()).unwrap();

    let mut data_to_sign = Vec::<u8>::new();
    data_to_sign.extend_from_slice(&input_txid[..]);
    data_to_sign.extend_from_slice(&input_vout.to_le_bytes());
    data_to_sign.extend_from_slice(&recipient_user_pubkey.serialize()[..]);

    let msg = Message::from_hashed_data::<sha256::Hash>(&data_to_sign);
    let signature = secp.sign_schnorr(&msg, &keypair);

    Ok(signature.to_string())
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn create_transfer_update_msg(x1: &str, recipient_address: &str, coin: &Coin, transfer_signature: &str, backup_transactions: &Vec<BackupTx>) -> Result<TransferUpdateMsgRequestPayload, MercuryError> {
    create_transfer_update_msg_with_branch(x1, recipient_address, coin, transfer_signature, backup_transactions, &Vec::new(), &Vec::new(), 0, None)
}

/// [in-ladder split] Build the mailbox message that conveys a split **child** bundle to the receiver.
///
/// This carries the STANDARD Mercury key-handover (`transfer_signature` + the blinded `t1 = o1 + x1`)
/// alongside the child bundle, so the receiver can complete `/transfer/receiver`: the SE rotates its
/// share, leaving the child aggregate `A_child` INVARIANT (so the pre-signed child ladder stays valid)
/// while the sender's auth key is rotated out and it is permanently locked out of the child. That is
/// what makes a received child a FIRST-CLASS coin rather than an exit-only claim
/// (`docs/utexo/spec/CHILDREN.md`).
///
/// The message also sets `child_tesr_bundle` (JSON `ChildTesrBundle`) and `protocol_version = 4`; the
/// receiver's claim() detects that field and runs `verify_child_bundle` (census + parent terminality)
/// BEFORE completing the handover. `protocol_version = 3` is the legacy no-handover conveyance and is
/// still adoptable exit-only.
///
/// `backup_transactions` stays EMPTY (a child slot is never funded on-chain, so `create_tx1` never ran
/// for it — `CHILD_V2_BASELINE = 0`), and `branch_txs`/`terminal_parents` stay EMPTY on purpose: the
/// ancestor chain `F → T → X_m → SP` already travels inside `ChildTesrBundle.parent` and is validated
/// there. Routing it through `branch_txs` would trip `required_terminal_ancestors`, which counts one
/// named ancestor per structural input (3: T, X_m, SP) while the chain holds exactly ONE statechain node.
///
/// `child_coin` is the sender-owned piece child (its `statechain_id` + `signed_statechain_id` authorise
/// the `update_msg` post, since the sender created that slot at split time); the message is encrypted to
/// `recipient_address`'s auth key so only the receiver's mailbox can read it.
/// **[REQ-83] A LADDERLESS claim as a DELIVERY, not a hand-over.**
///
/// The mailbox route carries a statechain hand-over: opened with an `x1` against the child's slot and
/// signed with that child coin's key. A `Stub` has neither — no slot is created for it, because
/// `SP.out[j]` pays the payee's OWN key. What the payee lacks is not a secret but the TRANSACTION,
/// so the message carries a document and nothing else.
///
/// **`t1` is ZERO and must never be consumed.** It is the blinded handover secret, and there is no
/// handover here: a `ladderless_leaf` message rotates no key and transfers no slot. Zero rather than
/// a random value on purpose — a plausible-looking secret is one a receiver might try to use, while
/// zero cannot be mistaken for one and the receiver refuses the message as a hand-over by shape.
///
/// It is posted under the SENDER'S OWN statechain id, because that is the only auth key the
/// coordinator can check for this message — and the coordinator checks exactly that and stores the
/// ciphertext. Nothing in the document is believed on that account: the receiver verifies it against
/// the chain and the SE's attested facts, the same as any conveyed leaf.
pub fn create_ladderless_conveyance_update_msg(
    recipient_address: &str,
    sender_coin: &Coin,
    ladderless_leaf_json: &str,
) -> Result<TransferUpdateMsgRequestPayload, MercuryError> {
    let (_, _, recipient_auth_pubkey) = decode_transfer_address(recipient_address)?;
    let statechain_id = sender_coin
        .statechain_id
        .as_ref()
        .ok_or(MercuryError::SecpError)?;
    let signed_statechain_id = sender_coin
        .signed_statechain_id
        .as_ref()
        .ok_or(MercuryError::SecpError)?;

    let transfer_msg = TransferMsg {
        statechain_id: statechain_id.to_string(),
        // No outpoint changes hands, so there is nothing for a transfer signature to commit to. The
        // empty string is refused by `verify_transfer_signature`, which is correct: this message must
        // never reach the hand-over path.
        transfer_signature: String::new(),
        backup_transactions: Vec::new(),
        t1: [0u8; 32],
        user_public_key: sender_coin.user_pubkey.clone(),
        branch_txs: Vec::new(),
        terminal_parents: Vec::new(),
        protocol_version: 4,
        tesr_ladder: None,
        child_tesr_bundle: None,
        ladderless_leaf: Some(ladderless_leaf_json.to_string()),
    };

    let transfer_msg_json_str = serde_json::to_string_pretty(&json!(&transfer_msg))?;
    let serialized_new_auth_pubkey = &recipient_auth_pubkey.serialize();
    let encrypted_msg = ecies::encrypt(serialized_new_auth_pubkey, transfer_msg_json_str.as_bytes())
        .map_err(|_| MercuryError::SecpError)?;
    let encrypted_msg_hex = hex::encode(&encrypted_msg);
    Ok(TransferUpdateMsgRequestPayload {
        statechain_id: statechain_id.to_string(),
        auth_sig: signed_statechain_id.to_string(),
        new_user_auth_key: recipient_auth_pubkey.to_string(),
        enc_transfer_msg: encrypted_msg_hex,
    })
}

pub fn create_child_conveyance_update_msg(
    x1: &str,
    recipient_address: &str,
    child_coin: &Coin,
    transfer_signature: &str,
    child_tesr_bundle_json: &str,
) -> Result<TransferUpdateMsgRequestPayload, MercuryError> {
    let (_, _, recipient_auth_pubkey) = decode_transfer_address(recipient_address)?;

    let statechain_id = child_coin
        .statechain_id
        .as_ref()
        .ok_or(MercuryError::SecpError)?;
    let signed_statechain_id = child_coin
        .signed_statechain_id
        .as_ref()
        .ok_or(MercuryError::SecpError)?;

    // The blinded handover secret, exactly as the flat lane builds it (see
    // `create_transfer_update_msg_with_branch`): t1 = o1 + x1, where o1 is this child slot's owner key.
    let client_seckey = PrivateKey::from_wif(&child_coin.user_privkey)?.inner;
    let x1 = hex::decode(x1)?;
    let x1: [u8; 32] = x1.try_into().map_err(|_| MercuryError::SecpError)?;
    let x1 = Scalar::from_be_bytes(x1)?;
    let t1 = client_seckey.add_tweak(&x1)?;

    let transfer_msg = TransferMsg {
        statechain_id: statechain_id.to_string(),
        transfer_signature: transfer_signature.to_string(),
        backup_transactions: Vec::new(),
        t1: t1.secret_bytes(),
        user_public_key: child_coin.user_pubkey.clone(),
        branch_txs: Vec::new(),
        terminal_parents: Vec::new(),
        protocol_version: 4,
        tesr_ladder: None,
        child_tesr_bundle: Some(child_tesr_bundle_json.to_string()),
        ladderless_leaf: None,
    };

    let transfer_msg_json_str = serde_json::to_string_pretty(&json!(&transfer_msg))?;
    let serialized_new_auth_pubkey = &recipient_auth_pubkey.serialize();
    let encrypted_msg = ecies::encrypt(serialized_new_auth_pubkey, transfer_msg_json_str.as_bytes())
        .map_err(|_| MercuryError::SecpError)?;

    Ok(TransferUpdateMsgRequestPayload {
        statechain_id: statechain_id.to_string(),
        auth_sig: signed_statechain_id.to_string(),
        new_user_auth_key: recipient_auth_pubkey.to_string(),
        enc_transfer_msg: hex::encode(&encrypted_msg),
    })
}

/// Like [`create_transfer_update_msg`] but attaching an exit branch (fully-signed raw txs hex,
/// root-first) and the `terminal_parents` (ancestor statechain ids the receiver must verify are
/// terminal at the SE) for a coin whose funding tx is un-broadcast — an off-chain sub-coin.
pub fn create_transfer_update_msg_with_branch(x1: &str, recipient_address: &str, coin: &Coin, transfer_signature: &str, backup_transactions: &Vec<BackupTx>, branch_txs: &Vec<String>, terminal_parents: &Vec<String>, protocol_version: u32, tesr_ladder: Option<String>) -> Result<TransferUpdateMsgRequestPayload, MercuryError> {

    let (_, _, recipient_auth_pubkey) = decode_transfer_address(recipient_address)?;  

    let client_seckey = PrivateKey::from_wif(&coin.user_privkey)?.inner;
    let client_public_key = coin.user_pubkey.to_string();

    let x1 = hex::decode(x1)?;
    let x1: [u8; 32] = x1.try_into().unwrap();
    let x1 = Scalar::from_be_bytes(x1)?;
    
    let t1 = client_seckey.add_tweak(&x1)?;

    let statechain_id = coin.statechain_id.as_ref().unwrap();
    let signed_statechain_id = coin.signed_statechain_id.as_ref().unwrap();

    let transfer_msg = TransferMsg {
        statechain_id: statechain_id.to_string(),
        transfer_signature: transfer_signature.to_string(),
        backup_transactions: backup_transactions.to_owned(),
        t1: t1.secret_bytes(),
        user_public_key: client_public_key,
        branch_txs: branch_txs.to_owned(),
        terminal_parents: terminal_parents.to_owned(),
        protocol_version,
        tesr_ladder,
        child_tesr_bundle: None,
        ladderless_leaf: None,
    };

    let transfer_msg_json = json!(&transfer_msg);

    let transfer_msg_json_str = serde_json::to_string_pretty(&transfer_msg_json)?;

    let msg = transfer_msg_json_str.as_bytes();

    let serialized_new_auth_pubkey = &recipient_auth_pubkey.serialize();
    let encrypted_msg = ecies::encrypt(serialized_new_auth_pubkey, msg);

    if encrypted_msg.is_err() {
        return Err(MercuryError::SecpError);
    }

    let encrypted_msg = encrypted_msg.unwrap();

    let encrypted_msg_string = hex::encode(&encrypted_msg);

    let transfer_update_msg_request_payload = TransferUpdateMsgRequestPayload {
        statechain_id: statechain_id.to_string(),
        auth_sig: signed_statechain_id.to_string(),
        new_user_auth_key: recipient_auth_pubkey.to_string(),
        enc_transfer_msg: encrypted_msg_string.clone(),
    };

    Ok(transfer_update_msg_request_payload)
}
 
