//! Mercury <-> RGB bridge.
//!
//! Thin wrapper around [`rgb_lib`] that lets the Mercury Layer statechain client issue RGB assets,
//! color the (externally built) statechain backup/withdrawal transactions and accept incoming
//! consignments - all while keeping RGB's `bitcoin`/`bdk` types isolated from Mercury's pinned
//! `bitcoin 0.30`. The only data crossing this boundary is strings: base64 PSBTs, hex txids and
//! base64 consignments.
//!
//! Model (RGB-over-statechain): the asset stays bound to the **statechain UTXO** (the `Tx0`
//! outpoint) throughout the coin's off-chain life. Each Mercury transfer produces a *colored*
//! backup transaction whose RGB transition closes the seal on the statechain UTXO and re-assigns
//! the asset to the new owner's output. The transition only becomes on-chain-valid when a witness
//! transaction is broadcast: the cooperative withdrawal transaction, or - on a unilateral exit -
//! the latest backup transaction. This mirrors how RGB is used over Lightning commitment txs.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rgb_lib::wallet::{
    rust_only::{check_indexer_url, IndexerProtocol},
    RgbWalletOpsOffline, RgbWalletOpsOnline, SinglesigKeys, Wallet,
};
use rgb_lib::{
    generate_keys, restore_keys,
    keys::WitnessVersion,
    wallet::{DatabaseType, Online, OnlineOptions, WalletData},
    Assignment, BitcoinNetwork,
};

/// An rgb-lib wallet wired for use alongside a Mercury Layer wallet.
pub struct RgbWallet {
    wallet: Wallet,
    online: Online,
    indexer_url: String,
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

impl RgbWallet {
    /// Generate a fresh mnemonic for a new RGB wallet (taproot keychain).
    pub fn generate_mnemonic(network: &str) -> Result<String> {
        let keys = generate_keys(map_network(network)?, WitnessVersion::Taproot);
        Ok(keys.mnemonic)
    }

    /// Open (creating if needed) an rgb-lib wallet from a mnemonic and bring it online against the
    /// given electrum `indexer_url`.
    pub fn open(
        data_dir: &str,
        mnemonic: &str,
        network: &str,
        indexer_url: &str,
    ) -> Result<Self> {
        let bitcoin_network = map_network(network)?;

        let keys = restore_keys(bitcoin_network, mnemonic.to_string(), WitnessVersion::Taproot)?;
        let wallet_keys = SinglesigKeys::from_keys(&keys, None);

        let wallet_data = WalletData {
            data_dir: data_dir.to_string(),
            bitcoin_network,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 1,
            // Empty list means "support all schemas rgb-lib supports".
            supported_schemas: vec![],
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
        })
    }

    /// A bitcoin address of the underlying RGB (BDK) wallet, used to fund it with sats so it can
    /// create the UTXOs needed to hold allocations and pay RGB witness fees.
    pub fn get_address(&mut self) -> Result<String> {
        Ok(self.wallet.get_address()?)
    }

    /// Create UTXOs in the RGB wallet (needed before issuance).
    pub fn create_utxos(&mut self, fee_rate: u64) -> Result<u8> {
        Ok(self
            .wallet
            .create_utxos(self.online.clone(), false, None, None, fee_rate, false)?)
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
    /// `output_map` maps *pre-coloring* output index -> asset amount. `blinding` is the
    /// deterministic seal blinding (must be shared with the receiver so it can accept).
    ///
    /// Returns `(colored_psbt_base64, consignment_base64)`.
    pub fn color(
        &self,
        psbt_base64: &str,
        contract_id: &str,
        output_map: HashMap<u32, u64>,
        blinding: u64,
    ) -> Result<(String, String)> {
        let (colored_psbt, consignments) = self.wallet.color_statechain_psbt(
            psbt_base64.to_string(),
            contract_id.to_string(),
            output_map,
            blinding,
        )?;

        let consignment = consignments
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("color produced no consignment"))?;

        Ok((colored_psbt, STANDARD.encode(consignment)))
    }

    /// Validate and accept an incoming consignment (base64) relayed in-band by the Mercury transfer
    /// message. `txid`/`vout` identify the witness-tx output that will hold the asset and `blinding`
    /// is the seal blinding used by the sender while coloring.
    ///
    /// Returns the total fungible amount assigned to this wallet by the consignment.
    pub fn accept(
        &mut self,
        consignment_base64: &str,
        txid: &str,
        vout: u32,
        blinding: u64,
    ) -> Result<u64> {
        let consignment = STANDARD
            .decode(consignment_base64)
            .map_err(|e| anyhow!("invalid consignment base64: {e}"))?;

        let (_transfer, assignments) =
            self.wallet
                .accept_consignment(consignment, txid.to_string(), vout, blinding)?;

        let received: u64 = assignments
            .into_iter()
            .map(|a| match a {
                Assignment::Fungible(amt) | Assignment::InflationRight(amt) => amt,
                Assignment::NonFungible | Assignment::Any => 0,
            })
            .sum();

        Ok(received)
    }

    /// Deposit binding: build, color and sign the funding transaction that pays `amount_sat` to the
    /// statechain aggregated `address` and assigns `rgb_amount` of `contract_id` to that output.
    ///
    /// Returns `(txid, vout, consignment_base64, signed_tx_hex)`. The caller broadcasts `signed_tx_hex`
    /// (via the Mercury electrum client); once it confirms, the statechain UTXO `txid:vout` holds both
    /// the bitcoins and the RGB allocation, and `consignment_base64` is the genesis-to-deposit proof.
    pub fn fund_statechain(
        &mut self,
        address: &str,
        amount_sat: u64,
        contract_id: &str,
        rgb_amount: u64,
        fee_rate: u64,
        blinding: u64,
    ) -> Result<(String, u32, String, String)> {
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

    /// Find the UTXO in this wallet that holds an allocation of `contract_id`, returning
    /// `(txid, vout, btc_amount)`. Used at deposit time to build the funding transaction that binds
    /// the asset to the statechain UTXO (the funding tx spends this UTXO and pays the statechain
    /// aggregated address).
    pub fn asset_utxo(&mut self, contract_id: &str) -> Result<Option<(String, u32, u64)>> {
        let unspents = self
            .wallet
            .list_unspents(Some(self.online.clone()), false, false)?;
        for u in unspents {
            for a in &u.rgb_allocations {
                if a.asset_id.as_deref() == Some(contract_id) {
                    if let Assignment::Fungible(amt) = a.assignment {
                        if amt > 0 {
                            return Ok(Some((
                                u.utxo.outpoint.txid.clone(),
                                u.utxo.outpoint.vout,
                                u.utxo.btc_amount,
                            )));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// Sign a PSBT (base64) with this wallet's keys, returning the signed PSBT (base64). Used to
    /// sign the deposit funding transaction (whose input is one of this wallet's own UTXOs).
    pub fn sign(&self, psbt_base64: &str) -> Result<String> {
        Ok(self.wallet.sign_psbt(psbt_base64.to_string(), None)?)
    }

    pub fn indexer_url(&self) -> &str {
        &self.indexer_url
    }
}
