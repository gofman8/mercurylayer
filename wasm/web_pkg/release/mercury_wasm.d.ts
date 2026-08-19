/* tslint:disable */
/* eslint-disable */

export function addToken(token_json: any, wallet_json: any): any;

export function confirmToken(token_id: string, wallet_json: any): any;

export function createAggregatedAddress(coin_json: any, network: string): any;

export function createAndCommitNonces(coin_json: any): any;

export function createCpfpTx(backup_tx_json: any, coin_json: any, to_address: string, fee_rate_sats_per_byte: number, network: string): string;

export function createDepositMsg1(coin_json: any, token_id: string): any;

export function createSignature(msg: string, client_partial_sig_hex: string, server_partial_sig_hex: string, session_hex: string, output_pubkey_hex: string): string;

export function createTransferReceiverRequestPayload(statechain_info: any, transfer_msg: any, coin: any): any;

export function createTransferSignature(recipient_address: string, input_txid: string, input_vout: number, client_seckey: string): string;

export function createTransferUpdateMsg(x1: string, recipient_address: string, coin_json: any, transfer_signature: string, backup_transactions: any): any;

export function decodeTransferAddress(sc_address: string): any;

export function decryptTransferMsg(encrypted_message: string, private_key_wif: string): any;

export function duplicateCoinToInitializedState(walletJson: any, authPubkey: string): any;

export function fromMnemonic(name: string, mnemonic: string): any;

export function generateMnemonic(): string;

export function getActivityLog(wallet_json: any): any;

export function getAmountFromTx0(tx0_hex: string, tx_outpoint: any): number;

export function getBalance(wallet_json: any): number;

export function getBlockheight(backup_tx: any): number;

export function getCoins(wallet_json: any): any;

export function getMockWallet(): any;

export function getNewCoin(wallet_json: any): any;

export function getNewKeyInfo(server_public_key_hex: string, coin: any, statechain_id: string, tx0_outpoint: any, tx0_hex: string, network: string): any;

export function getOutputAddressFromTx0(tx0_outpoint: any, tx0_hex: string, network: string): string;

export function getPartialSigRequest(coin_json: any, block_height: number, initlock: number, interval: number, fee_rate_sats_per_byte: number, qt_backup_tx: number, to_address: string, network: string, is_withdrawal: boolean): any;

export function getPreviousOutpoint(backup_tx: any): any;

export function getSCAddress(wallet_json: any, index: number, network: string): string;

export function getTokens(wallet_json: any): any;

export function getTx0Outpoint(backup_transactions: any): any;

export function getUserBackupAddress(coin_json: any, network: string): string;

export function handleDepositMsg1Response(coin_json: any, deposit_msg_1_response_json: any): any;

export function isEnclavePubkeyPartOfCoin(coin: any, enclave_pubkey: string): boolean;

export function latestBackuptxPaysToUserpubkey(backup_transactions: any, coin: any, network: string): any;

export function newBackupTransaction(encoded_unsigned_tx: string, signature_hex: string): string;

export function setBlockheight(blockheight: number, wallet_json: any): any;

export function setConfig(config_json: any, wallet_json: any): any;

export function signMessage(statechain_id: string, coin: any): string;

export function validateAddress(address: string, network: string): boolean;

export function validateSignatureScheme(backup_transactions: any, statechain_info: any, tx0_hex: string, current_blockheight: number, fee_rate_tolerance: number, current_fee_rate_sats_per_byte: number, lockheight_init: number, interval: number): any;

export function validateTx0OutputPubkey(enclave_public_key: string, transfer_msg: any, tx0_outpoint: any, tx0_hex: string, network: string): boolean;

export function verifyBlindedMusigScheme(backup_tx: any, tx0_hex: string, statechain_info: any): any;

export function verifyLatestBackupTxPaysToUserPubkey(transfer_msg: any, client_pubkey_share: string, network: string): boolean;

export function verifyTransactionSignature(tx_n_hex: string, tx0_hex: string, fee_rate_tolerance: number, current_fee_rate_sats_per_byte: number): any;

export function verifyTransferSignature(new_user_pubkey: string, tx0_outpoint: any, transfer_msg: any): boolean;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly addToken: (a: any, b: any) => any;
    readonly confirmToken: (a: number, b: number, c: any) => any;
    readonly createAggregatedAddress: (a: any, b: number, c: number) => any;
    readonly createAndCommitNonces: (a: any) => any;
    readonly createCpfpTx: (a: any, b: any, c: number, d: number, e: number, f: number, g: number) => [number, number];
    readonly createDepositMsg1: (a: any, b: number, c: number) => any;
    readonly createSignature: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => [number, number];
    readonly createTransferReceiverRequestPayload: (a: any, b: any, c: any) => any;
    readonly createTransferSignature: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number];
    readonly createTransferUpdateMsg: (a: number, b: number, c: number, d: number, e: any, f: number, g: number, h: any) => any;
    readonly decodeTransferAddress: (a: number, b: number) => any;
    readonly decryptTransferMsg: (a: number, b: number, c: number, d: number) => any;
    readonly duplicateCoinToInitializedState: (a: any, b: number, c: number) => any;
    readonly fromMnemonic: (a: number, b: number, c: number, d: number) => any;
    readonly generateMnemonic: () => [number, number];
    readonly getActivityLog: (a: any) => any;
    readonly getAmountFromTx0: (a: number, b: number, c: any) => number;
    readonly getBalance: (a: any) => number;
    readonly getBlockheight: (a: any) => number;
    readonly getCoins: (a: any) => any;
    readonly getMockWallet: () => any;
    readonly getNewCoin: (a: any) => any;
    readonly getNewKeyInfo: (a: number, b: number, c: any, d: number, e: number, f: any, g: number, h: number, i: number, j: number) => any;
    readonly getOutputAddressFromTx0: (a: any, b: number, c: number, d: number, e: number) => [number, number];
    readonly getPartialSigRequest: (a: any, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number) => any;
    readonly getPreviousOutpoint: (a: any) => any;
    readonly getSCAddress: (a: any, b: number, c: number, d: number) => [number, number];
    readonly getTokens: (a: any) => any;
    readonly getTx0Outpoint: (a: any) => any;
    readonly getUserBackupAddress: (a: any, b: number, c: number) => [number, number];
    readonly handleDepositMsg1Response: (a: any, b: any) => any;
    readonly isEnclavePubkeyPartOfCoin: (a: any, b: number, c: number) => number;
    readonly latestBackuptxPaysToUserpubkey: (a: any, b: any, c: number, d: number) => any;
    readonly newBackupTransaction: (a: number, b: number, c: number, d: number) => [number, number];
    readonly setBlockheight: (a: number, b: any) => any;
    readonly setConfig: (a: any, b: any) => any;
    readonly signMessage: (a: number, b: number, c: any) => [number, number];
    readonly validateAddress: (a: number, b: number, c: number, d: number) => number;
    readonly validateSignatureScheme: (a: any, b: any, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => any;
    readonly validateTx0OutputPubkey: (a: number, b: number, c: any, d: any, e: number, f: number, g: number, h: number) => number;
    readonly verifyBlindedMusigScheme: (a: any, b: number, c: number, d: any) => any;
    readonly verifyLatestBackupTxPaysToUserPubkey: (a: any, b: number, c: number, d: number, e: number) => number;
    readonly verifyTransactionSignature: (a: number, b: number, c: number, d: number, e: number, f: number) => any;
    readonly verifyTransferSignature: (a: number, b: number, c: any, d: any) => number;
    readonly rustsecp256k1zkp_v0_8_1_default_error_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1zkp_v0_8_1_default_illegal_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_8_1_default_error_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_8_1_default_illegal_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_8_1_context_destroy: (a: number) => void;
    readonly rustsecp256k1_v0_8_1_context_create: (a: number) => number;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
