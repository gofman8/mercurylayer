
use crate::utils::create_activity;
use std::str::FromStr;

use bitcoin::Address;
use electrum_client::{ElectrumApi, ListUnspentRes};
use mercurylib::{utils::is_enclave_pubkey_part_of_coin, wallet::{Activity, Coin, CoinStatus}};
use anyhow::{anyhow, Result, Ok};

use crate::{client_config::ClientConfig, sqlite_manager::{get_wallet, update_wallet}};

/// **Who establishes a fresh deposit's exit ladder, and when.**
///
/// A coin's ONLY exit material is its TES-R ladder (`T` → `X_0` → `S_0`, all co-signed in advance,
/// none broadcast). There is no flat absolute-locktime backup any more: the ladder is signed the
/// moment the funding transaction is first seen in the mempool, in place of the `tx1` that used to
/// be signed there — the trigger needs nothing but the funding outpoint, its value and the aggregate
/// key, all of which are known at first sight, and the coordinator never gates a co-sign on
/// confirmation. So the depositor's exit is `T` from the first moment, and the coin carries no
/// absolute calendar: nothing in its exit material ever matures on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LadderAtSight {
    /// Establish a PLAIN ladder inside [`update_coins_ex`], at first sight. The default for callers
    /// that have no RGB engine (the CLI, the legacy E2E suites): a plain deposit gets its ladder here
    /// and nowhere else.
    Plain,
    /// Establish nothing here. The caller (the SDK's `claim()`) runs its own establish pass over
    /// every un-laddered `IN_MEMPOOL` / `UNCONFIRMED` / `CONFIRMED` coin immediately after this
    /// call, choosing a plain or a COLOURED ladder from the coin's booked RGB allocations. That pass
    /// is inside the same `claim()` call, so the window in which a coin has no exit is the same one
    /// the flat `tx1` used to have: the deposit is seen and laddered in one pass.
    Defer,
}

struct DepositResult {
    activity: Activity,
    /// The ladder established at first sight under [`LadderAtSight::Plain`]. `None` under
    /// [`LadderAtSight::Defer`] and for `single_use` coins (the SE refuses a second co-sign on those,
    /// so a three-tier ladder is impossible; they never had exit material of their own).
    ladder: Option<crate::tesr::TesrBundle>,
}

async fn check_deposit(
    client_config: &ClientConfig,
    coin: &mut Coin,
    wallet_netwotk: &str,
    wallet_name: &str,
    ladder_at_sight: LadderAtSight,
) -> Result<Option<DepositResult>> {

    if coin.statechain_id.is_none() && coin.utxo_txid.is_none() && coin.utxo_vout.is_none() {
        if coin.status != CoinStatus::INITIALISED {
            return Err(anyhow!("Coin does not have a statechain ID, a UTXO and the status is not INITIALISED"));
        } else {
            return Ok(None);
        }
    }

    let mut utxo: Option<ListUnspentRes> = None;

    let address = Address::from_str(&coin.aggregated_address.as_ref().unwrap())?.require_network(client_config.network)?;

    let utxo_list =  client_config.electrum_client.script_list_unspent(&address.script_pubkey())?;

    for unspent in utxo_list {
        if unspent.value == coin.amount.unwrap() as u64 {
            utxo = Some(unspent);
            break;
        }
    }

    // No deposit found. No change in the coin status
    if utxo.is_none() {
        return Ok(None);
        // return Err(anyhow!("There is no UTXO with the address {} and the amount {}", coin.aggregated_address.as_ref().unwrap(), coin.amount.unwrap()));
    }

    let utxo = utxo.unwrap();

    // Coin amounts are stored as u32 sats (SPEC §14 "amount width"). Refuse to book a deposit whose
    // value would silently truncate on the `as u32` cast below, rather than mis-accounting it.
    if utxo.value > u32::MAX as u64 {
        return Err(anyhow!(
            "deposit UTXO value {} sats exceeds the u32 coin-amount ceiling (~42.9 BTC); refusing to book a truncated amount",
            utxo.value
        ));
    }

    // IN_MEMPOOL. there is nothing to do
    if utxo.height == 0 && coin.status == CoinStatus::IN_MEMPOOL {
        return Ok(None);
    }

    let block_header = client_config.electrum_client.block_headers_subscribe_raw()?;
    let blockheight = block_header.height;

    let mut deposit_result: Option<DepositResult> = None;

    if coin.status == CoinStatus::INITIALISED {
        let utxo_txid = utxo.tx_hash.to_string();
        let utxo_vout = utxo.tx_pos as u32;

        if coin.status != CoinStatus::INITIALISED {
            return Err(anyhow!("The coin with the public key {} is not in the INITIALISED state", coin.user_pubkey.to_string()));
        }

        coin.utxo_txid = Some(utxo_txid.to_string());
        coin.utxo_vout = Some(utxo_vout);

        coin.status = CoinStatus::IN_MEMPOOL;

        // THE EXIT LADDER, AT FIRST SIGHT. This is the moment the flat `tx1` used to be co-signed;
        // the ladder takes its place. Nothing here waits for a confirmation: `establish_auto` reads
        // only the funding outpoint, its value and the aggregate address, and the coordinator's
        // sign endpoints never look at the chain.
        //
        // Idempotent by construction: a ladder row already on disk for this sid (a previous pass
        // that established it and then failed to write the wallet record) is adopted rather than
        // re-established — a second establishment would spend three more irreversible co-signs and
        // leave the census permanently unbalanced.
        //
        // `single_use` coins get no ladder, as they got no `tx1`: the SE refuses any second co-sign
        // on such a coin, so a three-tier ladder cannot exist over it.
        let ladder = match ladder_at_sight {
            LadderAtSight::Defer => None,
            LadderAtSight::Plain if coin.single_use => None,
            LadderAtSight::Plain => {
                let statechain_id = coin
                    .statechain_id
                    .clone()
                    .ok_or_else(|| anyhow!("deposit seen for a coin with no statechain id"))?;
                match crate::tesr::load(client_config, wallet_name, &statechain_id).await? {
                    Some(existing) => Some(existing),
                    None => {
                        let payee = coin.backup_address.clone();
                        match crate::tesr::establish_auto(client_config, coin, &payee, wallet_netwotk).await {
                            std::result::Result::Ok(bundle) => Some(bundle),
                            Err(e) => {
                                // FAIL CLOSED, AND LEAVE THE COIN WHERE IT WAS. A deposit that is
                                // visible but has no ladder is a coin with NO EXIT; it must not be
                                // booked as IN_MEMPOOL (which every liveness allowlist reads as
                                // "ours to defend") until the ladder exists. The next pass sees the
                                // same UTXO and tries again.
                                coin.utxo_txid = None;
                                coin.utxo_vout = None;
                                coin.status = CoinStatus::INITIALISED;
                                return Err(anyhow!(
                                    "deposit {}:{} for statechain id {statechain_id} was seen in the \
                                     mempool but its exit ladder could not be established ({e}). The \
                                     coin is NOT booked until it has a ladder — a deposit without one \
                                     has no exit material at all. It will be retried on the next pass.",
                                    utxo.tx_hash, utxo.tx_pos
                                ));
                            }
                        }
                    }
                }
            }
        };

        let activity_utxo = format!("{}:{}", utxo.tx_hash.to_string(), utxo.tx_pos);

        let activity = Some(create_activity(&activity_utxo, utxo.value as u32, "deposit"));

        deposit_result = Some(DepositResult {
            activity: activity.unwrap(),
            ladder,
        });
    }

    if utxo.height > 0 {

        let confirmations = blockheight - utxo.height + 1;

        coin.status = CoinStatus::UNCONFIRMED;

        if confirmations as u32 >= client_config.confirmation_target {
            coin.status = CoinStatus::CONFIRMED;
        }
    }

    Ok(deposit_result)
}

async fn check_transfer(client_config: &ClientConfig, coin: &Coin) -> Result<bool> {

    if coin.statechain_id.is_none() {
        return Err(anyhow!("Coin does not have a statechain ID"));
    }

    let statechain_id = coin.statechain_id.as_ref().unwrap();

    let statechain_info = crate::utils::get_statechain_info(statechain_id, &client_config).await?;

    // if the statechain info is not found, we assume the coin has been transferred
    if statechain_info.is_none() {
        return Ok(true);
    }

    let statechain_info = statechain_info.unwrap();

    let enclave_public_key = statechain_info.enclave_public_key;

    // if the enclave's public key is no longer part of the coin, the coin has been transferred
    let is_transferred = !is_enclave_pubkey_part_of_coin(&coin, &enclave_public_key)?;

    return Ok(is_transferred);
}

async fn check_withdrawal(client_config: &ClientConfig, coin: &mut Coin) -> Result<()> {

    let mut txid: Option<String> = None;

    if coin.tx_withdraw.is_some() {
        txid = Some(coin.tx_withdraw.as_ref().unwrap().to_string());
    }

    if coin.tx_cpfp.is_some() {
        if txid.is_some() {
            return Err(anyhow!("Coin has both tx_withdraw and tx_cpfp"));
        }
        txid = Some(coin.tx_cpfp.as_ref().unwrap().to_string());
    }

    if txid.is_none() {
        // A coin can be WITHDRAWING because of a UNILATERAL exit rather than a cooperative one — an
        // in-ladder split child is routed that way, since its funding `SP.out[j]` is un-broadcast and
        // there is no confirmed outpoint for a cooperative withdraw to spend. Such a coin has neither
        // a withdrawal tx NOR a withdrawal address: its progress is tracked by the pre-signed exit
        // chain, not by watching one txid. Nothing to check here — treating it as an error made every
        // subsequent status poll fail for the life of the coin.
        if coin.withdrawal_address.is_none() {
            return Ok(());
        }
        return Err(anyhow!("Coin does not have tx_withdraw or tx_cpfp"));
    }

    let txid = txid.unwrap();

    if coin.withdrawal_address.is_none() {
        return Err(anyhow!("Coin does not have withdrawal_address"));
    }

    let address = Address::from_str(&coin.withdrawal_address.as_ref().unwrap())?.require_network(client_config.network)?;

    let utxo_list =  client_config.electrum_client.script_list_unspent(&address.script_pubkey())?;

    let mut utxo: Option<ListUnspentRes> = None;

    for unspent in utxo_list {
        if unspent.tx_hash.to_string() == txid {
            utxo = Some(unspent);
            break;
        }
    }

    if utxo.is_none() {
        // sometimes the transaction has not yet been transmitted to the specified Electrum server
        // return Err(anyhow!("There is no UTXO with the address {} and the txid {}", coin.withdrawal_address.as_ref().unwrap(), txid));
        return Ok(());
    }

    let utxo = utxo.unwrap();

    if utxo.height > 0 {

        let block_header = client_config.electrum_client.block_headers_subscribe_raw()?;
        let blockheight = block_header.height;

        let confirmations = blockheight - utxo.height + 1;

        if confirmations as u32 >= client_config.confirmation_target {
            coin.status = CoinStatus::WITHDRAWN;
        }
    }


    Ok(())
}

async fn check_for_duplicated(client_config: &ClientConfig, existing_coins: &Vec<Coin>) -> Result<Vec<Coin>>{

    let mut duplicated_coin_list : Vec<Coin> = Vec::new();

    for coin in existing_coins.iter() {

        if coin.status != CoinStatus::IN_MEMPOOL && coin.status != CoinStatus::UNCONFIRMED && coin.status != CoinStatus::CONFIRMED {
            continue;
        }

        let address = Address::from_str(&coin.aggregated_address.as_ref().unwrap())?.require_network(client_config.network)?;

        let utxo_list =  client_config.electrum_client.script_list_unspent(&address.script_pubkey())?;

        let mut max_duplicated_index = existing_coins.iter()
            .filter(|c|  c.statechain_id == coin.statechain_id )
            .map(|coin| coin.duplicate_index)
            .max()
            .unwrap();

        for unspent in utxo_list {

            let utxo_exists = existing_coins.iter().any(|coin| {
                coin.utxo_txid == Some(unspent.tx_hash.to_string()) &&
                coin.utxo_vout == Some(unspent.tx_pos as u32)
            });

            if utxo_exists {
                continue;
            }

            // u32 amount ceiling (SPEC §14): skip an oversized duplicate rather than truncate it.
            if unspent.value > u32::MAX as u64 {
                println!(
                    "skipping duplicate UTXO {}:{} — value {} sats exceeds the u32 coin-amount ceiling",
                    unspent.tx_hash, unspent.tx_pos, unspent.value
                );
                continue;
            }

            max_duplicated_index = max_duplicated_index + 1;

            let mut duplicated_coin = coin.clone();
            duplicated_coin.status = CoinStatus::DUPLICATED;
            duplicated_coin.utxo_txid = Some(unspent.tx_hash.to_string());
            duplicated_coin.utxo_vout = Some(unspent.tx_pos as u32);
            duplicated_coin.amount = Some(unspent.value as u32);
            duplicated_coin.duplicate_index = max_duplicated_index;
            duplicated_coin_list.push(duplicated_coin);
        }
    }

    Ok(duplicated_coin_list)

}

/// [`update_coins_ex`] with [`LadderAtSight::Plain`]: every fresh deposit is laddered here, at first
/// sight, with a PLAIN ladder. Callers with an RGB engine (the SDK) use [`update_coins_ex`] with
/// [`LadderAtSight::Defer`] and ladder the coin themselves, plain or coloured, in the same pass.
pub async fn update_coins(client_config: &ClientConfig, wallet_name: &str) -> Result<()> {
    update_coins_ex(client_config, wallet_name, LadderAtSight::Plain).await
}

pub async fn update_coins_ex(client_config: &ClientConfig, wallet_name: &str, ladder_at_sight: LadderAtSight) -> Result<()> {

    let mut wallet: mercurylib::wallet::Wallet = get_wallet(&client_config.pool, &wallet_name).await?;

    let network = wallet.network.clone();

    // A deposit whose ladder could not be established is left INITIALISED and retried next pass;
    // the OTHER coins' status updates must not be lost to it, so the failures are collected and the
    // wallet record is still written before they are reported.
    let mut establish_failures: Vec<String> = Vec::new();

    for coin in wallet.coins.iter_mut() {

        if coin.status == CoinStatus::INITIALISED || coin.status == CoinStatus::IN_MEMPOOL || coin.status == CoinStatus::UNCONFIRMED {

            let deposit_result = match check_deposit(client_config, coin, &network, wallet_name, ladder_at_sight).await {
                std::result::Result::Ok(r) => r,
                Err(e) if coin.status == CoinStatus::INITIALISED => {
                    establish_failures.push(e.to_string());
                    continue;
                }
                Err(e) => return Err(e),
            };

            if deposit_result.is_some() {
                let deposit_result = deposit_result.unwrap();
                let activity = deposit_result.activity;
                wallet.activities.push(activity);
                // The ladder row is written BEFORE the wallet record, so a crash between the two
                // leaves a ladder on disk for a coin still INITIALISED — which the next pass adopts
                // (see `check_deposit`) instead of establishing a second one.
                if let Some(bundle) = deposit_result.ladder {
                    crate::tesr::persist(client_config, &wallet.name, &bundle).await?;
                }
            }
        } else if coin.status == CoinStatus::IN_TRANSFER {

            let is_transferred = check_transfer(client_config, coin).await?;

            if is_transferred {
                coin.status = CoinStatus::TRANSFERRED;
            }

        } else if coin.status == CoinStatus::WITHDRAWING {
            check_withdrawal(client_config, coin).await?;
        }
    }

    let duplicated_coins = check_for_duplicated(client_config, &wallet.coins).await?;

    wallet.coins.extend(duplicated_coins);

    // invalidate duplicated coins that were not transferred: once the index-0 coin under a
    // statechain id has been handed over, no duplicate deposit to that address can move any more.
    for i in 0..wallet.coins.len() {
        if wallet.coins[i].status == CoinStatus::DUPLICATED {
            let is_transferred = (0..wallet.coins.len()).any(|j|
                i != j && // Skip comparing with self
                wallet.coins[j].statechain_id == wallet.coins[i].statechain_id &&
                wallet.coins[j].status == CoinStatus::TRANSFERRED
            );
            if is_transferred {
                wallet.coins[i].status = CoinStatus::INVALIDATED;
            }
        }
    }

    update_wallet(&client_config.pool, &wallet).await?;

    if !establish_failures.is_empty() {
        return Err(anyhow!(
            "{} deposit(s) were seen but could not be laddered this pass and are NOT booked:\n  {}",
            establish_failures.len(),
            establish_failures.join("\n  ")
        ));
    }

    Ok(())
}
