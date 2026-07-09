# utexo web wallet

An arkade-style self-custodial web wallet built ENTIRELY on our own stack — no third-party wallet
libraries, no framework, no npm dependencies:

```
browser SPA (public/, vanilla JS)
   │  REST + SSE
server.js (zero-dep Node bridge, 1:1 method forwarding)
   │  @mercury/utexo-sdk  (clients/libs/nodejs-utexo)
mercury-utexo-sdkd (JSON-lines daemon)
   │
mercury-utexo-sdk (Rust) — off-chain splits, receiver verification, auto-refresh,
                            watchtower, RGB — against the SE + electrum
```

## Run (regtest)

```sh
cargo build -p mercury-utexo-sdkd          # once, from the repo root
node clients/apps/web-wallet/server.js     # http://localhost:8787
```

Env: `PORT` (8787), `ML_NETWORK` (regtest), `DATA_DIR` (./data), `ML_DAEMON` (path to the daemon),
`SE_URL`/`ELECTRUM_URL` (mainnet overrides). One wallet per instance; run two instances with
different `PORT`+`DATA_DIR` to demo transfers.

## What the UI covers

- create / restore (12-word phrase) with an honest backup notice (the phrase restores keys; the
  `DATA_DIR` holds off-chain exit material — back up both)
- balance (available / incoming), activity feed
- receive: instant spark address (QR) or exact-amount on-chain deposit (QR, `bitcoin:` URI)
- send: instant, free, exact-to-the-sat statechain transfers (auto-split under the hood)
- live toasts from SDK events: deposits confirming, incoming transfers auto-claiming,
  auto-refresh, watchtower warnings

The bridge exposes only what it uses: `/api/status,wallet,balance,activities,deposit,send,
withdraw,claim,events`. Vendored: `public/vendor/qrcode.js` (MIT, Kazuhiko Arase) for QR rendering.
