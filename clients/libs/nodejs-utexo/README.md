# @mercury/utexo-sdk (node)

Spark-parity wallet SDK for node, mirroring `@buildonspark/spark-sdk`'s method surface
(camelCase) on Mercury+RGB. It is a thin JSON-lines client over the `mercury-utexo-sdkd`
daemon, so the complete Rust SDK — off-chain splits, branch verification, RGB validation —
runs underneath with no protocol reimplementation in JS.

```js
const { UtexoWallet } = require('@mercury/utexo-sdk');

const { wallet, mnemonic } = await UtexoWallet.initialize({
  walletName: 'alice', network: 'regtest',
  daemonPath: '/path/to/mercury-utexo-sdkd',      // cargo build -p mercury-utexo-sdkd
});
const address = await wallet.getUtexoAddress();
await wallet.startBackground();                    // wallet re-emits SDK events
wallet.on('TransferClaimed', (d) => console.log('claimed', d.statechain_ids));
await wallet.transfer({ receiverUtexoAddress: bob, amountSats: 15000 });
```

- Full method list mirrors the Rust SDK (see `docs/utexo/build/api-reference.md`).
- Wire protocol: one JSON object per line on the daemon's stdio —
  `{"id","method","params"}` → `{"id","result"|"error"}`, plus `{"event","data"}` pushes.
  The protocol is verified in CI-able form by driving the daemon directly (see the repo's
  testing guide); `test/smoke.js` runs the same checks where a node runtime is available.
- Web/wasm: the daemon boundary is the integration point — a WebSocket transport in front of
  the same protocol serves browser wallets (roadmap; the mercury-wasm crate keeps covering
  vanilla flows).
