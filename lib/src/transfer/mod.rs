use std::str::FromStr;

use bitcoin::{Transaction, secp256k1::PublicKey};
use secp256k1_zkp::musig::{MusigPubNonce, BlindingFactor};
use serde::{Deserialize, Serialize};

use crate::wallet::BackupTx;

pub mod receiver;
pub mod sender;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SenderBackupTransaction {
    pub statechain_id: String,
    pub tx_n: u32,
    pub tx: Transaction,
    pub client_public_nonce: Vec<u8>,
    pub server_public_nonce: Vec<u8>,
    pub client_public_key: PublicKey,
    pub server_public_key: PublicKey,
    pub blinding_factor: Vec<u8>,
    pub recipient_address: String,
}

/// This struct is similar to `SenderBackupTransaction`
/// but it is after the deserialization process because
/// `MusigPubNonce` amd `BlindingFactor` do not support
/// `Serialize` and `Deserialize` traits.
pub struct ReceiverBackupTransaction {
    pub statechain_id: String,
    pub tx_n: u32,
    pub tx: Transaction,
    pub client_public_nonce: MusigPubNonce,
    pub server_public_nonce: MusigPubNonce,
    pub client_public_key: PublicKey,
    pub server_public_key: PublicKey,
    pub blinding_factor: BlindingFactor,
    pub recipient_address: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SerializedBackupTransaction {
    pub tx_n: u32,
    pub  tx: String,
    pub client_public_nonce: String,
    pub server_public_nonce: String,
    pub client_public_key: String,
    pub server_public_key: String,
    pub blinding_factor: String,
    pub recipient_address: String,
}

impl SenderBackupTransaction {
    pub fn serialize(&self) -> SerializedBackupTransaction {
        SerializedBackupTransaction {
            tx_n: self.tx_n,
            tx: hex::encode(&bitcoin::consensus::encode::serialize(&self.tx)),
            client_public_nonce: hex::encode(&self.client_public_nonce),
            server_public_nonce: hex::encode(&self.server_public_nonce),
            client_public_key: self.client_public_key.to_string(),
            server_public_key: self.server_public_key.to_string(),
            blinding_factor: hex::encode(&self.blinding_factor),
            recipient_address: self.recipient_address.clone(),
        }
    }
}

impl SerializedBackupTransaction {
    pub fn deserialize(&self) -> ReceiverBackupTransaction {
        ReceiverBackupTransaction {
            statechain_id: "".to_string(),
            tx_n: self.tx_n,
            tx: bitcoin::consensus::encode::deserialize(&hex::decode(&self.tx).unwrap()).unwrap(),
            client_public_nonce: MusigPubNonce::from_slice(hex::decode(&self.client_public_nonce).unwrap().as_slice()).unwrap(),
            server_public_nonce: MusigPubNonce::from_slice(hex::decode(&self.server_public_nonce).unwrap().as_slice()).unwrap(),
            client_public_key: PublicKey::from_str(&self.client_public_key).unwrap(),
            server_public_key: PublicKey::from_str(&self.server_public_key).unwrap(),
            blinding_factor: BlindingFactor::from_slice(hex::decode(&self.blinding_factor).unwrap().as_slice()).unwrap(),
            recipient_address: self.recipient_address.clone(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TransferMsg1 {
    pub statechain_id: String,
    pub transfer_signature: String,
    pub backup_transactions: Vec<SerializedBackupTransaction>,
    pub t1: [u8; 32],
    pub user_public_key: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TransferMsg {
    pub statechain_id: String,
    pub transfer_signature: String,
    pub backup_transactions: Vec<BackupTx>,
    pub t1: [u8; 32],
    pub user_public_key: String,
    /// Exit branch for a coin whose funding tx is un-broadcast (an off-chain split/combine
    /// sub-coin): fully-signed raw txs (hex), root-first, from a spend of an ON-CHAIN outpoint
    /// down to the tx that funds this coin. Empty for ordinary on-chain coins; serde-default
    /// keeps the message wire-compatible with vanilla wallets.
    #[serde(default)]
    pub branch_txs: Vec<String>,
    /// Statechain ids of the structural ancestor nodes (the split/combine parents) that produced
    /// this sub-coin. The receiver must verify each is TERMINAL at the SE (its spend budget is
    /// exhausted) before accepting — otherwise a malicious sender could later double-spend an
    /// ancestor and invalidate the branch. Empty for ordinary on-chain coins.
    #[serde(default)]
    pub terminal_parents: Vec<String>,
    /// Conveyance format of this transfer, which tells the receiver WHICH validation to run:
    /// - `0`/absent — an un-laddered coin: only the flat backup chain is conveyed, and the receiver
    ///   runs the plain backup-count check (`num_sigs == backup_transactions.len()`).
    /// - `>= 2` — a laddered coin: `tesr_ladder` is conveyed and the receiver verifies that exit
    ///   ladder and its exact sig-count instead.
    /// - `3` — an in-ladder split child (`child_tesr_bundle`) with NO key handover; adopted
    ///   exit-only.
    /// - `4` — an in-ladder split child WITH the key handover, which the receiver completes to make
    ///   the child a first-class coin.
    ///
    /// serde-default keeps the message wire-compatible with wallets that predate the field.
    #[serde(default)]
    pub protocol_version: u32,
    /// JSON-serialized TES-R exit ladder (`mercuryrustlib::tesr::TesrBundle`) conveyed for R′
    /// verification; present only when `protocol_version >= 2`.
    #[serde(default)]
    pub tesr_ladder: Option<String>,
    /// JSON-serialized in-ladder split child bundle (`mercuryrustlib::tesr::ChildTesrBundle`) conveyed
    /// when this transfer is a NON-EXACT payment funded by splitting a laddered coin (protocol_version
    /// `>= 3`). The receiver verifies it with `verify_child_bundle` (parent `F` on-chain, child `F` =
    /// the un-broadcast `SP.out[j]`, both sids terminal) instead of the plain ladder. serde-default keeps
    /// the message wire-compatible with wallets that predate the field.
    #[serde(default)]
    pub child_tesr_bundle: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TxOutpoint {
    pub txid: String,
    pub vout: u32,
}
