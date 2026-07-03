//! Mercury <-> RGB bridge.
//!
//! Thin wrapper around [`rgb_lib`] that lets the Mercury Layer statechain client issue RGB assets,
//! color the (externally built) statechain backup/withdrawal transactions and accept incoming
//! consignments - all while keeping RGB's `bitcoin`/`bdk` types isolated from Mercury's pinned
//! `bitcoin 0.30`. The only data crossing this boundary is strings: base64 PSBTs, hex txids and
//! base64 consignments.
//!
//! This bridge uses only the **public** rgb-lib API - the same special-usage methods that
//! rgb-lightning-node uses (`color_psbt_and_consume`, `post_consignment`, `accept_transfer`) - so
//! the rgb-lib dependency needs no source changes for coloring/accepting. The single statechain
//! addition in the rgb-lib fork is `fund_statechain_utxo` (deposit binding), which has no public
//! equivalent because it spends a colored UTXO to an externally-owned (statechain) address.
//!
//! Model (RGB-over-statechain): the asset stays bound to the **statechain UTXO** (the `Tx0`
//! outpoint) throughout the coin's off-chain life. Each Mercury transfer produces a *colored*
//! backup transaction whose RGB transition closes the seal on the statechain UTXO and re-assigns
//! the asset to the new owner's output. The transition only becomes on-chain-valid when a witness
//! transaction is broadcast: the cooperative withdrawal transaction, or - on a unilateral exit -
//! the latest backup transaction. This mirrors how RGB is used over Lightning commitment txs.

use std::collections::HashMap;
use std::fs;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rgb_lib::bitcoin::psbt::Psbt as RgbPsbt;
use rgb_lib::wallet::{
    rust_only::{check_indexer_url, AssetColoringInfo, ColoringInfo, IndexerProtocol},
    RgbWalletOpsOffline, RgbWalletOpsOnline, SinglesigKeys, TransportEndpoint, Wallet,
};
use rgb_lib::{
    generate_keys, restore_keys,
    keys::WitnessVersion,
    wallet::{DatabaseType, Online, OnlineOptions, WalletData},
    Assignment, AssetSchema, BitcoinNetwork, ContractId, FileContent, RgbTransport,
};

/// An rgb-lib wallet wired for use alongside a Mercury Layer wallet.
pub struct RgbWallet {
    wallet: Wallet,
    online: Online,
    indexer_url: String,
    /// RGB proxy transport endpoint, e.g. `rpc://127.0.0.1:3000/json-rpc`.
    proxy: String,
}

fn map_network(network: &str) -> Result<BitcoinNetwork> {
    Ok(match network.to_lowercase().as_str() {
        "mainnet" | "bitcoin" => BitcoinNetwork::Mainnet,
        "testnet" => BitcoinNetwork::Testnet,
        "signet" => BitcoinNetwork::Signet,
        "regtest" => BitcoinNetwork::Regtest,
        other => return Err(anyhow!("unsupported network: {other}")),
    })
}

fn unique_tmp(prefix: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), unique))
}

impl RgbWallet {
    /// Generate a fresh mnemonic for a new RGB wallet (taproot keychain).
    pub fn generate_mnemonic(network: &str) -> Result<String> {
        let keys = generate_keys(map_network(network)?, WitnessVersion::Taproot);
        Ok(keys.mnemonic)
    }

    /// Open (creating if needed) an rgb-lib wallet from a mnemonic and bring it online against the
    /// given electrum `indexer_url`. `proxy` is the RGB proxy transport endpoint
    /// (e.g. `rpc://127.0.0.1:3000/json-rpc`), used for consignment exchange.
    pub fn open(
        data_dir: &str,
        mnemonic: &str,
        network: &str,
        indexer_url: &str,
        proxy: &str,
    ) -> Result<Self> {
        let bitcoin_network = map_network(network)?;

        let keys = restore_keys(bitcoin_network, mnemonic.to_string(), WitnessVersion::Taproot)?;
        let wallet_keys = SinglesigKeys::from_keys(&keys, None);

        let wallet_data = WalletData {
            data_dir: data_dir.to_string(),
            bitcoin_network,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 1,
            supported_schemas: vec![
                AssetSchema::Nia,
                AssetSchema::Uda,
                AssetSchema::Cfa,
                AssetSchema::Ifa,
            ],
            reuse_addresses: false,
        };

        let mut wallet = Wallet::new(wallet_data, wallet_keys)?;

        // Validate the indexer before going online so misconfiguration fails early.
        let _: IndexerProtocol = check_indexer_url(indexer_url, bitcoin_network)?;

        let online = wallet.go_online(OnlineOptions {
            indexer_url: indexer_url.to_string(),
            skip_consistency_check: true,
            vanilla_sync_lookback: 0,
        })?;

        Ok(Self {
            wallet,
            online,
            indexer_url: indexer_url.to_string(),
            proxy: proxy.to_string(),
        })
    }

    /// A bitcoin address of the underlying RGB (BDK) wallet, used to fund it with sats so it can
    /// create the UTXOs needed to hold allocations and pay RGB witness fees.
    pub fn get_address(&mut self) -> Result<String> {
        Ok(self.wallet.get_address()?)
    }

    /// Create `num` colorable UTXOs of `size` sats each in the RGB wallet (needed before issuance).
    /// The size must be large enough to later fund the statechain deposit from the colored UTXO.
    pub fn create_utxos(&mut self, num: u8, size: u32, fee_rate: u64) -> Result<u8> {
        Ok(self.wallet.create_utxos(
            self.online.clone(),
            false,
            Some(num),
            Some(size),
            fee_rate,
            false,
        )?)
    }

    /// Issue a NIA (Non-Inflatable Asset) and return its contract/asset id.
    pub fn issue_nia(
        &self,
        ticker: &str,
        name: &str,
        precision: u8,
        amounts: Vec<u64>,
    ) -> Result<String> {
        let asset = self
            .wallet
            .issue_asset_nia(ticker.to_string(), name.to_string(), precision, amounts)?;
        Ok(asset.asset_id)
    }

    /// Settled balance of an asset in this wallet.
    pub fn settled_balance(&self, asset_id: &str) -> Result<u64> {
        Ok(self.wallet.get_asset_balance(asset_id.to_string())?.settled)
    }

    /// Full RGB balance of an asset: `(settled, future, spendable)`.
    pub fn balance(&self, asset_id: &str) -> Result<(u64, u64, u64)> {
        let b = self.wallet.get_asset_balance(asset_id.to_string())?;
        Ok((b.settled, b.future, b.spendable))
    }

    /// RGB allocations of an asset held on this wallet's UTXOs: `(outpoint, amount, settled)`.
    pub fn list_allocations(&mut self, asset_id: &str) -> Result<Vec<(String, u64, bool)>> {
        let unspents = self
            .wallet
            .list_unspents(Some(self.online.clone()), false, false)?;
        let mut out = vec![];
        for u in unspents {
            for a in &u.rgb_allocations {
                if a.asset_id.as_deref() == Some(asset_id) {
                    if let Assignment::Fungible(amt) = a.assignment {
                        out.push((
                            format!("{}:{}", u.utxo.outpoint.txid, u.utxo.outpoint.vout),
                            amt,
                            a.settled,
                        ));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Summary of this wallet's transfers for an asset: `(kind, status, amount, txid)`.
    pub fn transfers(&self, asset_id: &str) -> Result<Vec<(String, String, u64, String)>> {
        let transfers = self.wallet.list_transfers(Some(asset_id.to_string()))?;
        Ok(transfers
            .into_iter()
            .map(|t| {
                let amount: u64 = t
                    .assignments
                    .iter()
                    .map(|a| match a {
                        Assignment::Fungible(n) | Assignment::InflationRight(n) => *n,
                        _ => 0,
                    })
                    .sum();
                (
                    format!("{:?}", t.kind),
                    format!("{:?}", t.status),
                    amount,
                    t.txid.unwrap_or_default(),
                )
            })
            .collect())
    }

    /// Sync RGB state with the indexer (settle pending transfers, update witnesses).
    pub fn refresh(&mut self, asset_id: Option<String>) -> Result<()> {
        self.wallet
            .refresh(self.online.clone(), asset_id, vec![], false)?;
        Ok(())
    }

    /// Color a Mercury-built unsigned transaction (provided as a base64 PSBT) so that the RGB asset
    /// is re-assigned to the given pre-coloring output vouts, returning the modified PSBT (base64,
    /// with the OP_RETURN opret commitment added) and the consignment (base64) to relay to the
    /// receiver.
    ///
    /// Uses the public `color_psbt_and_consume` (no rgb-lib source change). `output_map` maps
    /// *pre-coloring* output index -> asset amount. `blinding` is the deterministic seal blinding
    /// (must be shared with the receiver so it can accept).
    ///
    /// Returns `(colored_psbt_base64, consignment_base64)`.
    pub fn color(
        &self,
        psbt_base64: &str,
        contract_id: &str,
        output_map: HashMap<u32, u64>,
        blinding: u64,
    ) -> Result<(String, String)> {
        let contract = ContractId::from_str(contract_id)
            .map_err(|e| anyhow!("invalid contract id: {e}"))?;
        let mut psbt = RgbPsbt::from_str(psbt_base64).map_err(|e| anyhow!("invalid psbt: {e}"))?;

        let coloring_info = ColoringInfo {
            asset_info_map: HashMap::from([(
                contract,
                AssetColoringInfo {
                    output_map,
                    blinded_map: HashMap::new(),
                    static_blinding: Some(blinding),
                },
            )]),
            static_blinding: Some(blinding),
            nonce: None,
        };

        let transfers = self.wallet.color_psbt_and_consume(&mut psbt, coloring_info)?;
        let transfer = transfers
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("color produced no consignment"))?;

        let dir = unique_tmp("mercury-rgb-color");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        transfer
            .save_file(&path)
            .map_err(|e| anyhow!("could not serialize consignment: {e}"))?;
        let bytes = fs::read(&path)?;
        let _ = fs::remove_dir_all(&dir);

        Ok((psbt.to_string(), STANDARD.encode(bytes)))
    }

    /// Color a PSBT assigning to **blinded** beneficiaries (existing outpoints) rather than witness
    /// vouts: `blinded` is a list of `(recipient_id, amount)` where each `recipient_id` is a
    /// `blind_receive` blinded seal (e.g. on a statechain UTXO), enabling a statechain->statechain
    /// transfer where the receiver receives on its own statechain UTXO and change goes to a free
    /// statechain UTXO. Uses the public `color_psbt_and_consume` + the fork's `blinded_map`.
    /// Returns `(colored_psbt_base64, consignment_base64)` - one consignment covering all beneficiaries.
    pub fn color_blinded(
        &self,
        psbt_base64: &str,
        contract_id: &str,
        blinded: Vec<(String, u64)>,
        blinding: u64,
    ) -> Result<(String, String)> {
        let contract = ContractId::from_str(contract_id)
            .map_err(|e| anyhow!("invalid contract id: {e}"))?;
        let mut psbt = RgbPsbt::from_str(psbt_base64).map_err(|e| anyhow!("invalid psbt: {e}"))?;
        let blinded_map: HashMap<String, u64> = blinded.into_iter().collect();
        let coloring_info = ColoringInfo {
            asset_info_map: HashMap::from([(
                contract,
                AssetColoringInfo {
                    output_map: HashMap::new(),
                    blinded_map,
                    static_blinding: Some(blinding),
                },
            )]),
            static_blinding: Some(blinding),
            nonce: None,
        };
        let transfers = self.wallet.color_psbt_and_consume(&mut psbt, coloring_info)?;
        let transfer = transfers
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("color produced no consignment"))?;
        let dir = unique_tmp("mercury-rgb-color-blinded");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        transfer
            .save_file(&path)
            .map_err(|e| anyhow!("could not serialize consignment: {e}"))?;
        let bytes = fs::read(&path)?;
        let _ = fs::remove_dir_all(&dir);
        Ok((psbt.to_string(), STANDARD.encode(bytes)))
    }

    /// Validate a consignment (base64) **off-chain** — before its witness (exit) transaction is
    /// broadcast — using the witness bundled inside the consignment (rgb-lib's `OffchainResolver`).
    /// `txid` is the witness/exit transaction id. This is how a receiver accepts an off-chain P2P
    /// statechain transfer: it checks that the sender's *unbroadcast* exit tx commits the RGB
    /// transition assigning the asset to the receiver's seal, without anything touching Bitcoin.
    /// Returns `(valid, detail)` where `detail` is a warning summary when valid or the failure reason.
    pub fn validate_offchain(&self, consignment_base64: &str, txid: &str) -> Result<(bool, Option<String>)> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-validate");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let network = self.wallet.get_wallet_data().bitcoin_network;
        let res = rgb_lib::wallet::rust_only::validate_consignment_offchain(
            path.to_str().ok_or_else(|| anyhow!("bad temp path"))?,
            txid,
            &self.indexer_url,
            network,
        )?;
        let _ = fs::remove_dir_all(&dir);
        let detail = if res.valid {
            res.warnings.map(|w| w.join("; "))
        } else {
            res.details.or(res.error)
        };
        Ok((res.valid, detail))
    }

    /// Validate a consignment **off-chain** whose witness is a CHAIN of un-broadcast transactions —
    /// a multi-level off-chain split (Stage 2 of `docs/rgb_offchain_split_spilman.md`). `txids` is the
    /// branch from the on-chain root down to the leaf (e.g. `[S1, S2]`); every one is resolved from
    /// the consignment's bundled witnesses, so an un-broadcast *parent* need not be on-chain (unlike
    /// [`Self::validate_offchain`], which only treats the single terminal txid as offchain).
    pub fn validate_offchain_chain(
        &self,
        consignment_base64: &str,
        txids: &[String],
    ) -> Result<(bool, Option<String>)> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-validate-chain");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let network = self.wallet.get_wallet_data().bitcoin_network;
        let res = rgb_lib::wallet::rust_only::validate_consignment_offchain_chain(
            path.to_str().ok_or_else(|| anyhow!("bad temp path"))?,
            txids,
            &self.indexer_url,
            network,
        )?;
        let _ = fs::remove_dir_all(&dir);
        let detail = if res.valid {
            res.warnings.map(|w| w.join("; "))
        } else {
            res.details.or(res.error)
        };
        Ok((res.valid, detail))
    }

    /// Validate and accept an incoming consignment (base64) relayed in-band by the Mercury transfer
    /// message. The bytes are re-posted to the local RGB proxy and validated via the public
    /// `accept_transfer` (no rgb-lib source change). `txid`/`vout` identify the witness-tx output
    /// that will hold the asset and `blinding` is the seal blinding used by the sender.
    ///
    /// Returns the total fungible amount assigned to this wallet by the consignment.
    pub fn accept(
        &mut self,
        consignment_base64: &str,
        txid: &str,
        vout: u32,
        blinding: u64,
    ) -> Result<u64> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;

        let dir = unique_tmp("mercury-rgb-accept");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;

        // Re-post the in-band consignment to the proxy under recipient_id = txid, so the public
        // accept_transfer (which fetches by txid) validates exactly these bytes.
        let http_endpoint = TransportEndpoint::new(self.proxy.clone())?.endpoint;
        self.wallet.post_consignment(
            &http_endpoint,
            txid.to_string(),
            &path,
            txid.to_string(),
            Some(vout),
        )?;
        let _ = fs::remove_dir_all(&dir);

        let transport =
            RgbTransport::from_str(&self.proxy).map_err(|e| anyhow!("invalid proxy endpoint: {e}"))?;
        let (_transfer, assignments) =
            self.wallet
                .accept_transfer(txid.to_string(), vout, transport, blinding)?;

        let received: u64 = assignments
            .into_iter()
            .map(|a| match a {
                Assignment::Fungible(amt) | Assignment::InflationRight(amt) => amt,
                Assignment::NonFungible | Assignment::Any => 0,
            })
            .sum();

        Ok(received)
    }

    /// Create a statechain UTXO of `size_sat` sats and deposit the wallet's **free allocation** of
    /// `contract_id` onto it - the statechain analogue of rgb-lib's `create_utxos` (which creates
    /// colorable UTXOs sized in sats). The deposited asset amount is the wallet's spendable balance
    /// (not a passed rgb-amount); after this, `get_asset_balance(contract_id)` drops to 0.
    ///
    /// Returns `(txid, vout)` of the statechain UTXO.
    pub fn create_statechain_utxo(
        &mut self,
        statechain_address: &str,
        size_sat: u64,
        contract_id: &str,
        fee_rate: u64,
        blinding: u64,
    ) -> Result<(String, u32)> {
        let free = self.wallet.get_asset_balance(contract_id.to_string())?.spendable;
        if free == 0 {
            return Err(anyhow!("no free allocation of {contract_id} to deposit"));
        }
        // Deposit the full free allocation onto a statechain UTXO of `size_sat` sats.
        self.deposit_via_send(statechain_address, size_sat, contract_id, free, fee_rate, blinding)
    }

    /// Color-based deposit that keeps the statechain UTXO **re-colorable** by the owner (so it can
    /// later transfer/exit), unlike `create_statechain_utxo`/`deposit_via_send` which mark the asset
    /// as sent away. Returns `(txid, vout, consignment_base64, signed_tx_hex)`; the caller broadcasts
    /// `signed_tx_hex`.
    pub fn fund_statechain(
        &mut self,
        address: &str,
        amount_sat: u64,
        contract_id: &str,
        rgb_amount: u64,
        fee_rate: u64,
        blinding: u64,
    ) -> Result<(String, u32, String, String)> {
        let _ = self
            .wallet
            .list_unspents(Some(self.online.clone()), false, false)?;
        let (txid, vout, consignment, signed_tx_hex) = self.wallet.fund_statechain_utxo(
            address.to_string(),
            amount_sat,
            contract_id.to_string(),
            rgb_amount,
            fee_rate,
            blinding,
        )?;
        Ok((txid, vout, STANDARD.encode(consignment), signed_tx_hex))
    }

    /// Register a Mercury statechain UTXO as a wallet-owned colorable UTXO, so standard rgb-lib
    /// methods (`get_asset_balance`, `list_unspents`, `blind_receive`) treat it exactly like an
    /// on-chain UTXO (design-doc item 1). `rgb_amount == 0` registers a *free* coin (onboarding: a
    /// statechain UTXO to receive on). Any allocation must already be in the stash (e.g. it was put
    /// there by `fund_statechain` on this wallet). Returns the txo idx.
    pub fn register_statechain(
        &self,
        txid: &str,
        vout: u32,
        btc_amount: u64,
        contract_id: &str,
        rgb_amount: u64,
        spend_outpoints: &[String],
    ) -> Result<i32> {
        use rgb_lib::wallet::Outpoint;
        let spend = spend_outpoints
            .iter()
            .filter_map(|s| {
                let (t, v) = s.rsplit_once(':')?;
                Some(Outpoint { txid: t.to_string(), vout: v.parse().ok()? })
            })
            .collect();
        Ok(self.wallet.register_statechain_utxo(
            txid.to_string(),
            vout,
            btc_amount,
            contract_id.to_string(),
            rgb_amount,
            spend,
        )?)
    }

    /// Mark registered statechain UTXO(s) (`txid:vout`) as spent in the DB - call after a transfer
    /// consumes a statechain UTXO so `get_asset_balance` drops to just the change.
    pub fn mark_spent(&self, outpoints: &[String]) -> Result<()> {
        use rgb_lib::wallet::Outpoint;
        let ops = outpoints
            .iter()
            .filter_map(|s| {
                let (t, v) = s.rsplit_once(':')?;
                Some(Outpoint { txid: t.to_string(), vout: v.parse().ok()? })
            })
            .collect();
        Ok(self.wallet.mark_utxos_spent(ops)?)
    }

    /// Standard rgb-lib **blinded receive** invoice for `amount`. The seal is one of this wallet's
    /// free colorable UTXOs - which, after `register_statechain`, can be a statechain UTXO. This is
    /// the user's "receiver creates an rgb invoice whose recipient_id references its statechain UTXO".
    /// Returns the `recipient_id` (the blinded seal the sender assigns to).
    pub fn blind_receive(&mut self, asset_id: Option<&str>, amount: u64) -> Result<String> {
        let rd = self.wallet.blind_receive(
            asset_id.map(|s| s.to_string()),
            Assignment::Fungible(amount),
            None,
            vec![self.proxy.clone()],
            0,
        )?;
        Ok(rd.recipient_id)
    }

    /// Dump this wallet's unspents as `(outpoint, btc_amount, colorable, exists, n_allocations,
    /// pending_blinded)` - so a test can show that a registered statechain UTXO is treated as a
    /// colorable on-chain UTXO (and, after `blind_receive`, carries a pending blinded seal).
    pub fn unspents_dump(&mut self) -> Result<Vec<(String, u64, bool, bool, usize, u32)>> {
        let unspents = self
            .wallet
            .list_unspents(Some(self.online.clone()), false, false)?;
        Ok(unspents
            .into_iter()
            .map(|u| {
                (
                    format!("{}:{}", u.utxo.outpoint.txid, u.utxo.outpoint.vout),
                    u.utxo.btc_amount,
                    u.utxo.colorable,
                    u.utxo.exists,
                    u.rgb_allocations.len(),
                    u.pending_blinded,
                )
            })
            .collect())
    }

    /// Deposit using rgb-lib's **standard** `send`: pay `amount_sat` to the statechain `address` as
    /// an RGB *witness* recipient assigned `rgb_amount` of `contract_id`. Because this goes through
    /// rgb-lib's normal transfer flow, all standard methods reflect it afterwards: the issuer's
    /// `balance` drops, `transfers` shows a `Send`, and `list_unspents` shows the spent colored UTXO
    /// plus a new change UTXO (carrying any asset change). The witness output that becomes the
    /// statechain UTXO is at vout 1 (rgb-lib places the OP_RETURN commitment at vout 0). `send`
    /// broadcasts the funding tx itself. Returns `(txid, vout)`.
    pub fn deposit_via_send(
        &mut self,
        address: &str,
        amount_sat: u64,
        contract_id: &str,
        rgb_amount: u64,
        fee_rate: u64,
        blinding: u64,
    ) -> Result<(String, u32)> {
        use rgb_lib::bitcoin::Address as BdkAddress;
        use rgb_lib::utils::recipient_id_from_script_buf;
        use rgb_lib::wallet::{Recipient, WitnessData};

        let network = self.wallet.get_wallet_data().bitcoin_network;
        let addr = BdkAddress::from_str(address)
            .map_err(|e| anyhow!("invalid statechain address: {e}"))?
            .assume_checked();
        let recipient_id = recipient_id_from_script_buf(addr.script_pubkey(), network);

        let recipient = Recipient {
            recipient_id,
            witness_data: Some(WitnessData {
                amount_sat,
                blinding: Some(blinding),
            }),
            assignment: Assignment::Fungible(rgb_amount),
            transport_endpoints: vec![self.proxy.clone()],
        };
        let recipient_map = HashMap::from([(contract_id.to_string(), vec![recipient])]);

        let res = self.wallet.send(
            self.online.clone(),
            recipient_map,
            true, // donation: broadcast without recipient ACK (the statechain UTXO is external)
            fee_rate,
            1,
            None,
            None,
        )?;
        Ok((res.txid, 1))
    }

    /// Generate a standard rgb-lib **witness receive** invoice for `amount` (of any asset - the
    /// contract is imported from the consignment on refresh). Registers a pending incoming transfer
    /// and reserves the wallet's next address; returns the `recipient_id` (which encodes that
    /// address). Used for the cooperative exit so the receiver settles via the normal `refresh` flow.
    pub fn witness_receive(&mut self, amount: u64) -> Result<String> {
        let rd = self.wallet.witness_receive(
            None,
            Assignment::Fungible(amount),
            None,
            vec![self.proxy.clone()],
            1,
        )?;
        Ok(rd.recipient_id)
    }

    /// The bitcoin address that a witness `recipient_id` will be paid at (so the externally-built
    /// colored withdrawal tx can pay it).
    pub fn address_from_recipient_id(&self, recipient_id: &str) -> Result<String> {
        use rgb_lib::bitcoin::{Address, Network};
        use rgb_lib::utils::script_buf_from_recipient_id;

        let script = script_buf_from_recipient_id(recipient_id.to_string())?
            .ok_or_else(|| anyhow!("recipient_id is not a witness recipient"))?;
        let net = match self.wallet.get_wallet_data().bitcoin_network {
            BitcoinNetwork::Mainnet => Network::Bitcoin,
            BitcoinNetwork::Regtest => Network::Regtest,
            BitcoinNetwork::Signet | BitcoinNetwork::SignetCustom => Network::Signet,
            _ => Network::Testnet, // Testnet/Testnet4 share the testnet address format
        };
        let addr = Address::from_script(&script, net)
            .map_err(|e| anyhow!("could not derive address from recipient_id: {e}"))?;
        Ok(addr.to_string())
    }

    /// Post a consignment (base64) to the RGB proxy under `recipient_id`, so the receiver's standard
    /// `refresh` can fetch and settle it. `txid`/`vout` identify the witness output.
    pub fn post_consignment(
        &self,
        recipient_id: &str,
        consignment_base64: &str,
        txid: &str,
        vout: u32,
    ) -> Result<()> {
        let bytes = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;
        let dir = unique_tmp("mercury-rgb-post");
        fs::create_dir_all(&dir)?;
        let path = dir.join("consignment");
        fs::write(&path, &bytes)?;
        let http_endpoint = TransportEndpoint::new(self.proxy.clone())?.endpoint;
        self.wallet.post_consignment(
            &http_endpoint,
            recipient_id.to_string(),
            &path,
            txid.to_string(),
            Some(vout),
        )?;
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    pub fn indexer_url(&self) -> &str {
        &self.indexer_url
    }
}
