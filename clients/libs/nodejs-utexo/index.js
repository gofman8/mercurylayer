// @mercury/utexo-sdk — UtexoWallet for node, mirroring @buildonspark/spark-sdk's surface on
// Mercury+RGB. Thin JSON-lines client over the mercury-utexo-sdkd daemon (one process per
// wallet), so the full Rust SDK — off-chain splits, branch verification, RGB validation — runs
// under the hood with no protocol reimplementation in JS.

const { spawn } = require('child_process');
const readline = require('readline');
const { EventEmitter } = require('events');

class UtexoWallet extends EventEmitter {
  /**
   * @param {object} options
   *   walletName, network ('regtest'|'mainnet'), statechainEntityUrl?, electrumUrl?,
   *   databaseFile?, rgbProxyUrl?, rgbDataDir?, mnemonic? (restore),
   *   daemonPath? (default: mercury-utexo-sdkd on PATH)
   */
  static async initialize(options) {
    const wallet = new UtexoWallet(options);
    await wallet._start();
    const { mnemonic } = await wallet._call('initialize', {
      wallet_name: options.walletName,
      network: options.network || 'regtest',
      statechain_entity_url: options.statechainEntityUrl,
      electrum_url: options.electrumUrl,
      database_file: options.databaseFile,
      rgb_proxy_url: options.rgbProxyUrl,
      rgb_data_dir: options.rgbDataDir,
      poll_interval_secs: options.pollIntervalSecs,
      mnemonic: options.mnemonic,
    });
    return { wallet, mnemonic };
  }

  /** Restore a wallet from a full recovery bundle (review B7) into a FRESH database. */
  static async importRecoveryBundle(options, bundleJson) {
    const wallet = new UtexoWallet(options);
    await wallet._start();
    const { mnemonic } = await wallet._call('import_recovery_bundle', {
      wallet_name: options.walletName,
      network: options.network || 'regtest',
      statechain_entity_url: options.statechainEntityUrl,
      electrum_url: options.electrumUrl,
      database_file: options.databaseFile,
      rgb_proxy_url: options.rgbProxyUrl,
      rgb_data_dir: options.rgbDataDir,
      poll_interval_secs: options.pollIntervalSecs,
      bundle_json: bundleJson,
    });
    return { wallet, mnemonic };
  }

  constructor(options) {
    super();
    this._options = options;
    this._nextId = 1;
    this._pending = new Map();
  }

  async _start() {
    const bin = this._options.daemonPath || 'mercury-utexo-sdkd';
    this._proc = spawn(bin, [], { stdio: ['pipe', 'pipe', 'inherit'] });
    this._proc.on('exit', (code) => this.emit('daemonExit', code));
    const rl = readline.createInterface({ input: this._proc.stdout });
    rl.on('line', (line) => {
      let msg;
      try { msg = JSON.parse(line); } catch { return; }
      if (msg.event) {
        this.emit(msg.event, msg.data);
        return;
      }
      const waiter = this._pending.get(msg.id);
      if (waiter) {
        this._pending.delete(msg.id);
        if (msg.error !== undefined) waiter.reject(new Error(msg.error));
        else waiter.resolve(msg.result);
      }
    });
  }

  _call(method, params = {}) {
    const id = this._nextId++;
    const line = JSON.stringify({ id, method, params }) + '\n';
    return new Promise((resolve, reject) => {
      this._pending.set(id, { resolve, reject });
      this._proc.stdin.write(line);
    });
  }

  // --- identity & balance -------------------------------------------------------------------
  getUtexoAddress() { return this._call('get_utexo_address'); }
  /** Full backup: wallet record + all backup rows (exit ladder/branch/parents) + RGB seed (B7). */
  exportRecoveryBundle() { return this._call('export_recovery_bundle'); }
  getIdentityPublicKey() { return this._call('get_identity_public_key'); }
  getBalance() { return this._call('get_balance'); }
  getTokenBalances() { return this._call('get_token_balances'); }
  getActivities() { return this._call('get_activities'); }

  // --- deposit ------------------------------------------------------------------------------
  getDepositAddress(amountSats) { return this._call('get_deposit_address', { amount_sats: amountSats }); }
  addPrepaidToken(tokenId) { return this._call('add_prepaid_token', { token_id: tokenId }); }
  claim() { return this._call('claim'); }
  /** Starts the watcher; wallet events are re-emitted on this instance. */
  startBackground() { return this._call('start_background'); }

  // --- send ---------------------------------------------------------------------------------
  transfer({ receiverUtexoAddress, amountSats }) {
    return this._call('transfer', { receiver_address: receiverUtexoAddress, amount_sats: amountSats });
  }
  splitCoin(statechainId, pieceSats) {
    return this._call('split_coin', { statechain_id: statechainId, piece_sats: pieceSats });
  }
  transferTokens({ tokenIdentifier, receiverUtexoAddress, tokenAmount }) {
    return this._call('transfer_tokens', {
      asset_id: tokenIdentifier, receiver_address: receiverUtexoAddress, amount: Number(tokenAmount),
    });
  }

  // --- tokens (issuer) ----------------------------------------------------------------------
  getTokenFundingAddress() { return this._call('get_token_funding_address'); }
  issueToken({ ticker, name, precision, supply }) {
    return this._call('issue_token', { ticker, name, precision, supply: Number(supply) });
  }

  // --- lightning ----------------------------------------------------------------------------
  startLightningSwap({ counterpartyAddress, statechainId }) {
    return this._call('start_lightning_swap', {
      counterparty_address: counterpartyAddress, statechain_id: statechainId,
    });
  }
  getSwapPaymentHash(batchId) { return this._call('get_swap_payment_hash', { batch_id: batchId }); }
  settleLightningSwap(swap) {
    return this._call('settle_lightning_swap', {
      batch_id: swap.batch_id, payment_hash: swap.payment_hash, statechain_id: swap.statechain_id,
    });
  }

  // --- exit ---------------------------------------------------------------------------------
  withdraw({ onchainAddress, statechainIds, feeRate }) {
    return this._call('withdraw', {
      to_address: onchainAddress, statechain_ids: statechainIds, fee_rate: feeRate,
    });
  }
  unilateralExit({ statechainIds } = {}) {
    return this._call('unilateral_exit', { statechain_ids: statechainIds });
  }

  cleanup() { if (this._proc) this._proc.kill(); }
}

module.exports = { UtexoWallet };
