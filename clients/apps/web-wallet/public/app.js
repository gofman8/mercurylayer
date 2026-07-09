// utexo wallet UI — a thin view over the bridge's REST/SSE API (which is itself a 1:1 face on
// @mercury/spark-sdk). No wallet logic lives here: format, render, forward.

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

// ---------------------------------------------------------------- state + rendering

let myAddress = null;

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

const refreshAll = () => { refreshBalance(); refreshActivity(); };

function enterHome(status) {
  myAddress = status.address;
  $('net-pill').textContent = status.network;
  $('wallet-pill').textContent = status.walletName;
  $('my-address').textContent = myAddress;
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

$('tab-spark').onclick = () => {
  $('tab-spark').classList.add('active'); $('tab-onchain').classList.remove('active');
  $('pane-spark').classList.remove('hidden'); $('pane-onchain').classList.add('hidden');
};
$('tab-onchain').onclick = () => {
  $('tab-onchain').classList.add('active'); $('tab-spark').classList.remove('active');
  $('pane-onchain').classList.remove('hidden'); $('pane-spark').classList.add('hidden');
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
