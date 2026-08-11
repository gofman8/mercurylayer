# WP1 spike — TRUC ancestor limit and package-CPFP rescue, measured

Run 2026-08-11 against **Bitcoin Core 30.2.0** (`/Satoshi:30.2.0/`), on an **isolated regtest node**
started inside the existing bitcoind container with its own datadir and ports
(`-datadir=/tmp/spike -port=19444 -rpcport=19555`) so the shared stack was never perturbed, and with
**`-minrelaytxfee=0.00003`** — i.e. a mempool floor of **3 sat/vB, above the protocol's committed
2.0 sat/vB** (`lib/src/tesr.rs:210`, `:215`). That is the regime the whole question is about.

AUTHOR-ATTESTED: reproducible from this note, not from CI.

This retires what `SPEC-ROADMAP.md` §7/WP1 called **"the single largest UNVERIFIED block in the
round-2 corpus"** — every TRUC/BIP-431 claim in the corpus was a reading of Core's policy, and no
test in the tree had ever put two transactions of one exit chain in a mempool simultaneously (the
E2E harness mines between every tier, `clients/tests/rust/src/sdk69_transfer_many_inladder.rs:114`).

---

## 1. The TRUC ancestor limit — Core's verbatim refusal

Three v3 transactions chained, each spending the previous while it is still unconfirmed:

| tx | unconfirmed ancestors | result |
|---|---|---|
| A | 0 | **accepted** |
| B | 1 (A) | **accepted** |
| C | 2 (B, A) | **REJECTED** |

```
error code: -26
TRUC-violation, tx <txid> (wtxid=<wtxid>) would have too many ancestors
```

**So a TES-R exit walk cannot be submitted as one chain.** Two v3 transactions may be in flight at
once; the third is refused. The walk must proceed **sequentially — each tier confirming before the
next enters the mempool.**

**This is the load-bearing consequence, and it is a normative one.** `PROTOCOL.md:245-247` says "each
tier confirms before the next is valid, so no long vulnerable chains". For the CSV-separated rungs
(`T→X`, `X→S`, `SP→ext`) that sentence is true *and* it is what keeps the exit inside the TRUC
in-flight window of 2. For the **CATS-B spine it is false**: `SPINE_CSV = 0`
(`clients/libs/rust/src/tesr.rs:5346`) makes a spine tier valid *before* its parent confirms, which
is the whole point of the spine — and it means consecutive spine tiers are exactly the shape TRUC
refuses beyond two. The spec's §6 must therefore state the in-flight window of **2** as a normative
relay constraint, not merely a latency note.

**Deployed-profile interaction (D7).** The deployed profile (`server/Settings.toml`: testnet /
`lockheight_init = 1000`) silently gets the toy regtest schedule and admits an exit chain of **139
transactions** (`lib/src/transfer/receiver.rs:806-812`) — ~135 of them consecutive zero-CSV spine
tiers. At an in-flight window of 2 and one confirmation per step, that is a **~68-block relay stall**,
which exceeds the entire regtest state schedule (`d0 = 24`, `lib/src/tesr.rs:215`). On the profile
actually running, **the relay-stall term dominates the timelock schedule.** D7's chosen answer
(explicit `TesrParams` per profile) is what fixes this; this run is the evidence for why it matters.

---

## 2. Package-CPFP rescue of an underpaying tier — it works

A v3 parent carrying a small anchor output, built at ~1 sat/vB (below the 3 sat/vB floor):

**Alone:**
```
error code: -26
min relay fee not met, 200 < 423
```

**As a 1P1C package** with a v3 child spending the anchor and paying for both — `submitpackage`:
```
"package_msg": "success"
parent: vsize 141, base fee 0.00000200, effective-feerate 0.00567704
child:  vsize 177, base fee 0.00180330, effective-feerate 0.00567704
```

**The rescue works.** A tier committed at 2.0 sat/vB that cannot relay on its own IS recoverable via
a package, and both transactions inherit one effective feerate. **This is what D5's recommended
answer was gated on** (`DECISIONS.md` D5: "per-network constant, receiver-enforced by equality —
GATED on the P2A spike; if fee-bumping cannot be made reliable, this option is unsound as stated").

**D5's gate is satisfied. The decision stands as taken.**

### What this run did NOT establish — read before relying on it

1. ~~**The literal P2A script was not exercised.**~~ **RESOLVED 2026-08-11 — it relays.** A v3
   transaction carrying a genuine `OP_1 <0x4e73>` output was built (by patching the serialised
   output's value and script directly: `0000000000000000` `04` `6a024e73` ->
   `f000000000000000` `04` `51024e73`) and broadcast on the isolated node:

   ```
   version=3
     vout0 = 0.99999000 BTC  spk=0014b95838653ab062de2906e1ac5ca1fad3c1cfb744
     vout1 = 0.00000240 BTC  spk=51024e73          <- the real anchor, at exactly P2A_VALUE
   accepted: 4631aa67b674af5c3e2bfb44c09810c32997f2d11a573babd3526d498c0b36c2
   ```

   **240 sats at `51024e73` is NOT dust to Core** — no dust rejection, no standardness complaint. So
   `P2A_VALUE = 240` (`lib/src/tesr.rs:41`) is correct and the anchor is relayable as specified. A
   unit test also pins the script bytes and the value (`lib/tests/p2a_script_shape.rs`), so a change
   to either is caught without a node.

   (Note: the broadcast needed `sendrawtransaction <hex> 0` — the default `maxfeerate` guard tripped
   on the deliberately oversized fee of the test transaction, which is a wallet-side safety check,
   not a relay rule.)
2. **No `submitpackage` caller exists anywhere in the tree.** Verified: the rescue path is currently
   theoretical *in this codebase* even though it works in Core. Someone must build it, and D5's
   "choose a package-capable backend" sub-item is unresolved — electrum's protocol has no
   `submitpackage` equivalent, so the client needs a different route to a node for this one call.
3. **Who funds the child.** A keyless watchtower holds no UTXO by definition
   (`clients/libs/rust-sdk/src/watchtower.rs:3-4`), and `PROTOCOL.md:525`'s prepaid tower fee bond is
   unbuilt. The child in this run was funded from a second wallet input. **Fee-bumping an exit
   therefore requires an on-hand UTXO the current watchtower design does not have.** This is a real
   gap for §15/D19, not a detail.

---

## 3. What to do with this

| Item | Status |
|---|---|
| D5 (fee yardstick = per-network constant) | **Gate satisfied — decision stands** |
| §6 in-flight window of 2, sequential submission | Write as normative; cite this run |
| `PROTOCOL.md:245-247` "each tier confirms before the next is valid" | **False for the spine** — correct it (WP7) |
| D7 explicit per-profile `TesrParams` | Evidence strengthened: 139-tx chain ⇒ ~68-block stall > the whole schedule |
| A `submitpackage`-capable backend | **Open** — no caller exists; electrum cannot do it |
| Who funds a CPFP child for a keyless tower | **Open** — D19/§15 |
| Literal P2A output relay | **UNVERIFIED** — re-run with a correctly constructed `51024e73` output |

Reproduce: start an isolated regtest node with `-minrelaytxfee=0.00003`, build v3 transactions by
patching `createrawtransaction`'s output from `02000000` to `03000000`, and sign a child spending an
unconfirmed parent output by passing that output explicitly as `prevtxs` (the wallet cannot sign it
otherwise — it fails with `mempool-script-verify-flag-failed (Witness program hash mismatch)`).
