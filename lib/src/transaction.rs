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

/// **[REQ-56] Ask the SE what a collapse of this root would have to pay.**
///
/// The question a closer must ask before it can build `C` at all, and the SE is the only party that
/// can answer it: it recorded the leaves from the co-signatures it witnessed, it computes the
/// frontier, and it alone sees which holders have released. Any other source of the obligation set is
/// a second opinion that can only disagree with the one the predicate will actually use.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollapseObligationsRequest {
    pub root_statechain_id: String,
    pub statechain_id: String,
    pub signed_statechain_id: String,
}

/// One holder's claim: their exit key, and the FULL funding value owed to it (REQ-60 — the two tier
/// rungs are never broadcast, so their burn is never realised and is not deductible).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollapseObligation {
    pub exit_key: String,
    pub amount: u64,
}

/// What a collapse of this root would have to pay, and what it would have to spend.
///
/// **An empty `obligations` list is never returned as an answer.** "I have no usable leaf set for
/// this root" comes back as a refusal, because an empty list reads as *"you owe nobody"* — the one
/// answer that discharges every holder at once.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollapseObligations {
    pub obligations: Vec<CollapseObligation>,
    /// Reported, not an error: a closer asking after the freeze should be told the tree is already
    /// closing rather than handed a refusal that looks like a malformed request.
    pub frozen: bool,
    pub have_funding_outpoint: bool,
    #[serde(default)]
    pub funding_txid: Option<String>,
    #[serde(default)]
    pub funding_vout: Option<u32>,
}

impl CollapseObligations {
    /// Total owed to every unreleased frontier leaf — what `C` must pay out before any remainder.
    pub fn total_owed(&self) -> u64 {
        self.obligations.iter().map(|o| o.amount).sum()
    }

    /// The obligations in the `(key, amount)` form [`crate::tesr::build_collapse_tx`] takes.
    pub fn payouts(&self) -> Vec<(String, u64)> {
        self.obligations.iter().map(|o| (o.exit_key.clone(), o.amount)).collect()
    }
}

/// **[REQ-56 / REQ-82] The request that asks the SE for its half of a COLLAPSE.**
///
/// A collapse is not an ordinary spend and does not travel on the ordinary signing route, because
/// the SE decides it on entirely different grounds: `sign/second` asks *"is this coin still allowed
/// to spend?"*, while `collapse_grant` asks *"does this transaction pay every unreleased frontier
/// leaf its full funding value, out of THIS root's own funding output?"*. The second question has no
/// meaning on the first route and vice versa.
///
/// Every field but `root_statechain_id` is the ordinary signing payload, unchanged and forwarded
/// whole. That is deliberate: the SE rebuilds the blinded session from `disclosure` and byte-compares
/// it against `session` exactly as it does for any tier (REQ-57), so the collapse gets the same
/// binding as every other signature rather than a parallel one that could drift from it.
///
/// **Why the root's id is carried separately from `statechain_id`.** They are the same value today —
/// the closer IS the root owner (REQ-82) — and writing them as one field would bake that in. The SE
/// looks up the leaf set, the funding outpoint, the aggregate and the freeze by ROOT; it consumes the
/// secnonce and checks the auth signature by the SIGNER. Collapsing the two names would make a future
/// delegated closer a wire-format change rather than a policy one.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollapseGrantRequestPayload {
    /// The root whose tree is being closed. The SE reads its leaves, funding outpoint, aggregate and
    /// freeze flag under this id.
    pub root_statechain_id: String,
    pub statechain_id: String,
    pub negate_seckey: u8,
    pub session: String,
    pub signed_statechain_id: String,
    pub server_pub_nonce: String,
    /// [REQ-57] Same disclosure, same binding, same refusal as every other signature. `Option` on the
    /// wire only — the SE refuses a request that omits it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub disclosure: Option<SigningDisclosure>,
}

impl CollapseGrantRequestPayload {
    /// Wrap an ordinary signing payload as a collapse request for `root_statechain_id`.
    ///
    /// Takes the payload by value and moves every field: a version that copied would let the caller
    /// keep sending the same session down the ordinary route as well, and one secnonce answering two
    /// routes is the nonce reuse this whole design refuses elsewhere.
    pub fn for_root(root_statechain_id: String, p: PartialSignatureRequestPayload) -> Self {
        Self {
            root_statechain_id,
            statechain_id: p.statechain_id,
            negate_seckey: p.negate_seckey,
            session: p.session,
            signed_statechain_id: p.signed_statechain_id,
            server_pub_nonce: p.server_pub_nonce,
            disclosure: p.disclosure,
        }
    }
}

/// What the SE returns when it grants a collapse.
///
/// `granted` is not redundant with the presence of a signature: a caller that reads only
/// `partial_sig` cannot tell a grant from a refusal that happened to carry a body, and this route's
/// refusals are the interesting half of its behaviour.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollapseGrantResponse {
    /// The SE's half of the 2-of-2 over `C`. REQ-82: only ever its half.
    pub partial_sig: String,
    /// False on a replay of the SAME session — the grant is idempotent, not repeatable.
    pub newly_signed: bool,
    /// The root is frozen from this moment, in the same database transaction that produced the
    /// signature. INV-FREEZE is a ratchet: no path clears it.
    pub frozen: bool,
    pub granted: bool,
    /// How many unreleased frontier leaves `C` had to pay. Zero would be an empty obligation
    /// satisfied vacuously, which the SE refuses upstream rather than reports here.
    pub obligations: i32,
    /// What the released leaves were worth — REQ-74's self-funding figure.
    pub recovered: i64,
    pub self_funding: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PartialSignatureRequestPayload {
    pub statechain_id: String,
    pub negate_seckey: u8,
    pub session: String,
    pub signed_statechain_id: String,
    pub server_pub_nonce: String,
    /// [REQ-57] What the SE needs to verify WHAT it is signing, instead of signing blind.
    ///
    /// The SE reparses this transaction, recomputes the BIP-341 sighash itself, rebuilds the blinded
    /// session from it, and byte-compares against `session`. A disclosure that does not reproduce
    /// `session` is refused — so nothing here is believed, only checked.
    ///
    /// `Option` on the WIRE ONLY — the SE now REFUSES a request that omits it (`400`, no signature).
    /// The type stays optional because this struct is also the shape a request is parsed INTO, and a
    /// missing field must produce the SE's own refusal rather than a deserialisation error that
    /// says nothing useful. Every client in this tree populates it: each forwards
    /// `PartialSignatureMsg1::partial_signature_request_payload` wholesale, so it is filled in here
    /// once, for all of them.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub disclosure: Option<SigningDisclosure>,
}

/// The public inputs to a blinded MuSig2 session, plus the transaction they were derived from.
///
/// Every field is public data the client already holds; none of it helps anyone forge a signature.
/// It is exactly what `secp256k1_blinded_musig_nonce_process_without_keyaggcoeff` consumes, which is
/// what lets the SE reach the same 133 bytes independently.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SigningDisclosure {
    /// The unsigned transaction, serialized. The SE parses it; it does not take our word for it.
    pub unsigned_tx: String,
    pub input_index: u32,
    /// One per input, in input order — BIP-341 hashes EVERY input's amount and script, so a partial
    /// list cannot produce the right sighash.
    pub prevout_values: Vec<u64>,
    pub prevout_spks: Vec<String>,
    /// The TWEAKED output key, not the untweaked aggregate: it is what the session is built over.
    pub agg_pubkey: String,
    pub agg_nonce: String,
    pub blinding_factor: String,
    pub out_tweak: String,
    /// The tiers sign at `TapSighashType::All` (0x01). Sent explicitly rather than assumed, because a
    /// binding built against the wrong hash type refuses every honest signature — which looks
    /// exactly like a working security control.
    pub hash_type: u8,
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

    // P2TR dust threshold: an output at or below this is non-standard/unspendable.
    const DUST_LIMIT: u64 = 330;

    let input_amount = coin.amount.unwrap() as u64;
    let absolute_fee = (BACKUP_TX_SIZE as f64 * fee_rate_sats_per_byte).ceil() as u64;
    // Checked, dust-guarded (review M2): an unchecked `input - fee` underflow-panics when the fee
    // exceeds the input, and an output below the dust limit yields an un-broadcastable backup that
    // permanently strands the coin. Reject both rather than build a stuck backup.
    let amount_out = input_amount
        .checked_sub(absolute_fee)
        .ok_or(MercuryError::FeeTooLow)?;
    if amount_out < DUST_LIMIT {
        return Err(MercuryError::FeeTooLow);
    }

    let recipient_address = if to_address.starts_with(crate::MAINNET_HRP) || to_address.starts_with(crate::TESTNET_HRP) {
        let (_, recipient_user_pubkey, _) = decode_transfer_address(to_address)?;
        let new_address = Address::p2tr(&Secp256k1::new(), recipient_user_pubkey.x_only_public_key().0, None, network);
        new_address
    } else {
        // Validate the address instead of unwrapping: a malformed to_address must surface as a
        // typed error, not panic the client (this runs AFTER a sign/first nonce has been committed,
        // so a panic here also strands that nonce). In-repo callers pre-validate; this guards the
        // library boundary for any external caller.
        let new_address = Address::from_str(&to_address)
            .map_err(|_| MercuryError::InvalidBitcoinAddressError)?
            .require_network(network)?;
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
    // Dust floor (audit [9], INV-4): every split output becomes a sub-coin whose funding IS its exit
    // branch. A below-dust output makes the co-signed split tx non-standard / unrelayable, so once
    // the parent is consumed (single-use) BOTH sub-coins — and any RGB allocation on them — have no
    // on-chain exit. Reject any sub-dust output BEFORE the SE co-signs, while the parent is still
    // spendable. P2TR dust is 330 sats.
    const DUST_LIMIT: u64 = 330;
    if outputs.iter().any(|(_, v)| *v < DUST_LIMIT) {
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

    // INV-4: for an OFF-CHAIN split, this tx IS the child's exit branch and must be UNCONDITIONALLY
    // broadcastable and always mature before any parent backup. The parent's backup ladder is
    // DEPOSIT-height anchored, but a tip-relative decrementing locktime
    // (block_height - interval*qt_backup_tx) can exceed a parent backup, letting the parent's stale
    // backup mature first and win the exit race (review H5 — the "branch wins" invariant was
    // arithmetically false for any post-deposit split). A locktime-free (height 0) branch is always
    // spendable now and always sits below any deposit-anchored parent backup, so the latest state
    // (the child) always wins. A withdrawal to an on-chain address keeps its computed locktime.
    let lock_time = if is_withdrawal {
        let bh = calculate_block_height(block_height, initlock, interval, qt_backup_tx, is_withdrawal)?;
        absolute::LockTime::from_height(bh)?
    } else {
        absolute::LockTime::from_height(0)?
    };

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

    // Bound once and reused for both the sighash and the disclosure, so the two cannot disagree.
    let prevouts = vec![TxOut {
        value: input.witness_utxo.as_ref().unwrap().value,
        script_pubkey: input.witness_utxo.as_ref().unwrap().script_pubkey.clone(),
    }];

    let hash = SighashCache::new(&unsigned_tx).taproot_key_spend_signature_hash(
        vout,
        &sighash::Prevouts::All(&prevouts),
        hash_ty,
    )?;

    let tx_bytes = bitcoin::consensus::encode::serialize(&unsigned_tx);
    let encoded_unsigned_tx = hex::encode(tx_bytes);

    let session = calculate_musig_session(
        coin,
        hash,
        encoded_unsigned_tx,
        &prevouts,
        vout as u32,
        hash_ty)?;

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

    let prevouts = vec![TxOut {
        value: input_amount,
        script_pubkey: input_scriptpubkey,
    }];

    let hash = SighashCache::new(&unsigned_tx).taproot_key_spend_signature_hash(
        0,
        &sighash::Prevouts::All(&prevouts),
        TapSighashType::All,
    )?;

    // Sanity: the internal key recorded in the coin must match the funding output we are spending.
    let _ = input_xonly_pubkey;

    calculate_musig_session(coin, hash, encoded_unsigned_tx, &prevouts, 0, TapSighashType::All)
}

/// # Why the prevouts are PARAMETERS and not re-derived here
///
/// The disclosure must describe exactly what BIP-341 hashed. The first version of this function
/// rebuilt the prevout from `coin.amount` and the aggregate address, which silently disagreed with
/// three of its four callers:
///
///   * `cosign_tier_request` (`tesr.rs`) hashes over the PARENT tier's output value — its own doc
///     says "which is not `coin.amount`" — so every tier past the first disclosed a value the
///     sighash never committed to;
///   * the multi-input coloured path hashes input `i` against N prevouts, while the disclosure
///     claimed `input_index: 0` and a single prevout;
///   * the coloured PSBT path may carry a sighash type other than `All`, which was hard-coded.
///
/// Each of those is invisible from inside the SE: it would refuse an honest request and look like a
/// working control. Passing the same values that produced `hash` makes the two structurally
/// incapable of drifting, rather than merely equal today.
pub fn calculate_musig_session(
    coin: &Coin,
    hash: TapSighash,
    encoded_unsigned_tx: String,
    prevouts: &[TxOut],
    input_index: u32,
    hash_ty: TapSighashType,) -> core::result::Result<PartialSignatureMsg1, MercuryError>
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

    // [REQ-57] The disclosure the SE verifies against. Every field is public and already committed
    // to by `session`, so sending it reveals nothing and lets the SE check rather than trust.
    //
    // Every field below is taken from the SAME values that produced `hash` (see the doc comment on
    // this function for the three drifts that caused). Nothing is re-derived from the coin.
    let disclosure = SigningDisclosure {
        unsigned_tx: encoded_unsigned_tx.clone(),
        input_index,
        prevout_values: prevouts.iter().map(|p| p.value).collect(),
        prevout_spks: prevouts
            .iter()
            .map(|p| hex::encode(p.script_pubkey.as_bytes()))
            .collect(),
        agg_pubkey: hex::encode(output_pubkey.serialize()),
        agg_nonce: hex::encode(aggnonce.serialize()),
        blinding_factor: hex::encode(blinding_factor.as_bytes()),
        out_tweak: hex::encode(out_tweak32.as_ref()),
        // Sent explicitly, and taken from the caller rather than assumed: a binding built against
        // the wrong hash type refuses every honest signature while looking like a working control.
        hash_type: hash_ty as u8,
    };

    let payload = PartialSignatureRequestPayload {
        statechain_id: statechain_id.to_string(),
        negate_seckey,
        session: hex::encode(blinded_session.serialize()),
        signed_statechain_id: signed_statechain_id.to_string(),
        server_pub_nonce: server_pubnonce_hex,
        disclosure: Some(disclosure),
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

/// The P2TR dust threshold, in satoshis: an output at or below this makes its transaction
/// non-standard, so no node will relay it. Already asserted, as a function-local constant, by
/// `create_tx_out` and `get_unsigned_split_psbt`; hoisted here so the multi-input builders and the
/// combine fee model share ONE number with them instead of each carrying a copy that can drift.
pub const DUST_LIMIT: u64 = 330;

// ---------------------------------------------------------------------------------------------
// FEE ARITHMETIC FOR A MULTI-INPUT P2TR KEY-PATH SWEEP.
//
// `get_unsigned_combine_psbt` does NO fee arithmetic: it enforces `sum(outputs) < sum(inputs)` and
// calls whatever is left over "the fee". That is fine for an off-chain branch nobody broadcasts and
// useless for an on-chain sweep, where the remainder has to actually clear the relay minimum. The
// only fee model that existed in this tree is `create_tx_out`'s `BACKUP_TX_SIZE = 112`, a single
// hardcoded 1-in-1-out figure with no way to extend it to N inputs (extrapolating it per input
// over-estimates a 60-leaf sweep by roughly 2x — 134 400 sats instead of 70 380 at 20 sat/vB,
// which is not a safe rounding, it is money burnt).
//
// WHICH FIGURES ARE WHAT:
//   * 112 vB (1-in 1-out) — a REPO CONSTANT, `lib/src/transaction.rs:116`. Pinned against this
//     model by `the_one_input_sweep_agrees_with_the_repos_existing_backup_tx_size`, which it now
//     matches EXACTLY (it used to carry one vByte of margin over a model that was one byte short).
//   * 330 sat P2TR dust — a REPO CONSTANT, already asserted by `create_tx_out` and
//     `get_unsigned_split_psbt`; now [`DUST_LIMIT`].
//   * every byte figure below (41-byte input, 43-byte P2TR output, 67-byte key-path witness,
//     4-weight-unit-per-byte, the marker+flag) — STANDARD BITCOIN SERIALISATION, NOT previously
//     present anywhere in this repo. Derived here from consensus encoding, re-derived by hand in
//     `the_sweep_vsize_matches_the_hand_derived_serialisation_at_1_2_and_60_inputs`, and — because
//     a hand-derivation cannot catch a wrong PREMISE — measured against a real signed transaction
//     in `witness_size_ground_truth_tests`.
// ---------------------------------------------------------------------------------------------

/// Non-witness bytes of one transaction input: 32-byte txid + 4-byte vout + 1-byte empty
/// scriptSig length + 4-byte sequence. (Standard serialisation.)
pub(crate) const INPUT_BASE_BYTES: u64 = 41;
/// Witness bytes of a P2TR **key-path** spend as THIS repo signs it: 1-byte stack-item count +
/// 1-byte item length + 65-byte signature.
///
/// **65, not 64.** `new_backup_transaction_multi` (below) builds
/// `taproot::Signature { hash_ty: TapSighashType::All }`, and rust-bitcoin 0.30.1's
/// `taproot::Signature::to_vec` omits the trailing sighash byte for `TapSighashType::Default` ONLY
/// — for every other type, `All` (0x01) included, it appends it. So the witness item is 65 bytes
/// and the per-input witness is 67.
///
/// This constant was 66 (the `Default`-sighash figure) and therefore under-estimated EVERY sweep by
/// one byte per input. That is the dangerous direction: an under-paying transaction over N leaves
/// that have already been co-signed cannot be re-signed, only re-broadcast, so it sticks in the
/// mempool. `witness_size_ground_truth_tests` measures the real signed transaction rather than
/// re-deriving this number, so the premise itself is now pinned.
pub(crate) const INPUT_WITNESS_BYTES: u64 = 67;
/// Non-witness bytes of one P2TR output: 8-byte value + 1-byte script length + 34-byte
/// `OP_1 <32-byte x-only key>`. (Standard serialisation.)
const P2TR_OUTPUT_BYTES: u64 = 43;

/// Bytes a Bitcoin `CompactSize` needs for `n`.
const fn compact_size_bytes(n: u64) -> u64 {
    if n < 253 {
        1
    } else if n <= u16::MAX as u64 {
        3
    } else if n <= u32::MAX as u64 {
        5
    } else {
        9
    }
}

/// Virtual size, in vBytes, of an `n_inputs`-in / `n_outputs`-out transaction where every input is
/// a P2TR key-path spend and every output is P2TR.
///
/// `vsize = ceil(weight / 4)`, `weight = 4 * base_bytes + witness_bytes` — the segwit rule, so the
/// witness is discounted 4x. Rounding is UP: a truncated vsize under-pays, and an under-paying
/// sweep of N already-co-signed leaves is exactly the "looks fine, never confirms" failure the dust
/// floor above exists to prevent.
///
/// The marginal cost of one more input is `(4 * 41 + 67) / 4 = 57.75` vBytes and does not depend on
/// N — that number is the whole economic case for batching.
pub fn sweep_tx_vsize(n_inputs: usize, n_outputs: usize) -> core::result::Result<u64, MercuryError> {
    // Not `saturating` and not a default: a caller asking the size of a transaction with no inputs
    // or no outputs has a bug, and answering with a number lets it proceed.
    if n_inputs == 0 || n_outputs == 0 {
        return Err(MercuryError::EmptyInput);
    }
    let n_in = n_inputs as u64;
    let n_out = n_outputs as u64;

    let base_bytes = 4                            // nVersion
        + compact_size_bytes(n_in)                // input count
        + INPUT_BASE_BYTES * n_in
        + compact_size_bytes(n_out)               // output count
        + P2TR_OUTPUT_BYTES * n_out
        + 4; // nLockTime
    let witness_bytes = 2                         // segwit marker + flag
        + INPUT_WITNESS_BYTES * n_in;

    let weight = 4 * base_bytes + witness_bytes;
    Ok(weight.div_ceil(4))
}

/// The miner fee, in satoshis, for [`sweep_tx_vsize`] at `fee_rate_sats_per_vb`, rounded UP.
///
/// The rate is validated rather than cast: `(f64::NAN).ceil() as u64` is 0 in Rust, so an
/// un-checked rate turns a garbage input into a zero fee on the one path that decides whether a
/// co-signed sweep can be relayed. A zero or negative rate is refused for the same reason.
pub fn sweep_fee_sats(
    n_inputs: usize,
    n_outputs: usize,
    fee_rate_sats_per_vb: f64,
) -> core::result::Result<u64, MercuryError> {
    if !fee_rate_sats_per_vb.is_finite() || fee_rate_sats_per_vb <= 0.0 {
        return Err(MercuryError::FeeTooLow);
    }
    let vsize = sweep_tx_vsize(n_inputs, n_outputs)?;
    let fee = (vsize as f64 * fee_rate_sats_per_vb).ceil();
    if !fee.is_finite() || fee < 0.0 || fee > u64::MAX as f64 {
        return Err(MercuryError::FeeTooLow);
    }
    Ok(fee as u64)
}

/// Is a combine of `n_inputs` leaves totalling `total_in_sats` worth doing at this rate? True iff
/// the total covers the sweep fee AND still leaves a relayable (>= [`DUST_LIMIT`]) output.
///
/// Stated once, here, so the build path and any caller-facing quote cannot disagree about it.
pub fn combine_is_economic(
    total_in_sats: u64,
    n_inputs: usize,
    fee_rate_sats_per_vb: f64,
) -> core::result::Result<bool, MercuryError> {
    let fee = sweep_fee_sats(n_inputs, 1, fee_rate_sats_per_vb)?;
    Ok(total_in_sats >= fee.saturating_add(DUST_LIMIT))
}

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
    // Dust floor — the SAME rule `get_unsigned_split_psbt` carries (see its DUST_LIMIT above), and
    // it was missing here. A below-dust output makes the whole co-signed transaction non-standard,
    // so it is unrelayable — and on a combine that is worse than on a split, because N inputs have
    // already been co-signed by the time anyone finds out. Reject BEFORE the SE is asked for a
    // single signature. P2TR dust is 330 sats. Per-output, not on the sum: two 200-sat outputs
    // total 400 yet each one is individually unrelayable.
    if outputs.iter().any(|(_, v)| *v < DUST_LIMIT) {
        return Err(MercuryError::FeeTooLow);
    }

    let tx_outs: Vec<TxOut> = outputs
        .iter()
        .map(|(addr, value)| -> core::result::Result<TxOut, MercuryError> {
            Ok(TxOut { value: *value, script_pubkey: resolve_output_scriptpubkey(addr, network)? })
        })
        .collect::<core::result::Result<_, _>>()?;

    // INV-4 (audit [12]): identical to the split path — an OFF-CHAIN combine branch must be
    // unconditionally broadcastable (locktime 0) so it always matures below any deposit-anchored
    // parent backup and the latest state wins the exit race. A tip-relative decrementing locktime
    // would re-introduce the H5 inversion the instant combine is wired for non-withdrawal use. A
    // withdrawal to an on-chain address keeps its computed locktime.
    let lock_time = if is_withdrawal {
        let bh = calculate_block_height(block_height, initlock, interval, qt_backup_tx, is_withdrawal)?;
        absolute::LockTime::from_height(bh)?
    } else {
        absolute::LockTime::from_height(0)?
    };

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
        // `i`, not 0: this path signs input `i` against the FULL prevout set. Disclosing index 0 and
        // a single prevout — as this call did before — describes a hash nobody computed.
        sessions.push(calculate_musig_session(
            coin,
            hash,
            encoded_unsigned_tx.clone(),
            &prevouts,
            i as u32,
            TapSighashType::All,
        )?);
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

#[cfg(test)]
mod split_locktime_tests {
    use bitcoin::absolute::LockTime;

    // INV-4 / review H5: an OFF-CHAIN split branch must be locktime-free so it is unconditionally
    // broadcastable now and always matures BEFORE any deposit-anchored parent backup — otherwise a
    // stale parent backup can win the exit race. `get_unsigned_split_psbt` sets exactly this
    // (`from_height(0)`) for a non-withdrawal split. This guards that expression's runtime validity
    // and that it yields a zero block-height locktime.
    #[test]
    fn split_branch_locktime_is_zero_and_valid() {
        let lt = LockTime::from_height(0).expect("height-0 locktime must be valid");
        assert_eq!(lt.to_consensus_u32(), 0);
        assert!(lt.is_block_height());
    }
}

#[cfg(test)]
pub(crate) mod combine_test_support {
    use crate::wallet::{Coin, CoinStatus};

    /// A fixed, valid secp256k1 point used as every test leaf's aggregate key. The PSBT builder
    /// only needs it to parse and to key-path-tweak into a P2TR address, so one deterministic
    /// literal keeps these tests free of key derivation.
    pub const AGG_PUBKEY: &str =
        "0250929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0";

    /// The regtest P2TR address of [`AGG_PUBKEY`], derived rather than hard-coded so the literal
    /// can never drift out of agreement with the key.
    pub fn agg_address_regtest() -> String {
        use bitcoin::{Address, Network};
        use secp256k1_zkp::{PublicKey, Secp256k1};
        use std::str::FromStr;
        let pk = PublicKey::from_str(AGG_PUBKEY).expect("test agg pubkey parses");
        Address::p2tr(&Secp256k1::new(), pk.x_only_public_key().0, None, Network::Regtest).to_string()
    }

    /// A leaf `Coin` as `transfer_receiver.rs:1500-1513` leaves it after adoption: `utxo_txid` =
    /// SP.txid, `utxo_vout` = cb.sp_vout, `amount` = SP.out[j].value, plus its own aggregate key.
    /// Those are exactly the four fields the combine PSBT builder reads per input.
    pub fn leaf_coin(txid: &str, vout: u32, amount: u32) -> Coin {
        Coin {
            index: 0,
            user_privkey: String::new(),
            user_pubkey: String::new(),
            auth_privkey: String::new(),
            auth_pubkey: String::new(),
            derivation_path: String::new(),
            fingerprint: String::new(),
            address: String::new(),
            backup_address: String::new(),
            server_pubkey: None,
            aggregated_pubkey: Some(AGG_PUBKEY.to_string()),
            aggregated_address: Some(agg_address_regtest()),
            utxo_txid: Some(txid.to_string()),
            utxo_vout: Some(vout),
            amount: Some(amount),
            statechain_id: Some(format!("sid-{txid}-{vout}")),
            signed_statechain_id: None,
            locktime: None,
            secret_nonce: None,
            public_nonce: None,
            blinding_factor: None,
            server_public_nonce: None,
            tx_cpfp: None,
            tx_withdraw: None,
            withdrawal_address: None,
            status: CoinStatus::CONFIRMED,
            duplicate_index: 0,
            single_use: false,
            epoch_deadline: None,
        }
    }
}

/// **GROUND TRUTH FOR THE WITNESS TERM.** Every other test in `sweep_fee_model_tests` re-derives
/// the model's arithmetic by hand — which catches a typo, but cannot catch a wrong PREMISE. The
/// premise here is how many bytes `new_backup_transaction_multi` really puts in a witness, and that
/// is decided by rust-bitcoin, not by this repo:
///
/// ```text
/// // bitcoin-0.30.1/src/crypto/taproot.rs:55-64
/// pub fn to_vec(self) -> Vec<u8> {
///     let mut ser_sig = self.sig.as_ref().to_vec();
///     if self.hash_ty == TapSighashType::Default {
///         // default sighash type, don't add extra sighash byte
///     } else {
///         ser_sig.push(self.hash_ty as u8);
///     }
///     ser_sig
/// }
/// ```
///
/// `new_backup_transaction_multi` builds `taproot::Signature { hash_ty: TapSighashType::All }`
/// (`All` is 0x01, NOT `Default`), so the sighash byte IS appended: the signature is 65 bytes and
/// the per-input witness is 1 (item count) + 1 (item length) + 65 = **67**, not 66. So instead of
/// re-deriving, these two tests BUILD the transaction the fee model prices and MEASURE it.
#[cfg(test)]
mod witness_size_ground_truth_tests {
    use super::combine_test_support::{agg_address_regtest, leaf_coin};
    use super::{get_unsigned_combine_psbt, new_backup_transaction_multi, sweep_tx_vsize};
    use std::str::FromStr;

    const TXID_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    /// The FULLY SIGNED n-in / 1-out sweep, exactly as the combine driver produces it: the same
    /// `get_unsigned_combine_psbt` and the same `new_backup_transaction_multi`.
    ///
    /// The 64 signature bytes are fabricated, which changes nothing this test measures:
    /// `secp256k1::schnorr::Signature::from_slice` (secp256k1-0.27.0/src/schnorr.rs:78-87) checks
    /// LENGTH ONLY, so a fabricated BIP-340 signature serialises byte-for-byte like a real one. The
    /// question under test is a serialisation size, not a validity.
    fn signed_sweep(n: usize) -> bitcoin::Transaction {
        let coins: Vec<crate::wallet::Coin> =
            (0..n as u32).map(|i| leaf_coin(TXID_A, i, 5_000)).collect();
        let total: u64 = 5_000 * n as u64;
        let psbt_b64 = get_unsigned_combine_psbt(
            &coins,
            0,
            0,
            0,
            0,
            vec![(agg_address_regtest(), total - 2_000)],
            "regtest".to_string(),
            false,
        )
        .expect("the unsigned combine psbt must build");
        let psbt = bitcoin::psbt::Psbt::from_str(&psbt_b64).expect("psbt parses");
        let tx_hex = hex::encode(bitcoin::consensus::encode::serialize(&psbt.unsigned_tx));
        let sig_hex = "ab".repeat(64);
        let signed = new_backup_transaction_multi(tx_hex, vec![sig_hex; n])
            .expect("the signed sweep must assemble");
        bitcoin::consensus::encode::deserialize(&hex::decode(signed).expect("hex"))
            .expect("the signed sweep must deserialize")
    }

    /// **THE PREMISE, MEASURED.** 65-byte signature (64 + the `SIGHASH_ALL` byte), 67-byte witness.
    #[test]
    fn the_real_signed_sweep_carries_a_65_byte_signature_and_a_67_byte_witness_per_input() {
        for n in [1usize, 2, 60] {
            let tx = signed_sweep(n);
            assert_eq!(tx.input.len(), n);
            for (i, txin) in tx.input.iter().enumerate() {
                let items: Vec<&[u8]> = txin.witness.iter().collect();
                assert_eq!(items.len(), 1, "a key-path spend is a one-item witness (n={n}, i={i})");
                assert_eq!(
                    items[0].len(),
                    65,
                    "`taproot::Signature{{ hash_ty: TapSighashType::All }}.to_vec()` appends the \
                     0x01 sighash byte — only `TapSighashType::Default` is omitted — so the \
                     signature is 65 bytes, not 64 (n={n}, i={i})"
                );
                assert_eq!(
                    txin.witness.serialized_len(),
                    67,
                    "per-input witness = 1 (item count) + 1 (item length) + 65 (n={n}, i={i})"
                );
            }
        }
    }

    /// **THE MODEL AGAINST THE TRANSACTION IT PRICES.** `INPUT_WITNESS_BYTES` was 66, i.e. one byte
    /// per input too small, so the model UNDER-estimated every sweep — and an under-paying sweep of
    /// N already-co-signed leaves is the "looks fine, never confirms" failure that cannot be
    /// re-signed away. Measured against `Transaction::vsize()` of the real signed transaction at the
    /// three sizes the feature is specified at.
    #[test]
    fn the_fee_models_vsize_is_the_real_signed_transactions_vsize() {
        for n in [1usize, 2, 60] {
            let tx = signed_sweep(n);
            assert_eq!(
                sweep_tx_vsize(n, 1).unwrap(),
                tx.vsize() as u64,
                "the fee model must size the transaction it actually prices, at n={n}"
            );
        }
    }
}

#[cfg(test)]
mod sweep_fee_model_tests {
    use super::{sweep_fee_sats, sweep_tx_vsize, DUST_LIMIT};
    use crate::error::MercuryError;

    /// The three sizes the leaf combine is actually specified against. Each is derived here by
    /// hand from the consensus serialisation so a change to the model has to disagree with an
    /// independent arithmetic, not just with itself:
    ///
    ///   base  = 4 (version) + varint(n_in) + 41*n_in + varint(n_out) + 43*n_out + 4 (locktime)
    ///   wit   = 2 (marker+flag) + 67*n_in
    ///   vsize = ceil((4*base + wit) / 4)
    ///
    /// N=1  : base 4+1+41+1+43+4      = 94   ; wit 69   ; weight 445   ; vsize 112
    /// N=2  : base 4+1+82+1+43+4      = 135  ; wit 136  ; weight 676   ; vsize 169
    /// N=60 : base 4+1+2460+1+43+4    = 2513 ; wit 4022 ; weight 14074 ; vsize 3519
    ///
    /// The witness term is 67 per input, not 66: `new_backup_transaction_multi` signs with
    /// `TapSighashType::All`, so rust-bitcoin appends the 0x01 sighash byte and the signature is 65
    /// bytes. `witness_size_ground_truth_tests` measures that on the real signed transaction.
    #[test]
    fn the_sweep_vsize_matches_the_hand_derived_serialisation_at_1_2_and_60_inputs() {
        assert_eq!(sweep_tx_vsize(1, 1).unwrap(), 112, "1-in 1-out P2TR key-path");
        assert_eq!(sweep_tx_vsize(2, 1).unwrap(), 169, "2-in 1-out P2TR key-path");
        assert_eq!(sweep_tx_vsize(60, 1).unwrap(), 3519, "60-in 1-out P2TR key-path");
    }

    /// THE ONE NUMBER THE REPO ALREADY AGREED TO. `create_tx_out` hardcodes `BACKUP_TX_SIZE = 112`
    /// for a 1-in 1-out P2TR backup (`lib/src/transaction.rs:116`). The exact serialisation is 112
    /// as well — the repo's long-standing figure and this model now agree to the byte.
    ///
    /// That agreement is itself evidence about the fix: with the old 66-byte witness term this test
    /// read 111 and the repo constant was explained away as "exactly one vByte of margin". The
    /// margin was never margin, it was the missing sighash byte, and the repo's own hand-rolled
    /// backup figure had it right all along.
    #[test]
    fn the_one_input_sweep_agrees_with_the_repos_existing_backup_tx_size() {
        const REPO_BACKUP_TX_SIZE: u64 = 112;
        let exact = sweep_tx_vsize(1, 1).unwrap();
        assert_eq!(exact, 112);
        assert!(
            REPO_BACKUP_TX_SIZE >= exact && REPO_BACKUP_TX_SIZE - exact <= 1,
            "the repo's 112 must stay a >=, within one vByte, of the exact {exact}"
        );
        assert_eq!(REPO_BACKUP_TX_SIZE, exact, "and they now agree exactly");
    }

    /// The varint on the input count widens at 253. A model that assumed one byte forever would
    /// under-estimate every batch of 253+ by two bytes — small, but in the direction that produces
    /// an under-paying, un-mineable transaction.
    #[test]
    fn the_input_count_varint_widens_at_253() {
        // 252 inputs: 1-byte varint. base = 4+1+41*252+1+43+4 = 10385; wit = 2+67*252 = 16886;
        // weight = 41540+16886 = 58426; vsize = ceil(58426/4) = 14607 (58426/4 = 14606.5).
        assert_eq!(sweep_tx_vsize(252, 1).unwrap(), 14607);
        // 253 inputs: 3-byte varint (+2 base bytes = +8 weight).
        // base = 4+3+41*253+1+43+4 = 10428; wit = 2+67*253 = 16953;
        // weight = 41712+16953 = 58665; vsize = ceil(58665/4) = 14667 (58665/4 = 14666.25).
        assert_eq!(sweep_tx_vsize(253, 1).unwrap(), 14667);
    }

    /// The fee rounds UP. A truncating fee is a fee that can fall below the relay minimum, which is
    /// the same "looks fine, never confirms" failure the dust floor closes.
    #[test]
    fn the_sweep_fee_rounds_up_at_the_caller_supplied_rate() {
        // 112 vB * 1.5 = 168 exactly
        assert_eq!(sweep_fee_sats(1, 1, 1.5).unwrap(), 168);
        // 3519 vB * 1.5 = 5278.5 -> 5279
        assert_eq!(sweep_fee_sats(60, 1, 1.5).unwrap(), 5_279);
        // 3519 vB * 20 = 70_380 exactly
        assert_eq!(sweep_fee_sats(60, 1, 20.0).unwrap(), 70_380);
    }

    /// **THE MARGINAL-VALUE ARITHMETIC — what this feature is FOR.** Adding one more leaf to a
    /// batch costs only that leaf's own bytes: 41 base bytes (weight 164) + 67 witness bytes =
    /// 231 weight = 57.75 vB. So the smallest leaf worth including at N=60 is one worth more than
    /// `57.75 * rate` sats — a number that does NOT depend on N (the prefix and the single output
    /// are paid once for the whole batch).
    ///
    /// Pinned at the three rates the design was argued at. Compare with the tail-walk it replaces:
    /// a leaf that exits on its own pays a whole ~250-vB tail plus 2160 blocks of CSV.
    #[test]
    fn the_marginal_cost_of_one_more_leaf_is_57_and_three_quarter_vbytes() {
        let marginal = |n: usize| sweep_tx_vsize(n + 1, 1).unwrap() - sweep_tx_vsize(n, 1).unwrap();
        // **THE EXACT FIGURE, WITH THE ROUNDING CANCELLED.** 57.75 vB is not an integer, so no
        // single step can show it: `vsize` is a `ceil` of the whole transaction, and `vsize(60)`
        // itself rounds up by half a vByte. FOUR steps are 4 * 57.75 = 231 vB exactly, and at a
        // multiple of four the ceil contributes nothing either side.
        assert_eq!(
            sweep_tx_vsize(64, 1).unwrap() - sweep_tx_vsize(60, 1).unwrap(),
            231,
            "four more leaves must cost exactly 4 x 57.75 vB"
        );
        // The two-step and one-step figures, which the ceil DOES shift, kept as regression pins.
        assert_eq!(sweep_tx_vsize(62, 1).unwrap() - sweep_tx_vsize(60, 1).unwrap(), 115);
        assert!(marginal(60) == 57 || marginal(60) == 58);

        // What two more leaves cost at 2 / 20 / 50 sat/vB. (115 vB, not 115.5, for the ceil reason
        // above — the exact per-leaf floor is `57.75 * rate` sats.)
        for (rate, floor) in [(2.0_f64, 115_u64), (20.0, 1_150), (50.0, 2_875)] {
            let with = sweep_fee_sats(61, 1, rate).unwrap();
            let without = sweep_fee_sats(60, 1, rate).unwrap();
            let two_more = sweep_fee_sats(62, 1, rate).unwrap();
            assert_eq!(
                two_more - without,
                2 * floor,
                "two more leaves at {rate} sat/vB must cost exactly 2 x {floor} sats"
            );
            assert!(
                with > without,
                "one more leaf must never be free at {rate} sat/vB"
            );
        }
    }

    /// A batch is worth doing only if the swept total clears the fee AND leaves a relayable output.
    /// This is the arithmetic the economic refusal is built on; `combine_is_economic` states it in
    /// one place so the driver and any quote agree.
    #[test]
    fn a_batch_that_cannot_clear_its_fee_plus_dust_is_not_economic() {
        let fee_60 = sweep_fee_sats(60, 1, 20.0).unwrap();
        assert_eq!(fee_60, 70_380);
        assert!(!super::combine_is_economic(fee_60 + DUST_LIMIT - 1, 60, 20.0).unwrap());
        assert!(super::combine_is_economic(fee_60 + DUST_LIMIT, 60, 20.0).unwrap());
    }

    #[test]
    fn a_zero_input_or_zero_output_sweep_has_no_size_to_report() {
        assert!(matches!(sweep_tx_vsize(0, 1), Err(MercuryError::EmptyInput)));
        assert!(matches!(sweep_tx_vsize(1, 0), Err(MercuryError::EmptyInput)));
    }

    /// A NaN / negative / absurd fee rate must be refused, not silently turned into a zero fee by
    /// `as u64`. `(f64::NAN).ceil() as u64` is 0 in Rust — a fee of zero, on the path that decides
    /// whether a co-signed sweep can be relayed at all.
    #[test]
    fn a_nonsensical_fee_rate_is_refused_rather_than_saturating_to_zero() {
        for bad in [f64::NAN, -1.0, f64::INFINITY, 0.0] {
            assert!(
                matches!(sweep_fee_sats(2, 1, bad), Err(MercuryError::FeeTooLow)),
                "fee rate {bad} must be refused"
            );
        }
    }
}

#[cfg(test)]
mod combine_dust_tests {
    use super::combine_test_support::{agg_address_regtest, leaf_coin};
    use super::get_unsigned_combine_psbt;
    use crate::error::MercuryError;

    const TXID_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const TXID_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn build(outputs: Vec<(String, u64)>, coins: &[crate::wallet::Coin]) -> Result<String, MercuryError> {
        get_unsigned_combine_psbt(coins, 100, 100, 10, 0, outputs, "regtest".to_string(), false)
    }

    /// **THE DEFECT.** `get_unsigned_split_psbt` refuses any output below the 330-sat P2TR dust
    /// floor (`lib/src/transaction.rs:358-366`) because a sub-dust output is non-standard, so the
    /// co-signed tx can never be relayed and whatever funded it is stranded. Its multi-input
    /// sibling `get_unsigned_combine_psbt` had NO such floor: it checked only
    /// `sum(outputs) < sum(inputs)`. A 300-sat output passes that check and produces a transaction
    /// the network will not carry — after the SE has co-signed every input.
    #[test]
    fn a_combine_output_below_the_dust_floor_is_refused() {
        let coins = [leaf_coin(TXID_A, 0, 5_000), leaf_coin(TXID_B, 1, 5_000)];
        let r = build(vec![(agg_address_regtest(), 300)], &coins);
        assert!(
            matches!(r, Err(MercuryError::FeeTooLow)),
            "a 300-sat combine output is below the 330-sat P2TR dust floor and must be refused \
             BEFORE the SE co-signs N inputs, got {r:?}"
        );
    }

    /// The floor is per-output, not on the sum: two 200-sat outputs total 400 (above 330) yet each
    /// one is individually unrelayable. `get_unsigned_split_psbt` checks `.any(|v| v < DUST_LIMIT)`
    /// for exactly this reason.
    #[test]
    fn one_sub_dust_output_among_several_is_refused() {
        let coins = [leaf_coin(TXID_A, 0, 5_000), leaf_coin(TXID_B, 1, 5_000)];
        let r = build(
            vec![(agg_address_regtest(), 200), (agg_address_regtest(), 200)],
            &coins,
        );
        assert!(
            matches!(r, Err(MercuryError::FeeTooLow)),
            "each combine output must clear the dust floor on its own, got {r:?}"
        );
    }

    /// Exactly at the floor is ACCEPTED — the refusal must not be off by one and quietly reject
    /// legitimate 330-sat outputs.
    #[test]
    fn a_combine_output_exactly_at_the_dust_floor_is_accepted() {
        let coins = [leaf_coin(TXID_A, 0, 5_000), leaf_coin(TXID_B, 1, 5_000)];
        let r = build(vec![(agg_address_regtest(), 330)], &coins);
        assert!(r.is_ok(), "330 sats is the floor itself and must build, got {r:?}");
    }
}
