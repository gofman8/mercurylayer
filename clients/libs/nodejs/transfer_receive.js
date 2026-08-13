const sqlite_manager = require('./sqlite_manager');
const mercury_wasm = require('mercury-wasm');
const axios = require('axios').default;
const { SocksProxyAgent } = require('socks-proxy-agent');
const bitcoinjs = require("bitcoinjs-lib");
const ecc = require("tiny-secp256k1");
const utils = require('./utils');
const { CoinStatus } = require('./coin_enum');

const newTransferAddress = async (db, wallet_name) => {

    let wallet = await sqlite_manager.getWallet(db, wallet_name);

    let coin = mercury_wasm.getNewCoin(wallet);

    wallet.coins.push(coin);

    await sqlite_manager.updateWallet(db, wallet);

    return coin.address;
}

const execute = async (clientConfig, electrumClient, db, wallet_name) => {

    let wallet = await sqlite_manager.getWallet(db, wallet_name);

    const serverInfo = await utils.infoConfig(clientConfig, electrumClient);

    let blockHeader = undefined;
    try { 
        blockHeader = await electrumClient.request('blockchain.headers.subscribe'); // request(promise)
    } catch (error) {
        throw new Error("Error getting block height from electrs server");
    }
    const currentBlockheight = blockHeader.height;

    let uniqueAuthPubkeys = new Set();

    wallet.coins.forEach(coin => {
        uniqueAuthPubkeys.add(coin.auth_pubkey);
    });

    let encMsgsPerAuthPubkey = new Map();

    for (let authPubkey of uniqueAuthPubkeys) {
        try {
            let encMessages = await getMsgAddr(clientConfig, authPubkey);
            if (encMessages.length === 0) {
               // console.log("No messages");
                continue;
            }

            encMsgsPerAuthPubkey.set(authPubkey, encMessages);
        } catch (err) {
            // console.error(err);
        }
    }

    let isThereBatchLocked = false;
    let receivedStatechainIds = [];

    let tempCoins = [...wallet.coins];
    let tempActivities = [...wallet.activities];

    for (let [authPubkey, encMessages] of encMsgsPerAuthPubkey.entries()) {

        for (let encMessage of encMessages) {

            let coin = tempCoins.find(coin => coin.auth_pubkey === authPubkey && coin.status === 'INITIALISED');

            if (coin) {
                try {
                    let messageResult = await processEncryptedMessage(clientConfig, electrumClient, db, coin, encMessage, wallet.network, serverInfo, tempActivities);

                    if (messageResult.isBatchLocked) {
                        isThereBatchLocked = true;
                    }

                    if (messageResult.statechainId) {
                        receivedStatechainIds.push(messageResult.statechainId);
                    }
                } catch (error) {
                    console.error(`Error: ${error.message}`);
                    continue;
                }

            } else {
                try {
                    let newCoin = await mercury_wasm.duplicateCoinToInitializedState(wallet, authPubkey);

                    if (newCoin) {
                        let messageResult = await processEncryptedMessage(clientConfig, electrumClient, db, newCoin, encMessage, wallet.network, serverInfo, tempActivities);

                        if (messageResult.isBatchLocked) {
                            isThereBatchLocked = true;
                        }

                        if (messageResult.statechainId) {
                            tempCoins.push(newCoin);
                            receivedStatechainIds.push(messageResult.statechainId);
                        }
                    }
                } catch (error) {
                    console.error(`Error: ${error.message}`);
                    continue;
                }
            }

            if (currentBlockheight >= coin.locktime)  {
                console.error(`The coin is expired. Coin locktime is ${coin.locktime} and current blockheight is ${currentBlockheight}`);
            }
        }
    }

    wallet.coins = [...tempCoins];
    wallet.activities = [...tempActivities];

    await sqlite_manager.updateWallet(db, wallet);

    return {
        isThereBatchLocked,
        receivedStatechainIds
    };
}

const getMsgAddr = async (clientConfig, auth_pubkey) => {

    const statechain_entity_url = clientConfig.statechainEntity;
    const path = "transfer/get_msg_addr/";
    const url = statechain_entity_url + '/' + path + auth_pubkey;

    const torProxy = clientConfig.torProxy;

    let socksAgent = undefined;

    if (torProxy) {
        socksAgent = { httpAgent: new SocksProxyAgent(torProxy) };
    }

    let response;
    try {
        response = await axios.get(url, socksAgent);
    } catch (error) {
        throw new Error('Failed to get message address from mercury server');
    }

    return response.data.list_enc_transfer_msg;
}

const processEncryptedMessage = async (clientConfig, electrumClient, db, coin, encMessage, network, serverInfo, activities) => {
    let clientAuthKey = coin.auth_privkey;
    let newUserPubkey = coin.user_pubkey;

    let transferMsg = mercury_wasm.decryptTransferMsg(encMessage, clientAuthKey);

    let tx0Outpoint = mercury_wasm.getTx0Outpoint(transferMsg.backup_transactions);

    const tx0Hex = await getTx0(electrumClient, tx0Outpoint.txid);

    const isTransferSignatureValid = mercury_wasm.verifyTransferSignature(newUserPubkey, tx0Outpoint, transferMsg);

    if (!isTransferSignatureValid) {
        throw new Error("Invalid transfer signature");
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

    // [S7] FAIL CLOSED on a laddered (TES-R) coin. This JS client cannot run the R′ ladder verifier
    // (verify_bundle), so it must NOT fall through to the un-laddered flat `num_sigs == backups.length`
    // check below: that check does not detect a retained hidden state, so a malicious sender of a
    // laddered coin could pad backups to match num_sigs and later broadcast a lower-CSV state to take
    // the coin back. Refuse rather than mis-verify. (Receive laddered coins with the Rust SDK until the
    // ladder verifier is ported.)
    // [D40.2 / A.4] THE GATE IS STRUCTURAL. It used to key on three fields the SENDER fills in
    // (`protocol_version`, `tesr_ladder`, `child_tesr_bundle`), so a sender declaring version 0 with
    // both fields omitted fell straight through to the bare `num_sigs == backups.length` census
    // below — against an integer this client never authenticates. That is the identical shape the
    // Rust receiver had to close with MIN_PREPAY_PROTOCOL_VERSION, and it made these clients the
    // CHEAPEST route in the whole trust model: a plain HTTP-response edit, no seed, no DB write.
    //
    // The coordinator-served `statechainInfo` is not sender-controlled, so it is where the check
    // belongs. An `attestation` field means the enclave signs `utexo/sig_count/v2` over
    // (statechain_id, num_sigs, sig_budget, nonce) — i.e. this deployment runs laddered coins and
    // the count below is only as good as a signature THIS CLIENT CANNOT VERIFY. Refuse.
    //
    // This does not make the client conformant; it makes it fail closed for a reason it can check.
    // The real fix is porting the attestation verification (D40.2), and until it lands these clients
    // are non-conformant receivers by design rather than by accident.
    if (statechainInfo.sig_count_attestation !== undefined && statechainInfo.sig_count_attestation !== null) {
        throw new Error("This coordinator attests sig_count (utexo/sig_count/v2) and this client cannot verify that attestation — refusing (fail-closed). The flat census below trusts num_sigs, so an unverified attestation makes it worthless. Receive with the Rust SDK.");
    }
    // The sender-declared fields are still refused, because an HONEST sender shipping ladder material
    // to a client that cannot read it should get a refusal that names the reason. This is a
    // usability check, not the security one above.
    if (transferMsg.protocol_version >= 2 ||
        (transferMsg.tesr_ladder !== undefined && transferMsg.tesr_ladder !== null) ||
        (transferMsg.child_tesr_bundle !== undefined && transferMsg.child_tesr_bundle !== null)) {
        throw new Error("Laddered (TES-R) coin: this client cannot verify the exit ladder — refusing (fail-closed); receive it with the Rust SDK");
    }

    if (statechainInfo.num_sigs != transferMsg.backup_transactions.length) {
        throw new Error("num_sigs is not correct");
    }

    let isTx0OutputUnspent = await verifyTx0OutputIsUnspentAndConfirmed(clientConfig, electrumClient, tx0Outpoint, tx0Hex, network);
    if (!isTx0OutputUnspent.result) {
        throw new Error("tx0 output is spent or not confirmed");
    }

    let currentFeeRateSatsPerByte = (serverInfo.fee_rate_sats_per_byte > clientConfig.maxFeeRate) ? clientConfig.maxFeeRate: serverInfo.fee_rate_sats_per_byte;

    const feeRateTolerance = clientConfig.feeRateTolerance;

    let isSignatureValid = mercury_wasm.validateSignatureScheme(
        transferMsg,
        statechainInfo,
        tx0Hex,
        feeRateTolerance, 
        currentFeeRateSatsPerByte,
        serverInfo.interval
    )

    if (!isSignatureValid.result) {
        throw new Error(`Invalid signature scheme, ${isSignatureValid.msg}`);
    }

    let previousLockTime = isSignatureValid.previousLockTime;

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
    coin.locktime = previousLockTime;
    coin.status = isTx0OutputUnspent.status;

    let utxo = `${tx0Outpoint.txid}:${tx0Outpoint.vout}`;

    let activity = {
        utxo: utxo,
        amount: newKeyInfo.amount,
        action: "Receive",
        date: new Date().toISOString()
    };

    activities.push(activity);

    await sqlite_manager.insertOrUpdateBackupTxs(db, transferMsg.statechain_id, transferMsg.backup_transactions);

    return {
        isBatchLocked: false,
        statechainId: transferMsg.statechain_id,
    };
}

const getTx0 = async (electrumClient, tx0_txid) => {
    return await electrumClient.request('blockchain.transaction.get', [tx0_txid]); // request(promise)
}

const verifyTx0OutputIsUnspentAndConfirmed = async (clientConfig, electrumClient, tx0Outpoint, tx0Hex, wallet_network) => {

    let tx0outputAddress = mercury_wasm.getOutputAddressFromTx0(tx0Outpoint, tx0Hex, wallet_network);

    const network = utils.getNetwork(wallet_network);

    bitcoinjs.initEccLib(ecc);

    let script = bitcoinjs.address.toOutputScript(tx0outputAddress, network);
    let hash = bitcoinjs.crypto.sha256(script);
    let reversedHash = Buffer.from(hash.reverse());
    reversedHash = reversedHash.toString('hex');

    let utxo_list = await electrumClient.request('blockchain.scripthash.listunspent', [reversedHash]);

    let status = CoinStatus.UNCONFIRMED;

    for (let unspent of utxo_list) {
        if (unspent.tx_hash === tx0Outpoint.txid && unspent.tx_pos === tx0Outpoint.vout) {

            const block_header = await electrumClient.request('blockchain.headers.subscribe');
            const blockheight = block_header.height;

            const confirmations = blockheight - unspent.height + 1;

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

    const torProxy = clientConfig.torProxy;

    let socksAgent = undefined;

    if (torProxy) {
        socksAgent = { httpAgent: new SocksProxyAgent(torProxy) };
    }

    let transferUnlockRequestPayload = {
        statechain_id: statechainId,
        auth_sig: signedStatechainId,
        auth_pub_key: authPubkey,
    };

    const response = await axios.post(url, transferUnlockRequestPayload, socksAgent);

    if (response.status != 200) {
        throw new Error(`Failed to unlock transfer message`);
    }
}

const sendTransferReceiverRequestPayload = async (clientConfig, transferReceiverRequestPayload) => {

    const statechain_entity_url = clientConfig.statechainEntity;
    const path = "transfer/receiver";
    const url = statechain_entity_url + '/' + path;

    const torProxy = clientConfig.torProxy;

    let socksAgent = undefined;

    if (torProxy) {
        socksAgent = { httpAgent: new SocksProxyAgent(torProxy) };
    }

    try {
        const response = await axios.post(url, transferReceiverRequestPayload, socksAgent);
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

module.exports = { newTransferAddress, execute };
