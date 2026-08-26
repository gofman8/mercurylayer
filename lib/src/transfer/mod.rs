use std::str::FromStr;

use bitcoin::{Transaction, secp256k1::PublicKey};
use secp256k1_zkp::musig::{MusigPubNonce, BlindingFactor};
use serde::{Deserialize, Serialize};

use crate::wallet::BackupTx;

pub mod cancel;
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

/// **[D38/D9] The conveyed transfer payload. Its body is UNAUTHENTICATED.**
///
/// `verify_transfer_signature`'s preimage is `(tx0_txid, tx0_vout_le, new_user_pubkey)` and nothing
/// else — no site in `lib/`, `clients/` or `server/` signs this struct. Decrypting it proves only
/// that it was addressed here; it proves nothing about the fields inside. So every accepted value
/// must be either DERIVED from an authenticated source (the chain, the SE, the receiver's own
/// record) or REFUSED, and `deny_unknown_fields` closes the other direction: a field this receiver
/// does not understand is a payload it is accepting anyway.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
    /// - `3` — DELETED. It was the exit-only in-ladder child, adopted without the key handover,
    ///   and it is no longer admissible (see `ADMISSIBLE_PROTOCOL_VERSIONS`).
    /// - `4` — an in-ladder split child WITH the key handover, which the receiver completes to make
    ///   the child a first-class coin.
    ///
    /// **[D38/D9+D16] REQUIRED — no `serde(default)`.** This is the one field where an omission
    /// became a positive claim: defaulting to `0` made "the sender said nothing" indistinguishable
    /// from "the sender declared the un-laddered carrier shape", and `0` is an ADMISSIBLE shape, so
    /// the omission selected a lane rather than being refused. Absence is now a parse error, which
    /// is the typed refusal D9 requires.
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
    /// **[REQ-83] A LADDERLESS claim (`Stub`), delivered as a DOCUMENT rather than a hand-over.**
    ///
    /// `SP.out[j]` already pays the payee's own key, so there is no secret to hand over and no slot
    /// to rotate: what the payee lacks is the transaction. A message carrying this and nothing else
    /// is a delivery, and the receiver MUST treat it as one — `t1` is meaningless on it and must
    /// never be consumed as a handover secret.
    ///
    /// Mutually exclusive with [`Self::child_tesr_bundle`] and [`Self::tesr_ladder`]: a message
    /// claiming to be both a delivery and a hand-over is a message whose kind the sender chose after
    /// the receiver started reading it.
    #[serde(default)]
    pub ladderless_leaf: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TxOutpoint {
    pub txid: String,
    pub vout: u32,
}

/// [D38/D9] The wire payload's acceptance discipline, exercised rather than asserted.
#[cfg(test)]
mod wire_discipline_tests {
    use super::TransferMsg;

    fn minimal(extra: &str) -> String {
        format!(
            r#"{{"statechain_id":"sid","transfer_signature":"sig",
                "backup_transactions":[],"t1":[{}],"user_public_key":"pk",
                "protocol_version":0{extra}}}"#,
            (0..32).map(|_| "0").collect::<Vec<_>>().join(",")
        )
    }

    /// An UNKNOWN field is a payload this receiver does not understand. Accepting it silently is the
    /// other half of the same defect as defaulting an absent one.
    #[test]
    fn an_unknown_field_is_refused() {
        let ok: Result<TransferMsg, _> = serde_json::from_str(&minimal(""));
        assert!(ok.is_ok(), "the minimal shape must still parse: {:?}", ok.err());

        let e = serde_json::from_str::<TransferMsg>(&minimal(r#","surprise":"anything""#))
            .expect_err("an unknown field must be refused");
        assert!(
            e.to_string().contains("surprise"),
            "the refusal must name the field: {e}"
        );
    }

    /// **The field where absence became a positive claim.** Defaulting `protocol_version` to 0 made
    /// "said nothing" indistinguishable from "declared the un-laddered carrier shape" — and 0 is an
    /// ADMISSIBLE shape, so the omission selected a lane instead of being refused.
    #[test]
    fn an_absent_protocol_version_is_a_parse_error_not_a_zero() {
        let without = r#"{"statechain_id":"sid","transfer_signature":"sig",
            "backup_transactions":[],"t1":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
            0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"user_public_key":"pk"}"#;
        let e = serde_json::from_str::<TransferMsg>(without)
            .expect_err("an omitted protocol_version must NOT default to 0");
        assert!(
            e.to_string().contains("protocol_version"),
            "the refusal must name the missing field: {e}"
        );
    }
}
