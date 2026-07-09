// web-wallet bridge — a zero-dependency Node server that puts an HTTP/SSE face on
// @mercury/utexo-sdk (which itself is a thin JSON-lines client over mercury-utexo-sdkd, so the
// complete Rust SDK — off-chain splits, branch verification, auto-refresh, watchtower — runs
// underneath). No protocol logic lives here: every route is a 1:1 forward to an SDK method.
//
//   ~/tools/node/bin/node server.js
//
// Env:
//   PORT        HTTP port (default 8787)
//   ML_DAEMON   path to mercury-utexo-sdkd (default: ../../../target/debug/mercury-utexo-sdkd)
//   ML_NETWORK  regtest | mainnet (default regtest)
//   DATA_DIR    wallet databases + RGB state (default ./data)  ← BACK THIS UP with the mnemonic
//   SE_URL / ELECTRUM_URL  overrides for mainnet
//
// One active wallet per server instance (one daemon process per wallet). The last-opened wallet
// name is remembered in DATA_DIR/wallet.json and re-opened on boot, so a browser refresh or a
// server restart lands back in the wallet.

const http = require('http');
const fs = require('fs');
const path = require('path');
const { UtexoWallet } = require('../../libs/nodejs-utexo');

const PORT = Number(process.env.PORT || 8787);
const NETWORK = process.env.ML_NETWORK || 'regtest';
const DATA_DIR = path.resolve(process.env.DATA_DIR || path.join(__dirname, 'data'));
const DAEMON = process.env.ML_DAEMON
  || path.resolve(__dirname, '../../../target/debug/mercury-utexo-sdkd');
const PUBLIC_DIR = path.join(__dirname, 'public');
const MARKER = path.join(DATA_DIR, 'wallet.json');

const state = {
  wallet: null,       // UtexoWallet
  walletName: null,
  address: null,
  fundingAddress: null, // cached RGB funding address — every fetch REVEALS a new index, and the
                        // engine's fast sync only watches a small window of recent reveals, so
                        // handing out a fresh one per request would orphan already-funded ones
  tokensEnabled: false, // RGB assets live? (regtest default, or RGB_PROXY_URL supplied)
  sse: new Set(),     // live event-stream responses
};

// ---------------------------------------------------------------------------- SSE plumbing

function pushEvent(event, data) {
  const line = `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
  for (const res of state.sse) res.write(line);
}

const FORWARDED_EVENTS = [
  'DepositConfirmed', 'TransferClaimed', 'BalanceUpdate', 'TokenTransferClaimed',
  'CoinRefreshed', 'ExitDeadlineApproaching', 'ExitBranchConflict', 'TokenCarrierMaterialized',
];

// RGB asset rules (rgb-lib NIA): ticker 1-8 uppercase ASCII, name 1-40 chars, precision 0-18.
const TICKER_RE = /^[A-Z][A-Z0-9]{0,7}$/;

// ---------------------------------------------------------------------------- wallet lifecycle

async function openWallet(walletName, mnemonic) {
  if (state.wallet) {
    if (state.walletName === walletName) {
      return { mnemonic: null, address: state.address }; // already open
    }
    throw new Error(`this instance already serves wallet '${state.walletName}' — one wallet per instance`);
  }
  fs.mkdirSync(DATA_DIR, { recursive: true });
  const opts = {
    walletName,
    network: NETWORK,
    daemonPath: DAEMON,
    databaseFile: path.join(DATA_DIR, `${walletName}.db`),
    rgbDataDir: path.join(DATA_DIR, `rgb-${walletName}`),
  };
  if (process.env.SE_URL) opts.statechainEntityUrl = process.env.SE_URL;
  if (process.env.ELECTRUM_URL) opts.electrumUrl = process.env.ELECTRUM_URL;
  // The SDK defaults an RGB proxy only on regtest; on mainnet token support silently stays off
  // unless the operator supplies one — so plumb it through and tell the UI (via /api/status)
  // whether assets are live, instead of advertising a feature that can never book a transfer.
  if (process.env.RGB_PROXY_URL) opts.rgbProxyUrl = process.env.RGB_PROXY_URL;
  state.tokensEnabled = NETWORK === 'regtest' || !!process.env.RGB_PROXY_URL;
  if (mnemonic) opts.mnemonic = mnemonic;

  const { wallet, mnemonic: out } = await UtexoWallet.initialize(opts);
  for (const ev of FORWARDED_EVENTS) wallet.on(ev, (data) => pushEvent(ev, data || {}));
  wallet.on('daemonExit', (code) => pushEvent('DaemonExit', { code }));
  await wallet.startBackground(); // deposits confirm + incoming transfers claim automatically

  state.wallet = wallet;
  state.walletName = walletName;
  state.address = await wallet.getUtexoAddress();
  fs.writeFileSync(MARKER, JSON.stringify({ walletName }));
  return { mnemonic: out, address: state.address };
}

// Re-open the last wallet on boot so refresh/restart lands back in it.
async function reopenLastWallet() {
  try {
    const { walletName } = JSON.parse(fs.readFileSync(MARKER, 'utf8'));
    if (walletName) {
      await openWallet(walletName, null);
      console.log(`re-opened wallet '${walletName}'`);
    }
  } catch { /* no marker — fresh install */ }
}

// ---------------------------------------------------------------------------- http helpers

function json(res, code, body) {
  const s = JSON.stringify(body);
  res.writeHead(code, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(s) });
  res.end(s);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    let data = '';
    req.on('data', (c) => { data += c; if (data.length > 1e6) reject(new Error('body too large')); });
    req.on('end', () => {
      try { resolve(data ? JSON.parse(data) : {}); } catch (e) { reject(new Error('invalid JSON body')); }
    });
    req.on('error', reject);
  });
}

const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png' };

function serveStatic(res, urlPath) {
  const rel = urlPath === '/' ? '/index.html' : urlPath;
  const file = path.normalize(path.join(PUBLIC_DIR, rel));
  if (!file.startsWith(PUBLIC_DIR) || !fs.existsSync(file) || !fs.statSync(file).isFile()) {
    res.writeHead(404); res.end('not found'); return;
  }
  res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream' });
  fs.createReadStream(file).pipe(res);
}

function needWallet() {
  if (!state.wallet) { const e = new Error('no wallet open — create or restore one first'); e.code = 409; throw e; }
  return state.wallet;
}

// ---------------------------------------------------------------------------- routes

async function api(req, res, url) {
  const route = `${req.method} ${url.pathname}`;
  switch (route) {
    case 'GET /api/status':
      return json(res, 200, {
        initialized: !!state.wallet,
        walletName: state.walletName,
        network: NETWORK,
        address: state.address,
        tokensEnabled: state.tokensEnabled,
      });

    case 'POST /api/wallet': {
      const { walletName, mnemonic } = await readBody(req);
      if (!walletName || !/^[a-zA-Z0-9_-]{1,40}$/.test(walletName)) {
        return json(res, 400, { error: 'walletName: 1-40 chars, letters/digits/_/-' });
      }
      const out = await openWallet(walletName, mnemonic || null);
      return json(res, 200, { walletName, ...out });
    }

    case 'GET /api/balance':
      return json(res, 200, await needWallet().getBalance());

    case 'GET /api/activities': {
      const acts = await needWallet().getActivities();
      return json(res, 200, acts.slice().reverse()); // newest first
    }

    case 'POST /api/deposit': {
      const { amountSats } = await readBody(req);
      if (!Number.isInteger(amountSats) || amountSats <= 0) return json(res, 400, { error: 'amountSats: positive integer required' });
      const address = await needWallet().getDepositAddress(amountSats);
      return json(res, 200, { address, amountSats });
    }

    case 'POST /api/send': {
      const { address, amountSats } = await readBody(req);
      if (!address) return json(res, 400, { error: 'address required' });
      if (!Number.isInteger(amountSats) || amountSats <= 0) return json(res, 400, { error: 'amountSats: positive integer required' });
      const result = await needWallet().transfer({ receiverUtexoAddress: address, amountSats });
      pushEvent('Sent', result);
      return json(res, 200, result);
    }

    case 'POST /api/withdraw': {
      const { address, feeRate } = await readBody(req);
      if (!address) return json(res, 400, { error: 'address required' });
      // nodejs-utexo mirrors Spark's naming: the param is `onchainAddress` (review-found: passing
      // `toAddress` made every withdrawal die with "param 'to_address' required").
      const txids = await needWallet().withdraw({ onchainAddress: address, feeRate: feeRate || null });
      return json(res, 200, { txids });
    }

    case 'POST /api/claim':
      return json(res, 200, await needWallet().claim());

    // ---------------- RGB assets (raw units on the wire; the UI scales by `precision`) ----------

    case 'GET /api/tokens':
      return json(res, 200, await needWallet().getTokenBalances());

    case 'GET /api/tokens/funding-address': {
      if (!state.fundingAddress) state.fundingAddress = await needWallet().getTokenFundingAddress();
      return json(res, 200, { address: state.fundingAddress });
    }

    case 'POST /api/tokens/issue': {
      const { ticker, name, precision, supply } = await readBody(req);
      if (!TICKER_RE.test(ticker || '')) return json(res, 400, { error: 'ticker: 1-8 uppercase letters/digits, starting with a letter' });
      if (!name || name.length > 40) return json(res, 400, { error: 'name: 1-40 characters' });
      // rgb-lib allows precision up to 18, but amounts cross this bridge as JSON numbers, which
      // round above 2^53 raw units — precision 8 keeps ~90M whole tokens safely representable.
      if (!Number.isInteger(precision) || precision < 0 || precision > 8) return json(res, 400, { error: 'precision: integer 0-8' });
      if (!Number.isSafeInteger(supply) || supply <= 0) return json(res, 400, { error: 'supply: positive integer (raw units)' });
      // One on-chain tx (the colored carrier deposit) — takes a few seconds on regtest.
      const assetId = await needWallet().issueToken({ ticker, name, precision, supply });
      pushEvent('AssetIssued', { assetId, ticker });
      return json(res, 200, { assetId });
    }

    case 'POST /api/tokens/send': {
      const { assetId, address, amount } = await readBody(req);
      if (!assetId) return json(res, 400, { error: 'assetId required' });
      if (!address) return json(res, 400, { error: 'address required' });
      if (!Number.isSafeInteger(amount) || amount <= 0) return json(res, 400, { error: 'amount: positive integer (raw units)' });
      const result = await needWallet().transferTokens({
        tokenIdentifier: assetId, receiverUtexoAddress: address, tokenAmount: amount,
      });
      pushEvent('TokenSent', { assetId, amount });
      return json(res, 200, result);
    }

    case 'GET /api/events': {
      res.writeHead(200, {
        'Content-Type': 'text/event-stream',
        'Cache-Control': 'no-cache',
        Connection: 'keep-alive',
      });
      res.write('event: hello\ndata: {}\n\n');
      state.sse.add(res);
      req.on('close', () => state.sse.delete(res));
      return; // stream stays open
    }

    default:
      return json(res, 404, { error: `no route ${route}` });
  }
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  try {
    if (url.pathname.startsWith('/api/')) return await api(req, res, url);
    return serveStatic(res, url.pathname);
  } catch (e) {
    return json(res, e.code === 409 ? 409 : 500, { error: e.message || String(e) });
  }
});

server.listen(PORT, async () => {
  console.log(`web-wallet on http://localhost:${PORT} (network=${NETWORK}, data=${DATA_DIR})`);
  console.log(`daemon: ${DAEMON}`);
  await reopenLastWallet();
});
