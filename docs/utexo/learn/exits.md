# Deposits and exits

> **Read this first.** Every plain BTC deposit is **laddered**: `claim()` establishes a TES-R
> trigger → extension → state chain (relative CSV locks, all three tiers pre-signed and
> **un-broadcast**). Relative timelocks do not tick until their parent confirms, and the trigger has no
> timelock at all, so **nothing matures until someone broadcasts the trigger**. A laddered coin has
> **no calendar deadline, no expiry, and 0 vB of idle rent** — there is nothing to "exit before".
> The normative account is [PROTOCOL.md](../PROTOCOL.md) §5.2 (the tiers), §5.7 (races), §5.8
> (cooperative de-trigger) and §5.9 (exit costs).
>
> The **un-laddered** shape — RGB carriers and split sub-coins whose funding tx is un-broadcast — keeps
> the older signed-once backup with an **absolute** locktime, and therefore keeps a root deadline. The
> deadline arithmetic and pricing in INVALIDATION-SPEC (retired 2026-08-15) §6,
> [invalidation-deep-dive.md](invalidation-deep-dive.md) and
> invalidation-economics (retired 2026-08-15) describe **that shape** (and the
> pre-migration baseline they were written against) — not the laddered one.
>
> Token-carrier coins exit differently again — both plain paths refuse them (an RGB-unaware sweep
> destroys the allocation), so a carrier exits by **materializing** its branch. The `auto_exit_due`
> watchtower does this automatically for a *received* carrier nearing its root deadline (branch-only,
> emitting `TokenCarrierMaterialized`), so token pieces still get automated deadline protection
> (SPEC §9.5 / REQ-33, `sdk34`); an issued/flat carrier has no ancestor and is left untouched. See
> [tokens.md "Exits with tokens"](tokens.md#exits-with-tokens) and
> GRANULARITY-SPEC (retired 2026-08-15) GRN-INV-14.

## Deposits

`get_deposit_address(amount)` performs the SE handshake and returns a taproot address whose key
is `you + SE`. Send the exact amount; the SDK's watcher detects it, waits for confirmations, and flips
the coin to spendable — emitting `DepositConfirmed`. One deposit = one coin = one tree root.

`claim()` then **ladders that coin, unconditionally** — there is one protocol and no version flag to
choose (`deposit_protocol_version` and the `UTEXO_PROTOCOL_DEFAULT` env are deleted). Establishment is
idempotent and emits `LadderEstablished`. Off one funding output **F** it pre-signs, but never
broadcasts:

```
F   (on-chain, your deposit — the ONLY thing resting on-chain)
└─ T   TRIGGER    v3/TRUC + 240-sat P2A. NO timelock. Signed ONCE, at deposit. Never re-signed.
   └─ X_m EXTENSION  relative CSV E_m = E0 − m·δE, counted from T's confirmation.
      │              Renewal replaces it HORIZONTALLY with a lower-CSV X_{m+1} (off-chain).
      └─ S_k STATE   relative CSV Δ_k = D0 − k·δ, counted from X_m's confirmation. Pays owner k.
```

Mainnet defaults (served by `/info/config`): `E0/δE/E_floor = 720/36/144`, `D0/δ/D_floor = 1440/36/144`,
forced rollover at `m = 15`, committed fee 2 sat/vB. `sdk44` pins the schedule arithmetic; `sdk40`
PART 1 pins that real consensus enforces BIP-68 here (an extension is rejected before `E` confirmations
of the trigger, a state before `Δ` of the extension) and that **nothing ages while un-broadcast**.

Laddering is **root-only** (constraint B0, checked fail-closed against electrum): a coin whose funding
output is an un-broadcast split output cannot root a trigger, so it travels the un-laddered shape
instead. An RGB carrier is deliberately never laddered either (terminal-freeze — a plain tier spend
would sweep the sats and destroy the allocation; `sdk52`). Both are **current shapes, not legacy**.

(A deposit also gets one signed-once backup before the ladder goes up. If establishment can't complete
this pass — electrum momentarily unreachable, RGB state unavailable — the coin is simply left
un-laddered and still exitable by that backup, and the next `claim()` retries.)

Deposit slots consume a **deposit token** from the SE's token server (anti-spam/fee mechanism).
The SDK requests one automatically; when tokens require payment it surfaces
`TokenPaymentRequired` with the payment details (apps can also pre-pay and pool tokens).

*Static addresses:* Mercury deposit addresses are per-coin. Reuse is detected and handled
(duplicate coins can be swept), but for privacy and cleanliness the SDK treats an address as
one expected deposit — `get_deposit_address` is cheap; call it per receive. (Spark's rotating
"static" addresses are the same idea server-rotated.)

## Cooperative exit (the normal path — one transaction)

`withdraw(l1_address, coins?, fee_rate?)`: the SE co-signs a **fresh direct spend** of each coin's
funding output to your L1 address. Immediate (no timelock), one on-chain tx per coin, ≈111 vB. The
pre-signed ladder is simply abandoned un-broadcast — a cooperative exit never touches it.

Two shapes need a step first:

- an **un-laddered sub-coin** is materialized: the SDK broadcasts its exit branch (branch txs carry no
  locktime — like Spark's zero-timelock split nodes — so materialization is instant), then the withdraw
  spend;
- a received **in-ladder split child** has no confirmed outpoint to spend at all (its funding `SP.out[j]`
  is un-broadcast), so `withdraw` cannot co-sign a direct spend for it. It is routed automatically to
  the unilateral exit below and booked `WITHDRAWING` — the child's pre-signed chain already pays your own
  key, so nothing is lost, but it settles over several blocks instead of one.

Spark needs an SSP with an on-chain connector-tx swap for cooperative exits; here the SE's
co-signature on a direct spend does the whole job.

## Unilateral exit (the SE is gone)

`unilateral_exit(coins?, to?) → Vec<ExitStatus>` needs nobody. It is **incremental**, not a single
broadcast: each call advances the pre-signed chain as far as maturity allows and reports
`{complete, wait_blocks}`. Call it once per block (or run the background pass) until `complete`.

**A laddered coin** walks its tiers, waiting out each relative timelock in turn:

1. broadcast **T** — no timelock, so it confirms straight away, and *this is what starts every clock
   below it*;
2. wait `E_m` blocks after T confirms, broadcast **X_m**;
3. wait `Δ_k` blocks after X_m confirms, broadcast **S_k** — the funds land at your own key.

Worst-case total on a freshly laddered coin is `E0 + D0 = 2,160` blocks ≈ **15 days**, and it *shrinks*
by 36 blocks per hop and per renewal, because both tiers decrement as the coin is used. Cost: 3
pre-signed txs ≈ **372 vB**, self-relaying via their committed fees; in a fee spike anyone can attach
P2A fee children (~152 vB each), up to ≈828 vB. Driven end-to-end by **`sdk50`**, and by a keyless
tower with no key material in **`sdk45`**.

**A received split child** walks the same idea one level deeper: `T → X_m → SP → the child's own
extension → the child's own state` (5 pre-signed txs — this is why a child must clear
`min_child_value`, 1306 sat at 2 sat/vB: it funds its own two tiers plus dust). A child is otherwise
fully first-class and normally never exits at all — it can be paid onward off-chain, whole or split,
for one co-signature per hop ([CHILDREN.md](../CHILDREN.md), `sdk60`, `sdk17`). Depth-*d* generally
costs `3 + 2d` txs ≈ `124·(3+2d)` vB with a worst wait ≤ `(d+1)·2,160` blocks (depth-3 ≈ 60 days — the
honest price of relative timelocks; Spark shares the class). Mitigations: the cooperative exit covers
the normal case, the default depth cap is 3, and optional per-level shrinking schedules bound any depth
under ~30 days (shipped dial, default off).

**An un-laddered coin** (RGB carrier's sats structure, or a sub-coin over un-broadcast funding) is the
one place the older mechanics still apply: broadcast the **exit branch** — the chain of pre-signed
split/combine txs from the on-chain root down to the coin's funding output, consensus-final immediately
— then the coin's **latest signed-once backup tx**, which is locked to a future height by the
*decrementing absolute* scheme. Every previous owner's backup unlocks *later* than yours, so you have a
window in which only you can claim. This shape, and only this shape, carries a genuine calendar duty —
act (materialize, or move the coin) before the root deadline, per
INVALIDATION-SPEC (retired 2026-08-15) §6 — and it is exactly the shape `auto_exit_due` was
built to cover on your behalf.

One guard worth knowing: `unilateral_exit` refuses a coin that is not `CONFIRMED` — a parent already
consumed by a split must never be exited, because that would invalidate the sub-coins its split funds.

## Nothing expires — but you (or a tower) do have to watch

The trade a laddered coin makes is explicit: it deletes the calendar entirely and replaces it with
**perpetual but alarm-driven** watching.

- No theft transaction can even become *valid* until someone publicly broadcasts **T**, spending your
  funding output on-chain — a loud, unmissable alarm — and then waits at least 144 blocks (~1 day) of
  CSV. Compare an absolute-locktime backup chain, where a rival's maturity arrives silently on the
  calendar and the contest afterwards is a minutes-scale mempool race.
- Missed liveness is **never confiscation by design**: nothing expires, no output ever pays the
  operator by timeout. Loss requires a real adversary winning a telegraphed public race while every
  tower slept through a day or more of alarm.

Two responses exist, in preference order:

1. **Cooperative de-trigger** (needs the SE, ≈111 vB): T's output pays the coin's own aggregate, so you
   and the SE key-path-spend it *with no timelock* into a fresh funding output and rebuild the ladder —
   confirming unopposed inside the ≥144-block window during which no adversary tx is valid. This is why
   trigger-griefing is a priced nuisance rather than an attack: the griefer burns ~276 vB to cost you
   ~111 vB. Built by `tesr::build_detrigger` / `cosign_detrigger`; proven end-to-end by **`sdk40`
   PART 2**, which also shows the griefer's stale extension can never confirm afterwards.
2. **Race and win** (needs nobody): `defend_ladders()` runs one watch pass per coin. On an untriggered
   coin it is a no-op — there is nothing to defend. On a triggered one it broadcasts your tiers as they
   mature; because a transfer always co-signs a state one δ **lower** than the one it replaces
   (replace-by-lower-timelock), the current owner's state carries the strictly lowest CSV and matures
   first, by a ≥36-block (~6 h) edge per tier. **`sdk51`** drives exactly this: a prior owner triggers
   with a stale state, the owner runs only `defend_ladders()`, and the funds land at the owner's key.

The duty is delegable and **keyless**: `export_watch_bundle()` emits a JSON bundle of pre-signed txs
with **no key material** — every tx in it pays you and nobody else — and `watchtower::watch_pass` runs a
pass from that bundle plus an electrum client alone. A second, independent tower is idempotent
(**`sdk45`**). The background watcher (`SdkConfig::auto_exit`) also still runs the `auto_exit_due` pass,
which now matters only for the un-laddered shape — a laddered coin has no deadline for it to act on.

## Extending a coin's life (off-chain) and re-anchoring (on-chain)

Because idle coins never age, there is nothing to renew on a schedule. What does get consumed is the
**hop budget** — each transfer decrements the state CSV by δ. Both refills are **off-chain and
unbounded**, and both run unattended inside `transfer()`:

- **Renewal** — when the next state would fall below `D_floor`, two blind co-signs mint a fresh
  extension `X_{m+1}` at a *lower* CSV plus a fresh state on it. Zero on-chain bytes. The older
  extensions become consensus-dead: `X_{m+1}` strictly undercuts them in the race for T's output, so
  every state hanging off an older extension can never confirm (**`sdk40` PART 2**).
- **Rollover** — at `m = 15` the SDK co-signs a 1-in-1-out self-split whose child output hosts fresh
  extension and state tiers, i.e. a whole fresh 576-hop budget, at the cost of +1 depth level and ~248
  vB of *contingent* exit weight. Zero on-chain bytes.

**`sdk43`** drives renew → rollover → renew past epoch exhaustion and then exits unilaterally through
the whole deep chain, with the funding outpoint untouched throughout: a coin can live off-chain forever.

**Refresh is now the re-anchor primitive, not a deadline reset.**
`refresh(statechain_id, fee_rate?)` spends the coin's current outpoint into a **fresh aggregate** in one
SE-co-signed on-chain tx (~112 vB): a new `statechain_id` at a new funding outpoint, same owner, which
`claim()` then ladders from scratch. Because the old outpoint is spent, all older structure over it is
permanently dead. Use it to **cap depth** (the default depth-capping policy compacts a coin deeper than
3 levels at its next transfer, priced into that transfer's fee) or to move a coin to fresh funding —
*not* to buy time, because a laddered coin is not running out of any. Rescuing an **un-laddered
sub-coin** before its root deadline is the one case where refresh still buys time, and there it beats a
full exit-and-redeposit; it is refused outright on an RGB carrier (materialize the branch instead). The
fee is drawn from the coin (user-pays);
`refresh_sponsored(...)` layers an off-chain operator rebate of `max(fee + dust, min_child_value)` on
top so the user ends ≥ whole. Both fee models are pinned by **`sdk30`**. Refresh is cooperative — it
needs the SE; if the SE is gone, exit unilaterally instead.

## Timelock summary

| Transaction | Shape | Timelock |
|---|---|---|
| T — trigger | laddered | **none**; signed once at deposit, un-broadcast. Broadcasting it starts every clock below |
| X_m — extension | laddered | **relative** CSV `E_m = E0 − m·δE`, from T's confirmation |
| S_k — state | laddered | **relative** CSV `Δ_k = D0 − k·δ`, from X_m's confirmation; each transfer takes one δ lower |
| SP — in-ladder split | laddered | a state tier at `Δ_{k+1}`; each child output then hosts its own extension + state |
| cooperative withdraw / de-trigger | either | none (fresh co-signed spend) |
| exit branch txs (split/combine) | un-laddered | none — immediately broadcastable |
| deposit backup #1 | un-laddered | **absolute** `deposit_height + initlock` |
| each transfer's new backup | un-laddered | previous − `interval` |
| a colored split piece's first backup | un-laddered | fresh chain at the full initial locktime |

## Which exit costs what

| Path | On-chain weight | Wait | Needs the SE? |
|---|---|---|---|
| Cooperative withdraw | ≈111 vB, 1 tx | none | yes |
| Cooperative de-trigger (grief response) | ≈111 vB, 1 tx (~155 vB colored) | none | yes |
| Unilateral, laddered coin | 372–828 vB, 3 txs (+P2A children in spikes) | `E_m + Δ_k`, worst 2,160 blocks ≈ 15 d, shrinking 36 blocks/hop | no |
| Unilateral, depth-*d* sub-coin/child | `3 + 2d` txs ≈ `124·(3+2d)` vB | ≤ `(d+1)·2,160` blocks (depth-3 ≈ 60 d) | no |
| Token materialization | branch only, `2d+1` txs | none (branch txs have no locktime) | no |
| Re-anchor (`refresh`) | ~112 vB, 1 tx | none | yes |
| Renewal / rollover | **0 vB** | none | yes (blind co-sign only) |
