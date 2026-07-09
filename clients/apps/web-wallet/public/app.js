// utexo wallet UI — a thin view over the bridge's REST/SSE API (which is itself a 1:1 face on
// @mercury/utexo-sdk). No wallet logic lives here: format, render, forward.

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- helpers

async function api(path, opts) {
  const res = await fetch(path, opts && {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(opts),
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || `${res.status}`);
  return body;
}

const fmtSats = (n) => Number(n || 0).toLocaleString('en-US').replace(/,/g, ' ');
const fmtBtc = (n) => (Number(n || 0) / 1e8).toFixed(8) + ' BTC';
const shortAddr = (a) => (a && a.length > 26) ? `${a.slice(0, 14)}…${a.slice(-10)}` : (a || '');

function relTime(iso) {
  const t = new Date(iso).getTime();
  if (!isFinite(t)) return '';
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return new Date(iso).toLocaleDateString();
}

function toast(msg, cls = '') {
  const el = document.createElement('div');
  el.className = `toast ${cls}`;
  el.textContent = msg;
  $('toasts').appendChild(el);
  setTimeout(() => el.remove(), 5000);
}

function copy(text, note = 'Copied') {
  navigator.clipboard?.writeText(text).then(() => toast(note, 'good'))
    .catch(() => toast('Copy failed — select manually', 'bad'));
}

function drawQr(el, text) {
  const qr = qrcode(0, 'M');
  qr.addData(text);
  qr.make();
  el.innerHTML = qr.createSvgTag({ cellSize: 4, margin: 0 });
}

function show(screen) {
  for (const s of document.querySelectorAll('.screen')) s.classList.add('hidden');
  $(screen).classList.remove('hidden');
}

// ---------------------------------------------------------------- asset units
// TokenBalance amounts are RAW units scaled by `precision`. Convert via BigInt so a
// large supply never hits float rounding; the wire still carries a safe integer.

function toRawUnits(str, precision) {
  const s = String(str).trim();
  if (!/^\d+(\.\d+)?$/.test(s)) throw new Error('Enter a plain number, e.g. 12.50');
  const [ints, frac = ''] = s.split('.');
  if (frac.length > precision) throw new Error(`This asset has ${precision} decimal place${precision === 1 ? '' : 's'} max`);
  const raw = BigInt(ints + frac.padEnd(precision, '0'));
  if (raw <= 0n) throw new Error('Amount must be positive');
  if (raw > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error('Amount too large');
  return Number(raw);
}

const groupDigits = (s) => s.replace(/\B(?=(\d{3})+(?!\d))/g, ' ');

function fromRawUnits(raw, precision) {
  const s = BigInt(raw ?? 0).toString().padStart(precision + 1, '0');
  if (!precision) return groupDigits(s);
  const ints = s.slice(0, -precision);
  const frac = s.slice(-precision).replace(/0+$/, '');
  return groupDigits(ints) + (frac ? '.' + frac : '');
}

// Deterministic badge colour per asset id — assets stay visually recognisable.
function badgeHue(id) {
  let h = 0;
  for (const c of String(id)) h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}
function paintBadge(el, t) {
  el.textContent = (t.ticker || '??').slice(0, 3);
  const h = badgeHue(t.asset_id);
  el.style.background = `linear-gradient(140deg, hsl(${h} 70% 62%), hsl(${(h + 40) % 360} 75% 55%))`;
}

// ---------------------------------------------------------------- state + rendering

let myAddress = null;
let tokensEnabled = true; // from /api/status — mainnet without an RGB proxy runs sats-only
let tokens = [];          // last fetched token balances
let currentAsset = null;  // asset open in the asset sheet
// In-flight guards: closing a sheet does NOT abort the SDK call behind it, so reopening must
// land back on the progress pane — never on a resubmittable form (review-found double-spend).
const busy = { issue: false, send: false, sendAsset: null };

async function refreshBalance() {
  try {
    const b = await api('/api/balance');
    $('balance-sats').textContent = fmtSats(b.available_sats);
    $('balance-btc').textContent = fmtBtc(b.available_sats);
    const pending = (b.pending_sats || 0) + (b.in_transfer_sats || 0);
    $('balance-pending').classList.toggle('hidden', pending === 0);
    if (pending) $('balance-pending').textContent = `+ ${fmtSats(pending)} sats incoming`;
  } catch { /* wallet not open yet */ }
}

// Keyed lowercase — the SDK's action strings are mixed-case ("deposit" vs "Transfer").
const ACT_STYLE = {
  deposit:  { icon: '↓', cls: 'in',  title: 'Deposit' },
  receive:  { icon: '↓', cls: 'in',  title: 'Received' },
  transfer: { icon: '↑', cls: 'out', title: 'Sent' },
  withdraw: { icon: '⇱', cls: 'out', title: 'Withdrawn' },
};

async function refreshActivity() {
  try {
    const acts = await api('/api/activities');
    const list = $('activity-list');
    list.innerHTML = '';
    $('activity-empty').classList.toggle('hidden', acts.length > 0);
    for (const a of acts.slice(0, 30)) {
      const st = ACT_STYLE[(a.action || '').toLowerCase()] || { icon: '·', cls: '', title: a.action };
      const li = document.createElement('li');
      li.innerHTML = `
        <div class="act-icon ${st.cls}">${st.icon}</div>
        <div class="act-main">
          <div class="act-title">${st.title}</div>
          <div class="act-sub">${relTime(a.date)}</div>
        </div>
        <div class="act-amount ${st.cls}">${st.cls === 'in' ? '+' : '−'}${fmtSats(a.amount)}</div>`;
      list.appendChild(li);
    }
  } catch { /* wallet not open yet */ }
}

async function refreshTokens() {
  if (!tokensEnabled) return;
  try {
    tokens = await api('/api/tokens');
    const list = $('asset-list');
    list.innerHTML = '';
    $('asset-empty').classList.toggle('hidden', tokens.length > 0);
    for (const t of tokens) {
      const li = document.createElement('li');
      const badge = document.createElement('div');
      badge.className = 'asset-badge';
      paintBadge(badge, t);
      const unsettled = (t.total || 0) - (t.balance || 0);
      const main = document.createElement('div');
      main.className = 'asset-main';
      main.innerHTML = `
        <div class="asset-name"></div>
        <div class="asset-sub"></div>`;
      main.querySelector('.asset-name').textContent = t.name || t.ticker || 'asset';
      main.querySelector('.asset-sub').textContent =
        (t.ticker || '') + (unsettled > 0 ? ` · +${fromRawUnits(unsettled, t.precision)} incoming` : '');
      const amount = document.createElement('div');
      amount.className = 'asset-amount';
      amount.textContent = fromRawUnits(t.balance, t.precision);
      li.append(badge, main, amount);
      li.onclick = () => openAsset(t);
      list.appendChild(li);
    }
    // Keep an OPEN asset sheet honest: re-render its hero from the fresh balances (it would
    // otherwise show a pre-send/pre-receive snapshot until closed and reopened).
    if (currentAsset && !$('modal-asset').classList.contains('hidden')) {
      const fresh = tokens.find((x) => x.asset_id === currentAsset.asset_id);
      if (fresh) {
        currentAsset = fresh;
        renderAssetHero(fresh);
      }
    }
  } catch { /* wallet not open yet */ }
}

const refreshAll = () => { refreshBalance(); refreshActivity(); refreshTokens(); };

function enterHome(status) {
  myAddress = status.address;
  tokensEnabled = status.tokensEnabled !== false;
  $('net-pill').textContent = status.network;
  $('wallet-pill').textContent = status.walletName;
  $('my-address').textContent = myAddress;
  // Sats-only deployment (no RGB proxy configured): hide the assets feature entirely rather
  // than advertise receives that could never be booked.
  $('assets-card').classList.toggle('hidden', !tokensEnabled);
  $('receive-assets-hint').classList.toggle('hidden', !tokensEnabled);
  $('receive-sats-hint').classList.toggle('hidden', tokensEnabled);
  show('screen-home');
  refreshAll();
}

// ---------------------------------------------------------------- events (SSE)

function connectEvents() {
  const es = new EventSource('/api/events');
  es.addEventListener('DepositConfirmed', (e) => {
    const d = JSON.parse(e.data);
    toast(`Deposit confirmed: +${fmtSats(d.amount_sats)} sats`, 'good');
    refreshAll();
  });
  es.addEventListener('TransferClaimed', () => {
    toast('Incoming transfer received', 'good');
    refreshAll();
  });
  es.addEventListener('BalanceUpdate', refreshBalance);
  es.addEventListener('TokenTransferClaimed', async (e) => {
    const d = JSON.parse(e.data);
    await refreshTokens(); // learn the asset's ticker/precision before announcing it
    const t = tokens.find((x) => x.asset_id === d.asset_id);
    toast(`Received ${t ? fromRawUnits(d.amount, t.precision) : d.amount} ${t?.ticker || 'asset units'}`, 'good');
  });
  es.addEventListener('CoinRefreshed', (e) => {
    const d = JSON.parse(e.data);
    toast(`Coin auto-refreshed (fee ${fmtSats(d.fee_sats)} sats)`);
    refreshAll();
  });
  es.addEventListener('ExitDeadlineApproaching', () => toast('Watchtower: exit deadline near — acting', 'bad'));
  es.addEventListener('ExitBranchConflict', () => toast('Exit branch conflict — check the wallet!', 'bad'));
  es.addEventListener('DaemonExit', () => toast('Wallet daemon stopped', 'bad'));
  es.onerror = () => { /* EventSource auto-reconnects */ };
}

// ---------------------------------------------------------------- onboarding

$('btn-show-restore').onclick = () => $('restore-panel').classList.toggle('hidden');

async function createOrRestore(mnemonic) {
  const err = $('onboard-error');
  err.classList.add('hidden');
  const walletName = $('wallet-name').value.trim();
  try {
    $('btn-create').disabled = $('btn-restore').disabled = true;
    const out = await api('/api/wallet', { walletName, mnemonic });
    const status = await api('/api/status');
    if (!mnemonic && out.mnemonic) {
      const grid = $('mnemonic-grid');
      grid.innerHTML = '';
      for (const w of out.mnemonic.split(' ')) {
        const li = document.createElement('li');
        li.textContent = w;
        grid.appendChild(li);
      }
      $('btn-copy-mnemonic').onclick = () => copy(out.mnemonic, 'Phrase copied — store it safely');
      $('btn-backup-done').onclick = () => enterHome(status);
      show('screen-backup');
    } else {
      enterHome(status);
    }
  } catch (e) {
    err.textContent = e.message;
    err.classList.remove('hidden');
  } finally {
    $('btn-create').disabled = $('btn-restore').disabled = false;
  }
}

$('btn-create').onclick = () => createOrRestore(null);
$('btn-restore').onclick = () => {
  const words = $('restore-mnemonic').value.trim().replace(/\s+/g, ' ');
  if (words.split(' ').length !== 12) {
    $('onboard-error').textContent = 'A recovery phrase is exactly 12 words.';
    $('onboard-error').classList.remove('hidden');
    return;
  }
  createOrRestore(words);
};

// ---------------------------------------------------------------- receive

$('btn-receive').onclick = () => {
  $('modal-receive').classList.remove('hidden');
  $('spark-address').textContent = myAddress;
  drawQr($('qr-spark'), myAddress);
};

$('tab-utexo').onclick = () => {
  $('tab-utexo').classList.add('active'); $('tab-onchain').classList.remove('active');
  $('pane-utexo').classList.remove('hidden'); $('pane-onchain').classList.add('hidden');
};
$('tab-onchain').onclick = () => {
  $('tab-onchain').classList.add('active'); $('tab-utexo').classList.remove('active');
  $('pane-onchain').classList.remove('hidden'); $('pane-utexo').classList.add('hidden');
};

$('btn-gen-deposit').onclick = async () => {
  const err = $('receive-error');
  err.classList.add('hidden');
  const amountSats = Number($('deposit-amount').value);
  try {
    $('btn-gen-deposit').disabled = true;
    const { address } = await api('/api/deposit', { amountSats });
    $('deposit-address').textContent = address;
    $('deposit-exact').textContent = fmtSats(amountSats);
    drawQr($('qr-deposit'), `bitcoin:${address}?amount=${(amountSats / 1e8).toFixed(8)}`);
    $('deposit-result').classList.remove('hidden');
  } catch (e) {
    err.textContent = e.message;
    err.classList.remove('hidden');
  } finally {
    $('btn-gen-deposit').disabled = false;
  }
};

// ---------------------------------------------------------------- send

$('btn-send').onclick = () => {
  $('send-form').classList.remove('hidden');
  $('send-progress').classList.add('hidden');
  $('send-done').classList.add('hidden');
  $('send-error').classList.add('hidden');
  $('modal-send').classList.remove('hidden');
};

$('btn-do-send').onclick = async () => {
  const address = $('send-address').value.trim();
  const amountSats = Number($('send-amount').value);
  const err = $('send-error');
  err.classList.add('hidden');
  if (!/^(t?ml1|t?sp1)/.test(address)) {
    err.textContent = 'That does not look like a spark address.';
    err.classList.remove('hidden');
    return;
  }
  $('send-form').classList.add('hidden');
  $('send-progress').classList.remove('hidden');
  try {
    const res = await api('/api/send', { address, amountSats });
    $('send-progress').classList.add('hidden');
    $('send-summary').textContent = `Sent ${fmtSats(res.total_sats)} sats`;
    $('send-done').classList.remove('hidden');
    $('send-address').value = ''; $('send-amount').value = '';
    refreshAll();
  } catch (e) {
    $('send-progress').classList.add('hidden');
    $('send-form').classList.remove('hidden');
    err.textContent = e.message;
    err.classList.remove('hidden');
  }
};

// ---------------------------------------------------------------- assets: issue

$('btn-issue').onclick = async () => {
  // An issuance is still running (its sheet was closed over it): reopen ON the progress pane —
  // resetting to the form would invite a duplicate on-chain issuance (review-found).
  $('issue-form').classList.toggle('hidden', busy.issue);
  $('issue-progress').classList.toggle('hidden', !busy.issue);
  $('issue-done').classList.add('hidden');
  $('issue-error').classList.add('hidden');
  $('modal-issue').classList.remove('hidden');
  if (busy.issue) return;
  try {
    const { address } = await api('/api/tokens/funding-address');
    $('funding-address').textContent = address;
  } catch (e) {
    $('funding-address').textContent = `unavailable: ${e.message}`;
  }
};

$('issue-ticker').addEventListener('input', (e) => { e.target.value = e.target.value.toUpperCase(); });

$('btn-do-issue').onclick = async () => {
  if (busy.issue) return;
  const err = $('issue-error');
  err.classList.add('hidden');
  try {
    // Validate the RAW string: an emptied number input yields '' and Number('') === 0, which
    // would silently mint an irreversible 0-decimals asset (review-found).
    const precisionRaw = $('issue-precision').value.trim();
    if (!/^\d+$/.test(precisionRaw) || Number(precisionRaw) > 8) {
      throw new Error('Decimals: a number from 0 to 8');
    }
    const precision = Number(precisionRaw);
    const payload = {
      ticker: $('issue-ticker').value.trim(),
      name: $('issue-name').value.trim(),
      precision,
      supply: toRawUnits($('issue-supply').value, precision),
    };
    busy.issue = true;
    $('issue-form').classList.add('hidden');
    $('issue-progress').classList.remove('hidden');
    await api('/api/tokens/issue', payload);
    busy.issue = false;
    $('issue-progress').classList.add('hidden');
    $('issue-summary').textContent =
      `${payload.ticker} issued — the full supply is on a statechain coin, ready to send off-chain.`;
    $('issue-done').classList.remove('hidden');
    refreshAll();
  } catch (e) {
    busy.issue = false;
    $('issue-progress').classList.add('hidden');
    $('issue-form').classList.remove('hidden');
    err.textContent = e.message;
    err.classList.remove('hidden');
  }
};

// ---------------------------------------------------------------- assets: details + send

/// Hero portion of the asset sheet (badge, balance, incoming chip) — rendered on open AND
/// re-rendered by refreshTokens while the sheet stays open, so the balance never goes stale.
function renderAssetHero(t) {
  $('asset-title').textContent = t.name || t.ticker || 'Asset';
  paintBadge($('asset-badge'), t);
  $('asset-balance').textContent = fromRawUnits(t.balance, t.precision);
  $('asset-ticker-lbl').textContent = t.ticker || '';
  const unsettled = (BigInt(t.total || 0) - BigInt(t.balance || 0));
  $('asset-pending').classList.toggle('hidden', unsettled <= 0n);
  if (unsettled > 0n) $('asset-pending').textContent = `+ ${fromRawUnits(unsettled, t.precision)} incoming`;
  $('asset-id').textContent = t.asset_id;
}

function openAsset(t) {
  // A send is still in flight (sheet was closed over it): reopen the SENDING asset on its
  // progress pane instead of a resubmittable form — a retry here would transfer twice.
  if (busy.send) t = busy.sendAsset;
  currentAsset = t;
  renderAssetHero(t);
  $('asset-send-form').classList.toggle('hidden', busy.send);
  $('asset-send-progress').classList.toggle('hidden', !busy.send);
  $('asset-send-done').classList.add('hidden');
  $('asset-send-error').classList.add('hidden');
  $('modal-asset').classList.remove('hidden');
}

$('btn-do-asset-send').onclick = async () => {
  if (busy.send) return;
  const err = $('asset-send-error');
  err.classList.add('hidden');
  const address = $('asset-send-address').value.trim();
  if (!/^(t?ml1|t?sp1)/.test(address)) {
    err.textContent = 'That does not look like a spark address.';
    err.classList.remove('hidden');
    return;
  }
  const asset = currentAsset;
  try {
    const amount = toRawUnits($('asset-send-amount').value, asset.precision);
    busy.send = true;
    busy.sendAsset = asset;
    $('asset-send-form').classList.add('hidden');
    $('asset-send-progress').classList.remove('hidden');
    await api('/api/tokens/send', { assetId: asset.asset_id, address, amount });
    busy.send = false;
    busy.sendAsset = null;
    $('asset-send-progress').classList.add('hidden');
    $('asset-send-summary').textContent =
      `Sent ${fromRawUnits(amount, asset.precision)} ${asset.ticker || ''}`;
    $('asset-send-done').classList.remove('hidden');
    $('asset-send-address').value = ''; $('asset-send-amount').value = '';
    refreshAll();
  } catch (e) {
    busy.send = false;
    busy.sendAsset = null;
    $('asset-send-progress').classList.add('hidden');
    $('asset-send-form').classList.remove('hidden');
    err.textContent = e.message;
    err.classList.remove('hidden');
  }
};

// ---------------------------------------------------------------- generic modal wiring

for (const b of document.querySelectorAll('[data-close]')) {
  b.onclick = () => $(b.dataset.close).classList.add('hidden');
}
for (const o of document.querySelectorAll('.overlay')) {
  o.addEventListener('click', (e) => { if (e.target === o) o.classList.add('hidden'); });
}
document.addEventListener('click', (e) => {
  const t = e.target.closest('[data-copy]');
  if (t) copy($(t.dataset.copy).textContent);
});
$('btn-copy-address').onclick = () => copy(myAddress, 'Address copied');

// ---------------------------------------------------------------- boot

(async function boot() {
  connectEvents();
  setInterval(refreshAll, 5000);
  try {
    const status = await api('/api/status');
    if (status.initialized) return enterHome(status);
  } catch { /* server starting */ }
  show('screen-onboard');
})();
