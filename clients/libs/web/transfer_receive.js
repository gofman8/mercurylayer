import axios from 'axios';
import initWasm from 'mercury-wasm';
import wasmUrl from 'mercury-wasm/mercury_wasm_bg.wasm?url'
import * as mercury_wasm from 'mercury-wasm';
import storageManager from './storage_manager.js';
import utils from './utils.js';
import CoinStatus from './coin_enum.js';

const newTransferAddress = async (walletName) => {

    await initWasm(wasmUrl);

    let wallet = storageManager.getWallet(walletName);

    let coin = mercury_wasm.getNewCoin(wallet);

    wallet.coins.push(coin);

    storageManager.updateWallet(wallet);

    return coin.address;
}

const getMsgAddr = async (clientConfig, auth_pubkey) => {

    const statechain_entity_url = clientConfig.statechainEntity;
    const path = "transfer/get_msg_addr/";
    const url = statechain_entity_url + '/' + path + auth_pubkey;

    const response = await axios.get(url);

    return response.data.list_enc_transfer_msg;
}

function splitBackupTransactions(backupTransactions) {
    // Map to store grouped transactions
    const groupedTxs = new Map();
    
    // Array to keep track of order of appearance of outpoints
    const orderOfAppearance = [];
    // Set to track which outpoints we've seen
    const seenOutpoints = new Set();
    
    // Process each transaction
    for (const tx of backupTransactions) {
        try {
            // Get the outpoint for this transaction
            const outpoint = mercury_wasm.getPreviousOutpoint(tx);
            
            // Create a key string from txid and vout (since Map keys need to be strings/primitives)
            const key = `${outpoint.txid}:${outpoint.vout}`;
            
            // If we haven't seen this outpoint before, record its order
            if (!seenOutpoints.has(key)) {
                seenOutpoints.add(key);
                orderOfAppearance.push(key);
            }
            
            // Add the transaction to its group
            if (!groupedTxs.has(key)) {
                groupedTxs.set(key, []);
            }
            // Create a deep copy of the transaction before adding it
            // groupedTxs.get(key).push(JSON.parse(JSON.stringify(tx)));
            groupedTxs.get(key).push(tx);
            
        } catch (error) {
            console.error('Error processing transaction:', error);
            throw error;
        }
    }
    
    // Create result array
    const result = [];
    
    // Add arrays to result in order of first appearance
    for (const key of orderOfAppearance) {
        const transactions = groupedTxs.get(key);
        if (transactions) {
            // Sort each group by tx_n
            transactions.sort((a, b) => a.tx_n - b.tx_n);
            result.push(transactions);
        }
    }
    
    return result;
}

const getTx0 = async (clientConfig, tx0_txid) => {

    let response = await axios.get(`${clientConfig.esploraServer}/api/tx/${tx0_txid}/hex`);

    return response.data;
}

const verifyTx0OutputIsUnspentAndConfirmed = async (clientConfig, tx0Outpoint, tx0Hex, wallet_network) => {

    let tx0outputAddress = mercury_wasm.getOutputAddressFromTx0(tx0Outpoint, tx0Hex, wallet_network);

    let response = await axios.get(`${clientConfig.esploraServer}/api/address/${tx0outputAddress}/utxo`);

    let utxo_list = response.data;

    let status = CoinStatus.UNCONFIRMED;

    for (let unspent of utxo_list) {
        if (unspent.txid === tx0Outpoint.txid && unspent.vout === tx0Outpoint.vout) {

            if (!unspent.status.confirmed) {
                status = CoinStatus.IN_MEMPOOL;
                break;
            }

            const response = await axios.get(`${clientConfig.esploraServer}/api/blocks/tip/height`);
            const block_header = response.data;
            const blockheight = parseInt(block_header, 10);

            if (isNaN(blockheight)) {
                throw new Error(`Invalid block height: ${block_header}`);
            }

            const confirmations = blockheight - parseInt(unspent.status.block_height, 10) + 1;

            const confirmationTarget = clientConfig.confirmationTarget;

            if (confirmations >= confirmationTarget) {
                status = CoinStatus.CONFIRMED;
            }
            
            return { result: true, status };
        }
    }

    return { result: false, status };
}

const unlockStatecoin = async (clientConfig, statechainId, signedStatechainId, authPubkey) => {

    const statechain_entity_url = clientConfig.statechainEntity;
    const path = "transfer/unlock";
    const url = statechain_entity_url + '/' + path;

    let transferUnlockRequestPayload = {
        statechain_id: statechainId,
        auth_sig: signedStatechainId,
        auth_pub_key: authPubkey,
    };

    const response = await axios.post(url, transferUnlockRequestPayload);

    if (response.status != 200) {
        throw new Error(`Failed to unlock transfer message`);
    }
}

const sendTransferReceiverRequestPayload = async (clientConfig, transferReceiverRequestPayload) => {
 
    try {
        const url = `${clientConfig.statechainEntity}/transfer/receiver`;
        const response = await axios.post(url, transferReceiverRequestPayload);
        return {
            isBatchLocked: false,
            serverPubkey: response.data.server_pubkey,
        };
    }
    catch (error) {

        if (error.response.status == 400) {
            if (error.response.data.code == 'ExpiredBatchTimeError') {
                throw new Error(`Failed to update transfer message ${error.response.data.message}`);
            } else  if (error.response.data.code == 'StatecoinBatchLockedError') {
                return {
                    isBatchLocked: true,
                    serverPubkey: null,
                };
            }
        } else {
            throw new Error(`Failed to update transfer message ${JSON.stringify(error.response.data)}`);
        }
    }
    
}

const validateEncryptedMessage = async (clientConfig, coin, encMessage, network, serverInfo, blockheight) => {
    let clientAuthKey = coin.auth_privkey;
    let newUserPubkey = coin.user_pubkey;

    let transferMsg = mercury_wasm.decryptTransferMsg(encMessage, clientAuthKey);

    let groupedBackupTransactions = splitBackupTransactions(transferMsg.backup_transactions);

    for (const [index, backupTransactions] of groupedBackupTransactions.entries()) {

        let tx0Outpoint = mercury_wasm.getTx0Outpoint(backupTransactions);

        const tx0Hex = await getTx0(clientConfig, tx0Outpoint.txid);

        if (index == 0) {
            const isTransferSignatureValid = mercury_wasm.verifyTransferSignature(newUserPubkey, tx0Outpoint, transferMsg);

            if (!isTransferSignatureValid) {
                throw new Error("Invalid transfer signature");
            }
        }
        
        const statechainInfo = await utils.getStatechainInfo(clientConfig, transferMsg.statechain_id);

        if (statechainInfo == null) {
            throw new Error("Statechain info not found");
        }

        const isTx0OutputPubkeyValid = mercury_wasm.validateTx0OutputPubkey(statechainInfo.enclave_public_key, transferMsg, tx0Outpoint, tx0Hex, network);

        if (!isTx0OutputPubkeyValid) {
            throw new Error("Invalid tx0 output pubkey");
        }

        let latestBackupTxPaysToUserPubkey = mercury_wasm.verifyLatestBackupTxPaysToUserPubkey(transferMsg, newUserPubkey, network);

        if (!latestBackupTxPaysToUserPubkey) {
            throw new Error("Latest Backup Tx does not pay to the expected public key");
        }

        // [S7] FAIL CLOSED on a laddered (TES-R) coin. This web client cannot run the R′ ladder
        // verifier (verify_bundle), so it must NOT fall through to the un-laddered flat
        // `num_sigs == backups.length` check below: that check does not detect a retained hidden
        // state, so a malicious sender of a laddered coin could pad backups to match num_sigs and
        // later broadcast a lower-CSV state to take the coin back. Refuse rather than mis-verify.
        // (Receive laddered coins with the Rust SDK until the ladder verifier is ported.)
        if (transferMsg.protocol_version >= 2 ||
            (transferMsg.tesr_ladder !== undefined && transferMsg.tesr_ladder !== null) ||
            (transferMsg.child_tesr_bundle !== undefined && transferMsg.child_tesr_bundle !== null)) {
            throw new Error("Laddered (TES-R) coin: this client cannot verify the exit ladder — refusing (fail-closed); receive it with the Rust SDK");
        }

        if (statechainInfo.num_sigs != transferMsg.backup_transactions.length) {
            throw new Error("num_sigs is not correct");
        }

        let isTx0OutputUnspent = await verifyTx0OutputIsUnspentAndConfirmed(clientConfig, tx0Outpoint, tx0Hex, network);
        if (!isTx0OutputUnspent.result) {
            throw new Error("tx0 output is spent or not confirmed");
        }

        const currentFeeRateSatsPerByte = (serverInfo.feeRateSatsPerByte > clientConfig.maxFeeRate) ? clientConfig.maxFeeRate: serverInfo.feeRateSatsPerByte;

        const feeRateTolerance = clientConfig.feeRateTolerance;

        let isSignatureValid = mercury_wasm.validateSignatureScheme(
            backupTransactions,
            statechainInfo,
            tx0Hex,
            blockheight,
            feeRateTolerance, 
            currentFeeRateSatsPerByte,
            serverInfo.initlock,
            serverInfo.interval
        )

        if (!isSignatureValid.result) {
            throw new Error(`Invalid signature scheme, ${isSignatureValid.msg}`);
        }
    }
}

const processEncryptedMessage = async (clientConfig, coin, encMessage, network, walletName, activities) => {

    let transferReceiveResult = {
        isBatchLocked: false,
        statechainId: null,
        duplicatedCoins: [],
    };

    let clientAuthKey = coin.auth_privkey;

    let transferMsg = mercury_wasm.decryptTransferMsg(encMessage, clientAuthKey);

    let groupedBackupTransactions = splitBackupTransactions(transferMsg.backup_transactions);

    for (const [index, backupTransactions] of groupedBackupTransactions.entries()) {

        if (index == 0) {

            let tx0Outpoint = mercury_wasm.getTx0Outpoint(backupTransactions);
            const tx0Hex = await getTx0(clientConfig, tx0Outpoint.txid);

            const statechainInfo = await utils.getStatechainInfo(clientConfig, transferMsg.statechain_id);
            if (statechainInfo == null) {
                throw new Error("Statechain info not found");
            }

            let isTx0OutputUnspent = await verifyTx0OutputIsUnspentAndConfirmed(clientConfig, tx0Outpoint, tx0Hex, network);
            if (!isTx0OutputUnspent.result) {
                throw new Error("tx0 output is spent or not confirmed");
            }

            const backupTx = backupTransactions.at(-1);

            let lastTxLockTime = mercury_wasm.getBlockheight(backupTx);

            const transferReceiverRequestPayload = mercury_wasm.createTransferReceiverRequestPayload(statechainInfo, transferMsg, coin);

            let signedStatechainIdForUnlock = mercury_wasm.signMessage(transferMsg.statechain_id, coin);

            await unlockStatecoin(clientConfig, transferMsg.statechain_id, signedStatechainIdForUnlock, coin.auth_pubkey);

            let serverPublicKeyHex = "";

            try {
                const transferReceiverResult = await sendTransferReceiverRequestPayload(clientConfig, transferReceiverRequestPayload);
        
                if (transferReceiverResult.isBatchLocked) {
                    return {
                        isBatchLocked: true,
                        statechainId: null,
                        duplicatedCoins: [],
                    };
                }
        
                serverPublicKeyHex = transferReceiverResult.serverPubkey;
            } catch (error) {
                throw new Error(error);
            }

            let newKeyInfo = mercury_wasm.getNewKeyInfo(serverPublicKeyHex, coin, transferMsg.statechain_id, tx0Outpoint, tx0Hex, network);

            coin.server_pubkey = serverPublicKeyHex;
            coin.aggregated_pubkey = newKeyInfo.aggregate_pubkey;
            coin.aggregated_address = newKeyInfo.aggregate_address;
            coin.statechain_id = transferMsg.statechain_id;
            coin.signed_statechain_id = newKeyInfo.signed_statechain_id;
            coin.amount = newKeyInfo.amount;
            coin.utxo_txid = tx0Outpoint.txid;
            coin.utxo_vout = tx0Outpoint.vout;
            coin.locktime = lastTxLockTime;
            coin.status = isTx0OutputUnspent.status;

            let utxo = `${tx0Outpoint.txid}:${tx0Outpoint.vout}`;

            let activity = {
                utxo: utxo,
                amount: newKeyInfo.amount,
                action: "Receive",
                date: new Date().toISOString()
            };

            activities.push(activity);

            storageManager.updateBackupTransactions(walletName, transferMsg.statechain_id, transferMsg.backup_transactions);

            transferReceiveResult.isBatchLocked = false;
            transferReceiveResult.statechainId = transferMsg.statechain_id;
        } else {
            let tx0Outpoint = mercury_wasm.getTx0Outpoint(backupTransactions);
            const tx0Hex = await getTx0(clientConfig, tx0Outpoint.txid);

            const firstBackupTx = backupTransactions.at(0);
            
            let txOutpoint = mercury_wasm.getPreviousOutpoint(firstBackupTx);

            let amount = mercury_wasm.getAmountFromTx0(tx0Hex, txOutpoint);

            transferReceiveResult.duplicatedCoins.push({
                txid: txOutpoint.txid,
                vout: txOutpoint.vout,
                amount,
                index,
            });
        }
    }

    return transferReceiveResult;
}

function sortCoinsByStatechain(coins) {
    // Create a map to store the position of first occurrence of each statechain_id
    const firstPositions = new Map();

    // Record the position of the first occurrence of each statechain_id
    coins.forEach((coin, idx) => {
        if (coin.statechain_id) {
            if (!firstPositions.has(coin.statechain_id)) {
                firstPositions.set(coin.statechain_id, idx);
            }
        }
    });

    // Sort the array maintaining original order of different statechain_ids
    coins.sort((a, b) => {
        // Helper function to compare duplicate_index values
        const compareDuplicateIndex = (x, y) => {
            // Handle undefined cases with a default value of 0
            const indexA = x.duplicate_index ?? 0;
            const indexB = y.duplicate_index ?? 0;
            return indexA - indexB;
        };

        // Handle cases where statechain_id might be null/undefined
        if (!a.statechain_id && !b.statechain_id) {
            return compareDuplicateIndex(a, b);
        }
        if (!a.statechain_id) {
            return 1;  // equivalent to Ordering::Greater
        }
        if (!b.statechain_id) {
            return -1; // equivalent to Ordering::Less
        }

        // Both have statechain_id
        if (a.statechain_id === b.statechain_id) {
            // Same statechain_id: sort by duplicate_index
            return compareDuplicateIndex(a, b);
        } else {
            // Different statechain_ids: compare their first positions
            const posA = firstPositions.get(a.statechain_id);
            const posB = firstPositions.get(b.statechain_id);
            return posA - posB;
        }
    });
}


const execute = async (clientConfig, walletName) => {
    await initWasm(wasmUrl);

    let wallet = storageManager.getWallet(walletName);

    const serverInfo = await utils.infoConfig(clientConfig);

    let uniqueAuthPubkeys = new Set();

    wallet.coins.forEach(coin => {
        uniqueAuthPubkeys.add(coin.auth_pubkey);
    });

    let encMsgsPerAuthPubkey = new Map();

    for (let authPubkey of uniqueAuthPubkeys) {
        try {
            let encMessages = await getMsgAddr(clientConfig, authPubkey);
            if (encMessages.length === 0) {
                console.log("No messages");
                continue;
            }

            encMsgsPerAuthPubkey.set(authPubkey, encMessages);
        } catch (err) {
            console.error(err);
        }
    }

    let isThereBatchLocked = false;
    let receivedStatechainIds = [];

    let tempCoins = [...wallet.coins];
    let tempActivities = [...wallet.activities];
    let duplicatedCoins = [];

    const response = await axios.get(`${clientConfig.esploraServer}/api/blocks/tip/height`);
    const blockheight = response.data;

    for (let [authPubkey, encMessages] of encMsgsPerAuthPubkey.entries()) {

        for (let encMessage of encMessages) {

            let coin = tempCoins.find(coin => coin.auth_pubkey === authPubkey && coin.status === 'INITIALISED');

            if (coin) {
                try {

                    await validateEncryptedMessage(clientConfig, coin, encMessage, wallet.network, serverInfo, blockheight);

                    let messageResult = await processEncryptedMessage(clientConfig, coin, encMessage, wallet.network, wallet.name, tempActivities);
                    if (messageResult.isBatchLocked) {
                        isThereBatchLocked = true;
                    }

                    if (messageResult.statechainId) {
                        receivedStatechainIds.push(messageResult.statechainId);
                    }

                    if (messageResult.duplicatedCoins.length > 0 && messageResult.isBatchLocked) {
                        throw new Error("Batch is locked and there are duplicated coins");
                    }

                    for (const duplicatedCoinData of messageResult.duplicatedCoins) {

                        if (duplicatedCoinData.index == 0) {
                            throw new Error("Duplicated coin with index 0");
                        }

                        const duplicatedCoin = {
                            ...coin,
                            status: CoinStatus.DUPLICATED,
                            utxo_txid: duplicatedCoinData.txid,
                            utxo_vout: duplicatedCoinData.vout,
                            amount: duplicatedCoinData.amount,
                            duplicate_index: duplicatedCoinData.index
                        };

                        duplicatedCoins.push(duplicatedCoin);
                    }

                } catch (error) {
                    console.error(`Error: ${error}`);
                    continue;
                }

            } else {
                try {
                    let newCoin = await mercury_wasm.duplicateCoinToInitializedState(wallet, authPubkey);

                    if (newCoin) {

                        await validateEncryptedMessage(clientConfig, newCoin, encMessage, wallet.network, serverInfo, blockheight);

                        let messageResult = await processEncryptedMessage(clientConfig, newCoin, encMessage, wallet.network, wallet.name, tempActivities);
                        if (messageResult.isBatchLocked) {
                            isThereBatchLocked = true;
                        }

                        if (messageResult.statechainId) {
                            tempCoins.push(newCoin);
                            receivedStatechainIds.push(messageResult.statechainId);
                        }

                        if (messageResult.duplicatedCoins.length > 0 && messageResult.isBatchLocked) {
                            throw new Error("Batch is locked and there are duplicated coins");
                        }

                        for (const duplicatedCoinData of messageResult.duplicatedCoins) {

                            if (duplicatedCoinData.index == 0) {
                                throw new Error("Duplicated coin with index 0");
                            }
    
                            const duplicatedCoin = {
                                ...newCoin,
                                status: CoinStatus.DUPLICATED,
                                utxo_txid: duplicatedCoinData.txid,
                                utxo_vout: duplicatedCoinData.vout,
                                amount: duplicatedCoinData.amount,
                                duplicate_index: duplicatedCoinData.index
                            };
    
                            duplicatedCoins.push(duplicatedCoin);
                        }
                    }
                } catch (error) {
                    console.error(`Error: ${error}`);
                    continue;
                }
            }
        }
    }

    tempCoins.push(...duplicatedCoins);
    sortCoinsByStatechain(tempCoins);
    wallet.coins = [...tempCoins];
    wallet.activities = [...tempActivities];

    storageManager.updateWallet(wallet);

    return {
        isThereBatchLocked,
        receivedStatechainIds
    };
}

export default { newTransferAddress, execute, splitBackupTransactions }