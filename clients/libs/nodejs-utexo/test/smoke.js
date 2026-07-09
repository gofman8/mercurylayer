// Smoke: node binding round-trip against the local regtest stack.
// Prereq: stack up + daemon built (cargo build -p mercury-utexo-sdkd).
// Run from clients/libs/nodejs-utexo:  node test/smoke.js
const { UtexoWallet } = require('../index.js');
const path = require('path');

const DAEMON = path.resolve(__dirname, '../../../../target/debug/mercury-utexo-sdkd');

async function main() {
  const { wallet, mnemonic } = await UtexoWallet.initialize({
    walletName: 'nodejs_smoke_' + Date.now().toString(36),
    network: 'regtest',
    daemonPath: DAEMON,
    databaseFile: '/tmp/nodejs-utexo-smoke.db',
  });
  console.log('mnemonic words:', mnemonic.split(' ').length);

  const address = await wallet.getUtexoAddress();
  if (!address.startsWith('tml1')) throw new Error('bad address: ' + address);
  console.log('address:', address);

  const balance = await wallet.getBalance();
  if (balance.available_sats !== 0) throw new Error('fresh wallet should be empty');
  console.log('balance:', balance);

  const deposit = await wallet.getDepositAddress(50000);
  if (!deposit.startsWith('bcrt1')) throw new Error('bad deposit address: ' + deposit);
  console.log('deposit address:', deposit);

  // error surface: over-balance transfer is a typed refusal
  let refused = false;
  try {
    await wallet.transfer({ receiverUtexoAddress: address, amountSats: 10000 });
  } catch (e) {
    refused = /insufficient balance/.test(e.message);
  }
  if (!refused) throw new Error('expected insufficient-balance refusal');
  console.log('over-balance transfer refused as expected');

  wallet.cleanup();
  console.log('NODEJS SMOKE - SUCCESS: initialize / address / balance / deposit-address / typed error, all through the daemon.');
  process.exit(0);
}

main().catch((e) => { console.error('SMOKE FAILED:', e); process.exit(1); });
