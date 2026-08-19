/* @ts-self-types="./mercury_wasm.d.ts" */

//#region exports

/**
 * @param {any} token_json
 * @param {any} wallet_json
 * @returns {any}
 */
export function addToken(token_json, wallet_json) {
    const ret = wasm.addToken(token_json, wallet_json);
    return ret;
}

/**
 * @param {string} token_id
 * @param {any} wallet_json
 * @returns {any}
 */
export function confirmToken(token_id, wallet_json) {
    const ptr0 = passStringToWasm0(token_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.confirmToken(ptr0, len0, wallet_json);
    return ret;
}

/**
 * @param {any} coin_json
 * @param {string} network
 * @returns {any}
 */
export function createAggregatedAddress(coin_json, network) {
    const ptr0 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.createAggregatedAddress(coin_json, ptr0, len0);
    return ret;
}

/**
 * @param {any} coin_json
 * @returns {any}
 */
export function createAndCommitNonces(coin_json) {
    const ret = wasm.createAndCommitNonces(coin_json);
    return ret;
}

/**
 * @param {any} backup_tx_json
 * @param {any} coin_json
 * @param {string} to_address
 * @param {number} fee_rate_sats_per_byte
 * @param {string} network
 * @returns {string}
 */
export function createCpfpTx(backup_tx_json, coin_json, to_address, fee_rate_sats_per_byte, network) {
    let deferred3_0;
    let deferred3_1;
    try {
        const ptr0 = passStringToWasm0(to_address, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.createCpfpTx(backup_tx_json, coin_json, ptr0, len0, fee_rate_sats_per_byte, ptr1, len1);
        deferred3_0 = ret[0];
        deferred3_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

/**
 * @param {any} coin_json
 * @param {string} token_id
 * @returns {any}
 */
export function createDepositMsg1(coin_json, token_id) {
    const ptr0 = passStringToWasm0(token_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.createDepositMsg1(coin_json, ptr0, len0);
    return ret;
}

/**
 * @param {string} msg
 * @param {string} client_partial_sig_hex
 * @param {string} server_partial_sig_hex
 * @param {string} session_hex
 * @param {string} output_pubkey_hex
 * @returns {string}
 */
export function createSignature(msg, client_partial_sig_hex, server_partial_sig_hex, session_hex, output_pubkey_hex) {
    let deferred6_0;
    let deferred6_1;
    try {
        const ptr0 = passStringToWasm0(msg, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(client_partial_sig_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(server_partial_sig_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(session_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(output_pubkey_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len4 = WASM_VECTOR_LEN;
        const ret = wasm.createSignature(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        deferred6_0 = ret[0];
        deferred6_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred6_0, deferred6_1, 1);
    }
}

/**
 * @param {any} statechain_info
 * @param {any} transfer_msg
 * @param {any} coin
 * @returns {any}
 */
export function createTransferReceiverRequestPayload(statechain_info, transfer_msg, coin) {
    const ret = wasm.createTransferReceiverRequestPayload(statechain_info, transfer_msg, coin);
    return ret;
}

/**
 * @param {string} recipient_address
 * @param {string} input_txid
 * @param {number} input_vout
 * @param {string} client_seckey
 * @returns {string}
 */
export function createTransferSignature(recipient_address, input_txid, input_vout, client_seckey) {
    let deferred4_0;
    let deferred4_1;
    try {
        const ptr0 = passStringToWasm0(recipient_address, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(input_txid, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        _assertNum(input_vout);
        const ptr2 = passStringToWasm0(client_seckey, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.createTransferSignature(ptr0, len0, ptr1, len1, input_vout, ptr2, len2);
        deferred4_0 = ret[0];
        deferred4_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
    }
}

/**
 * @param {string} x1
 * @param {string} recipient_address
 * @param {any} coin_json
 * @param {string} transfer_signature
 * @param {any} backup_transactions
 * @returns {any}
 */
export function createTransferUpdateMsg(x1, recipient_address, coin_json, transfer_signature, backup_transactions) {
    const ptr0 = passStringToWasm0(x1, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(recipient_address, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(transfer_signature, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.createTransferUpdateMsg(ptr0, len0, ptr1, len1, coin_json, ptr2, len2, backup_transactions);
    return ret;
}

/**
 * @param {string} sc_address
 * @returns {any}
 */
export function decodeTransferAddress(sc_address) {
    const ptr0 = passStringToWasm0(sc_address, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.decodeTransferAddress(ptr0, len0);
    return ret;
}

/**
 * @param {string} encrypted_message
 * @param {string} private_key_wif
 * @returns {any}
 */
export function decryptTransferMsg(encrypted_message, private_key_wif) {
    const ptr0 = passStringToWasm0(encrypted_message, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(private_key_wif, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.decryptTransferMsg(ptr0, len0, ptr1, len1);
    return ret;
}

/**
 * @param {any} walletJson
 * @param {string} authPubkey
 * @returns {any}
 */
export function duplicateCoinToInitializedState(walletJson, authPubkey) {
    const ptr0 = passStringToWasm0(authPubkey, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.duplicateCoinToInitializedState(walletJson, ptr0, len0);
    return ret;
}

/**
 * @param {string} name
 * @param {string} mnemonic
 * @returns {any}
 */
export function fromMnemonic(name, mnemonic) {
    const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(mnemonic, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.fromMnemonic(ptr0, len0, ptr1, len1);
    return ret;
}

/**
 * @returns {string}
 */
export function generateMnemonic() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.generateMnemonic();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * @param {any} wallet_json
 * @returns {any}
 */
export function getActivityLog(wallet_json) {
    const ret = wasm.getActivityLog(wallet_json);
    return ret;
}

/**
 * @param {string} tx0_hex
 * @param {any} tx_outpoint
 * @returns {number}
 */
export function getAmountFromTx0(tx0_hex, tx_outpoint) {
    const ptr0 = passStringToWasm0(tx0_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.getAmountFromTx0(ptr0, len0, tx_outpoint);
    return ret >>> 0;
}

/**
 * @param {any} wallet_json
 * @returns {number}
 */
export function getBalance(wallet_json) {
    const ret = wasm.getBalance(wallet_json);
    return ret >>> 0;
}

/**
 * @param {any} backup_tx
 * @returns {number}
 */
export function getBlockheight(backup_tx) {
    const ret = wasm.getBlockheight(backup_tx);
    return ret >>> 0;
}

/**
 * @param {any} wallet_json
 * @returns {any}
 */
export function getCoins(wallet_json) {
    const ret = wasm.getCoins(wallet_json);
    return ret;
}

/**
 * @returns {any}
 */
export function getMockWallet() {
    const ret = wasm.getMockWallet();
    return ret;
}

/**
 * @param {any} wallet_json
 * @returns {any}
 */
export function getNewCoin(wallet_json) {
    const ret = wasm.getNewCoin(wallet_json);
    return ret;
}

/**
 * @param {string} server_public_key_hex
 * @param {any} coin
 * @param {string} statechain_id
 * @param {any} tx0_outpoint
 * @param {string} tx0_hex
 * @param {string} network
 * @returns {any}
 */
export function getNewKeyInfo(server_public_key_hex, coin, statechain_id, tx0_outpoint, tx0_hex, network) {
    const ptr0 = passStringToWasm0(server_public_key_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(statechain_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(tx0_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len3 = WASM_VECTOR_LEN;
    const ret = wasm.getNewKeyInfo(ptr0, len0, coin, ptr1, len1, tx0_outpoint, ptr2, len2, ptr3, len3);
    return ret;
}

/**
 * @param {any} tx0_outpoint
 * @param {string} tx0_hex
 * @param {string} network
 * @returns {string}
 */
export function getOutputAddressFromTx0(tx0_outpoint, tx0_hex, network) {
    let deferred3_0;
    let deferred3_1;
    try {
        const ptr0 = passStringToWasm0(tx0_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.getOutputAddressFromTx0(tx0_outpoint, ptr0, len0, ptr1, len1);
        deferred3_0 = ret[0];
        deferred3_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

/**
 * @param {any} coin_json
 * @param {number} block_height
 * @param {number} initlock
 * @param {number} interval
 * @param {number} fee_rate_sats_per_byte
 * @param {number} qt_backup_tx
 * @param {string} to_address
 * @param {string} network
 * @param {boolean} is_withdrawal
 * @returns {any}
 */
export function getPartialSigRequest(coin_json, block_height, initlock, interval, fee_rate_sats_per_byte, qt_backup_tx, to_address, network, is_withdrawal) {
    _assertNum(block_height);
    _assertNum(initlock);
    _assertNum(interval);
    _assertNum(qt_backup_tx);
    const ptr0 = passStringToWasm0(to_address, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    _assertBoolean(is_withdrawal);
    const ret = wasm.getPartialSigRequest(coin_json, block_height, initlock, interval, fee_rate_sats_per_byte, qt_backup_tx, ptr0, len0, ptr1, len1, is_withdrawal);
    return ret;
}

/**
 * @param {any} backup_tx
 * @returns {any}
 */
export function getPreviousOutpoint(backup_tx) {
    const ret = wasm.getPreviousOutpoint(backup_tx);
    return ret;
}

/**
 * @param {any} wallet_json
 * @param {number} index
 * @param {string} network
 * @returns {string}
 */
export function getSCAddress(wallet_json, index, network) {
    let deferred2_0;
    let deferred2_1;
    try {
        _assertNum(index);
        const ptr0 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.getSCAddress(wallet_json, index, ptr0, len0);
        deferred2_0 = ret[0];
        deferred2_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
    }
}

/**
 * @param {any} wallet_json
 * @returns {any}
 */
export function getTokens(wallet_json) {
    const ret = wasm.getTokens(wallet_json);
    return ret;
}

/**
 * @param {any} backup_transactions
 * @returns {any}
 */
export function getTx0Outpoint(backup_transactions) {
    const ret = wasm.getTx0Outpoint(backup_transactions);
    return ret;
}

/**
 * @param {any} coin_json
 * @param {string} network
 * @returns {string}
 */
export function getUserBackupAddress(coin_json, network) {
    let deferred2_0;
    let deferred2_1;
    try {
        const ptr0 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.getUserBackupAddress(coin_json, ptr0, len0);
        deferred2_0 = ret[0];
        deferred2_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
    }
}

/**
 * @param {any} coin_json
 * @param {any} deposit_msg_1_response_json
 * @returns {any}
 */
export function handleDepositMsg1Response(coin_json, deposit_msg_1_response_json) {
    const ret = wasm.handleDepositMsg1Response(coin_json, deposit_msg_1_response_json);
    return ret;
}

/**
 * @param {any} coin
 * @param {string} enclave_pubkey
 * @returns {boolean}
 */
export function isEnclavePubkeyPartOfCoin(coin, enclave_pubkey) {
    const ptr0 = passStringToWasm0(enclave_pubkey, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.isEnclavePubkeyPartOfCoin(coin, ptr0, len0);
    return ret !== 0;
}

/**
 * @param {any} backup_transactions
 * @param {any} coin
 * @param {string} network
 * @returns {any}
 */
export function latestBackuptxPaysToUserpubkey(backup_transactions, coin, network) {
    const ptr0 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.latestBackuptxPaysToUserpubkey(backup_transactions, coin, ptr0, len0);
    return ret;
}

/**
 * @param {string} encoded_unsigned_tx
 * @param {string} signature_hex
 * @returns {string}
 */
export function newBackupTransaction(encoded_unsigned_tx, signature_hex) {
    let deferred3_0;
    let deferred3_1;
    try {
        const ptr0 = passStringToWasm0(encoded_unsigned_tx, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(signature_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.newBackupTransaction(ptr0, len0, ptr1, len1);
        deferred3_0 = ret[0];
        deferred3_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

/**
 * @param {number} blockheight
 * @param {any} wallet_json
 * @returns {any}
 */
export function setBlockheight(blockheight, wallet_json) {
    _assertNum(blockheight);
    const ret = wasm.setBlockheight(blockheight, wallet_json);
    return ret;
}

/**
 * @param {any} config_json
 * @param {any} wallet_json
 * @returns {any}
 */
export function setConfig(config_json, wallet_json) {
    const ret = wasm.setConfig(config_json, wallet_json);
    return ret;
}

/**
 * @param {string} statechain_id
 * @param {any} coin
 * @returns {string}
 */
export function signMessage(statechain_id, coin) {
    let deferred2_0;
    let deferred2_1;
    try {
        const ptr0 = passStringToWasm0(statechain_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.signMessage(ptr0, len0, coin);
        deferred2_0 = ret[0];
        deferred2_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
    }
}

/**
 * @param {string} address
 * @param {string} network
 * @returns {boolean}
 */
export function validateAddress(address, network) {
    const ptr0 = passStringToWasm0(address, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.validateAddress(ptr0, len0, ptr1, len1);
    return ret !== 0;
}

/**
 * @param {any} backup_transactions
 * @param {any} statechain_info
 * @param {string} tx0_hex
 * @param {number} current_blockheight
 * @param {number} fee_rate_tolerance
 * @param {number} current_fee_rate_sats_per_byte
 * @param {number} lockheight_init
 * @param {number} interval
 * @returns {any}
 */
export function validateSignatureScheme(backup_transactions, statechain_info, tx0_hex, current_blockheight, fee_rate_tolerance, current_fee_rate_sats_per_byte, lockheight_init, interval) {
    const ptr0 = passStringToWasm0(tx0_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    _assertNum(current_blockheight);
    _assertNum(lockheight_init);
    _assertNum(interval);
    const ret = wasm.validateSignatureScheme(backup_transactions, statechain_info, ptr0, len0, current_blockheight, fee_rate_tolerance, current_fee_rate_sats_per_byte, lockheight_init, interval);
    return ret;
}

/**
 * @param {string} enclave_public_key
 * @param {any} transfer_msg
 * @param {any} tx0_outpoint
 * @param {string} tx0_hex
 * @param {string} network
 * @returns {boolean}
 */
export function validateTx0OutputPubkey(enclave_public_key, transfer_msg, tx0_outpoint, tx0_hex, network) {
    const ptr0 = passStringToWasm0(enclave_public_key, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(tx0_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.validateTx0OutputPubkey(ptr0, len0, transfer_msg, tx0_outpoint, ptr1, len1, ptr2, len2);
    return ret !== 0;
}

/**
 * @param {any} backup_tx
 * @param {string} tx0_hex
 * @param {any} statechain_info
 * @returns {any}
 */
export function verifyBlindedMusigScheme(backup_tx, tx0_hex, statechain_info) {
    const ptr0 = passStringToWasm0(tx0_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.verifyBlindedMusigScheme(backup_tx, ptr0, len0, statechain_info);
    return ret;
}

/**
 * @param {any} transfer_msg
 * @param {string} client_pubkey_share
 * @param {string} network
 * @returns {boolean}
 */
export function verifyLatestBackupTxPaysToUserPubkey(transfer_msg, client_pubkey_share, network) {
    const ptr0 = passStringToWasm0(client_pubkey_share, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(network, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.verifyLatestBackupTxPaysToUserPubkey(transfer_msg, ptr0, len0, ptr1, len1);
    return ret !== 0;
}

/**
 * @param {string} tx_n_hex
 * @param {string} tx0_hex
 * @param {number} fee_rate_tolerance
 * @param {number} current_fee_rate_sats_per_byte
 * @returns {any}
 */
export function verifyTransactionSignature(tx_n_hex, tx0_hex, fee_rate_tolerance, current_fee_rate_sats_per_byte) {
    const ptr0 = passStringToWasm0(tx_n_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(tx0_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.verifyTransactionSignature(ptr0, len0, ptr1, len1, fee_rate_tolerance, current_fee_rate_sats_per_byte);
    return ret;
}

/**
 * @param {string} new_user_pubkey
 * @param {any} tx0_outpoint
 * @param {any} transfer_msg
 * @returns {boolean}
 */
export function verifyTransferSignature(new_user_pubkey, tx0_outpoint, transfer_msg) {
    const ptr0 = passStringToWasm0(new_user_pubkey, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.verifyTransferSignature(ptr0, len0, tx0_outpoint, transfer_msg);
    return ret !== 0;
}

//#endregion

//#region wasm imports
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Error_960c155d3d49e4c2: function() { return logError(function (arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg___wbindgen_bigint_get_as_i64_3d3aba5d616c6a51: function(arg0, arg1) {
            const v = arg1;
            const ret = typeof(v) === 'bigint' ? v : undefined;
            if (!isLikeNone(ret)) {
                _assertBigInt(ret);
            }
            getDataViewMemory0().setBigInt64(arg0 + 8 * 1, isLikeNone(ret) ? BigInt(0) : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_boolean_get_6ea149f0a8dcc5ff: function(arg0) {
            const v = arg0;
            const ret = typeof(v) === 'boolean' ? v : undefined;
            if (!isLikeNone(ret)) {
                _assertBoolean(ret);
            }
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg___wbindgen_debug_string_ab4b34d23d6778bd: function(arg0, arg1) {
            const ret = debugString(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_in_a5d8b22e52b24dd1: function(arg0, arg1) {
            const ret = arg0 in arg1;
            _assertBoolean(ret);
            return ret;
        },
        __wbg___wbindgen_is_bigint_ec25c7f91b4d9e93: function(arg0) {
            const ret = typeof(arg0) === 'bigint';
            _assertBoolean(ret);
            return ret;
        },
        __wbg___wbindgen_is_function_3baa9db1a987f47d: function(arg0) {
            const ret = typeof(arg0) === 'function';
            _assertBoolean(ret);
            return ret;
        },
        __wbg___wbindgen_is_object_63322ec0cd6ea4ef: function(arg0) {
            const val = arg0;
            const ret = typeof(val) === 'object' && val !== null;
            _assertBoolean(ret);
            return ret;
        },
        __wbg___wbindgen_is_string_6df3bf7ef1164ed3: function(arg0) {
            const ret = typeof(arg0) === 'string';
            _assertBoolean(ret);
            return ret;
        },
        __wbg___wbindgen_is_undefined_29a43b4d42920abd: function(arg0) {
            const ret = arg0 === undefined;
            _assertBoolean(ret);
            return ret;
        },
        __wbg___wbindgen_jsval_eq_d3465d8a07697228: function(arg0, arg1) {
            const ret = arg0 === arg1;
            _assertBoolean(ret);
            return ret;
        },
        __wbg___wbindgen_jsval_loose_eq_cac3565e89b4134c: function(arg0, arg1) {
            const ret = arg0 == arg1;
            _assertBoolean(ret);
            return ret;
        },
        __wbg___wbindgen_number_get_c7f42aed0525c451: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            if (!isLikeNone(ret)) {
                _assertNum(ret);
            }
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_string_get_7ed5322991caaec5: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_6b64449b9b9ed33c: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_call_14b169f759b26747: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.call(arg1);
            return ret;
        }, arguments); },
        __wbg_call_a24592a6f349a97e: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_crypto_90efa04a103d6db2: function() { return logError(function (arg0) {
            const ret = arg0.crypto;
            return ret;
        }, arguments); },
        __wbg_done_9158f7cc8751ba32: function() { return logError(function (arg0) {
            const ret = arg0.done;
            _assertBoolean(ret);
            return ret;
        }, arguments); },
        __wbg_entries_e0b73aa8571ddb56: function() { return logError(function (arg0) {
            const ret = Object.entries(arg0);
            return ret;
        }, arguments); },
        __wbg_getRandomValues_b9488c03d6ecdc0d: function() { return handleError(function (arg0, arg1) {
            arg0.getRandomValues(arg1);
        }, arguments); },
        __wbg_get_1affdbdd5573b16a: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_get_8360291721e2339f: function() { return logError(function (arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        }, arguments); },
        __wbg_get_unchecked_17f53dad852b9588: function() { return logError(function (arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        }, arguments); },
        __wbg_get_with_ref_key_f38bf27dc398d91b: function() { return logError(function (arg0, arg1) {
            const ret = arg0[arg1];
            return ret;
        }, arguments); },
        __wbg_instanceof_ArrayBuffer_7c8433c6ed14ffe3: function() { return logError(function (arg0) {
            let result;
            try {
                result = arg0 instanceof ArrayBuffer;
            } catch (_) {
                result = false;
            }
            const ret = result;
            _assertBoolean(ret);
            return ret;
        }, arguments); },
        __wbg_instanceof_Uint8Array_152ba1f289edcf3f: function() { return logError(function (arg0) {
            let result;
            try {
                result = arg0 instanceof Uint8Array;
            } catch (_) {
                result = false;
            }
            const ret = result;
            _assertBoolean(ret);
            return ret;
        }, arguments); },
        __wbg_isArray_c3109d14ffc06469: function() { return logError(function (arg0) {
            const ret = Array.isArray(arg0);
            _assertBoolean(ret);
            return ret;
        }, arguments); },
        __wbg_isSafeInteger_4fc213d1989d6d2a: function() { return logError(function (arg0) {
            const ret = Number.isSafeInteger(arg0);
            _assertBoolean(ret);
            return ret;
        }, arguments); },
        __wbg_iterator_013bc09ec998c2a7: function() { return logError(function () {
            const ret = Symbol.iterator;
            return ret;
        }, arguments); },
        __wbg_length_3d4ecd04bd8d22f1: function() { return logError(function (arg0) {
            const ret = arg0.length;
            _assertNum(ret);
            return ret;
        }, arguments); },
        __wbg_length_9f1775224cf1d815: function() { return logError(function (arg0) {
            const ret = arg0.length;
            _assertNum(ret);
            return ret;
        }, arguments); },
        __wbg_msCrypto_68b2f4999b2901b0: function() { return logError(function (arg0) {
            const ret = arg0.msCrypto;
            return ret;
        }, arguments); },
        __wbg_new_0c7403db6e782f19: function() { return logError(function (arg0) {
            const ret = new Uint8Array(arg0);
            return ret;
        }, arguments); },
        __wbg_new_682678e2f47e32bc: function() { return logError(function () {
            const ret = new Array();
            return ret;
        }, arguments); },
        __wbg_new_aa8d0fa9762c29bd: function() { return logError(function () {
            const ret = new Object();
            return ret;
        }, arguments); },
        __wbg_new_with_length_8c854e41ea4dae9b: function() { return logError(function (arg0) {
            const ret = new Uint8Array(arg0 >>> 0);
            return ret;
        }, arguments); },
        __wbg_next_0340c4ae324393c3: function() { return handleError(function (arg0) {
            const ret = arg0.next();
            return ret;
        }, arguments); },
        __wbg_next_7646edaa39458ef7: function() { return logError(function (arg0) {
            const ret = arg0.next;
            return ret;
        }, arguments); },
        __wbg_node_046e1cb1b8cf3d92: function() { return logError(function (arg0) {
            const ret = arg0.node;
            return ret;
        }, arguments); },
        __wbg_process_7b13606d1afee88f: function() { return logError(function (arg0) {
            const ret = arg0.process;
            return ret;
        }, arguments); },
        __wbg_prototypesetcall_a6b02eb00b0f4ce2: function() { return logError(function (arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        }, arguments); },
        __wbg_randomFillSync_73a2861b2e659112: function() { return handleError(function (arg0, arg1) {
            arg0.randomFillSync(arg1);
        }, arguments); },
        __wbg_require_01ac6430ef887047: function() { return handleError(function () {
            const ret = module.require;
            return ret;
        }, arguments); },
        __wbg_set_3bf1de9fab0cd644: function() { return logError(function (arg0, arg1, arg2) {
            arg0[arg1 >>> 0] = arg2;
        }, arguments); },
        __wbg_set_d1cb61e9f39c870f: function() { return logError(function (arg0, arg1, arg2) {
            arg0[arg1] = arg2;
        }, arguments); },
        __wbg_static_accessor_GLOBAL_8cfadc87a297ca02: function() { return logError(function () {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_static_accessor_GLOBAL_THIS_602256ae5c8f42cf: function() { return logError(function () {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_static_accessor_SELF_e445c1c7484aecc3: function() { return logError(function () {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_static_accessor_WINDOW_f20e8576ef1e0f17: function() { return logError(function () {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_subarray_f8ca46a25b1f5e0d: function() { return logError(function (arg0, arg1, arg2) {
            const ret = arg0.subarray(arg1 >>> 0, arg2 >>> 0);
            return ret;
        }, arguments); },
        __wbg_value_ee3a06f4579184fa: function() { return logError(function (arg0) {
            const ret = arg0.value;
            return ret;
        }, arguments); },
        __wbg_versions_6963303269777792: function() { return logError(function (arg0) {
            const ret = arg0.versions;
            return ret;
        }, arguments); },
        __wbindgen_cast_0000000000000001: function() { return logError(function (arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        }, arguments); },
        __wbindgen_cast_0000000000000002: function() { return logError(function (arg0, arg1) {
            // Cast intrinsic for `Ref(Slice(U8)) -> NamedExternref("Uint8Array")`.
            const ret = getArrayU8FromWasm0(arg0, arg1);
            return ret;
        }, arguments); },
        __wbindgen_cast_0000000000000003: function() { return logError(function (arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        }, arguments); },
        __wbindgen_cast_0000000000000004: function() { return logError(function (arg0) {
            // Cast intrinsic for `U64 -> Externref`.
            const ret = BigInt.asUintN(64, arg0);
            return ret;
        }, arguments); },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./mercury_wasm_bg.js": import0,
    };
}


//#endregion

//#region intrinsics
function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

function _assertBigInt(n) {
    if (typeof(n) !== 'bigint') throw new Error(`expected a bigint argument, found ${typeof(n)}`);
}

function _assertBoolean(n) {
    if (typeof(n) !== 'boolean') {
        throw new Error(`expected a boolean argument, found ${typeof(n)}`);
    }
}

function _assertNum(n) {
    if (typeof(n) !== 'number') throw new Error(`expected a number argument, found ${typeof(n)}`);
}

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function logError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        let error = (function () {
            try {
                return e instanceof Error ? `${e.message}\n\nStack:\n${e.stack}` : e.toString();
            } catch(_) {
                return "<failed to stringify thrown value>";
            }
        }());
        console.error("wasm-bindgen: imported JS function that was not marked as `catch` threw an error:", error);
        throw e;
    }
}

function passStringToWasm0(arg, malloc, realloc) {
    if (typeof(arg) !== 'string') throw new Error(`expected a string argument, found ${typeof(arg)}`);
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);
        if (ret.read !== arg.length) throw new Error('failed to pass whole string');
        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;


//#endregion

//#region wasm loading
let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('mercury_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
//#endregion
export { wasm as __wasm }
