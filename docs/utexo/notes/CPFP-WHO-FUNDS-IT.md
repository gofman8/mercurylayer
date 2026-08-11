# Who funds a CPFP child? — the keyless tower cannot, and the anchor does not fix it

**Status:** analysis complete, **decision needed from the owner**. 2026-08-11.
Spun out of `WP1-TRUC-P2A-SPIKE.md` §4 ("Who funds a CPFP child for a keyless tower — Open, D19/§15").

---

## 1. The finding, in one line

**The P2A anchor makes a tier's fee bumpable by *anyone*; it does not make it bumpable by *nobody*.**
A CPFP child still needs an input to pay with, and an input needs a UTXO and a key. The tower has
neither, by design. So the escape hatch that `WP1` proved works is one the tower structurally cannot
reach.

This is not a bug in the anchor. `OP_1 <0x4e73>` is spendable without a signature (verified,
`#124`), so nothing stops a *funded* party from bumping any tier. The gap is that the protocol names
the tower as the party who acts when the owner is offline, and then gives it nothing to act with.

## 2. Why it matters, with the measured numbers

Every tier commits `committed_fee_rate = 2.0` sat/vB at co-sign time (`lib/src/tesr.rs`), and that
rate is fixed for the life of the tier — it is signed. The WP1 spike ran a node at a **3 sat/vB**
floor, i.e. the regime where a committed tier does not relay at all:

| | measured |
|---|---|
| tier alone, under the floor | `min relay fee not met, 200 < 423` — **refused** |
| the same tier as a 1P1C package | `"package_msg": "success"` |
| parent | vsize 141, fee 200 sats |
| child | vsize 177, fee **180 330 sats** |
| resulting effective feerate | both at ~5.68 sat/vB |

**The child paid ~900× the parent's fee.** That is the number the decision turns on: bumping is not a
rounding cost, it is a real outlay by whoever performs it, and the anchor it spends is worth 240 sats.
There is **no fee-market incentive** for a stranger to do it — the spender always pays more than the
anchor returns. "Anyone can spend it" is therefore not the same as "someone will".

So when the mempool floor rises above 2 sat/vB and the owner is offline, today: the tower broadcasts,
the broadcast is refused, and the tower reports a failure it has no means to resolve.

## 3. The options, with what each actually costs

### (A) Owner-funded — the tower never bumps

The owner's wallet has UTXOs and keys, so it can build the package. The tower stays keyless.

* **Cost:** the guarantee becomes "the owner is online during a fee spike". That is precisely the
  case the tower exists for, so this option does not close the gap — it renames it. Honest, cheap,
  and must be written into the spec as a stated limit rather than left implied.

### (B) Funded tower — give it a fee UTXO and a key

The tower gains a small hot wallet used only to pay for packages.

* **Cost:** "keyless" stops being true, and `PROTOCOL.md` claims it in four places (`:72`, `:124`,
  `:178`, `:241`). The exposure is bounded and worth stating precisely: the tower still holds **no
  coin keys**, so a compromised tower loses its own fee float and cannot touch a user's coin. That is
  a materially smaller claim than "keyless", and materially larger than nothing.
* Also introduces an operational duty — the float has to be refilled, and a tower that runs dry fails
  exactly when it is needed.

### (C) Third-party fee service

A commercial bumper with a business relationship, paid out of band.

* **Cost:** it is (B) with the float moved to someone else's balance sheet, plus a counterparty. It
  does not avoid the funding question; it relocates it. Worth considering only if towers are run by
  many parties and the float is better pooled.

### (D) Raise `committed_fee_rate` so bumping is rarely needed

Make tiers self-relaying across a wider fee regime.

* **Cost:** paid by **every tier of every coin, permanently, whether or not it is ever broadcast** —
  and it is signed in, so it cannot be revised for coins already laddered. It shrinks the exposure
  window rather than closing it: any fixed rate is exceeded by some future mempool.
* It also directly raises `V_min` (the smallest economically viable coin), which D5/D3 are still
  settling. This option cannot be priced in isolation.

## 4. What I recommend, and what I am not deciding

**(A) as the honest v1 statement, with (B) as the deployment option**, and the spec saying both:

* the normative text should say plainly that **a keyless tower cannot fee-bump**, so an implementer
  reading only the spec cannot conclude otherwise (this is exactly what D19 asks for — "write down
  what a keyless tower **cannot** do as normative properties");
* a tower operator MAY run the funded variant, and the spec should describe its bounded exposure
  (fee float only, never coin keys) so that the choice is informed rather than ad hoc.

**I am not deciding (D).** Changing `committed_fee_rate` is a permanent, per-coin, signed-in cost that
interacts with `V_min` and with two decisions still open; picking it here to make one gap close
neatly would be trading a priced cost for an unpriced one.

## 5. What this blocks

`#123` (a `submitpackage`-capable broadcast path) is **not** blocked on this decision for its client
half — an owner-driven bump under (A) needs the same code. What (A) vs (B) changes is *who calls it*
and whether the tower needs a wallet at all. The `submitpackage` route itself is separately blocked:
electrum has no equivalent, so any bumping path needs a Bitcoin Core RPC endpoint that the SDK does
not currently have (WP1 §4).
