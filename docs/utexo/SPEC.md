# Mercury + RGB Utexo — System Specification

> ## ⚠️ Direction of travel: ONE COIN TYPE
>
> This document specifies the system **as built today**, which has two coin shapes — *laddered*
> (TES-R) and *un-laddered* (RGB carriers and un-broadcast split sub-coins). **That is a transitional
> state, not the target architecture.** The decided direction is a single coin type; the un-laddered
> shape is being removed, not kept.
>
> The mechanism is **CTES-R** — colour every TES-R tier so an RGB carrier can be laddered, retiring
> terminal-freeze. Its gate passed against the live stack ([CTESR-GATE.md](CTESR-GATE.md)) and its
> foundation has landed (the `payload_vout` migration, the coloured tier builder, per-tier seal
> blinding). **The colouring itself is not yet wired.** Until it is, everything below about the
> un-laddered shape remains accurate as-built.
>
> So: read two-shape material here as a description of the present, never as the target. Items scoped
> to the un-laddered shape — its absolute deadlines, backup-chain handover, terminal-parent proofs,
> and the exit-cost and carrier-depletion arithmetic — are expected to be **deleted**, not migrated.
>
> Reaching one coin type also requires porting `verify_bundle` to wasm/JS and Kotlin: the nodejs and
> web clients refuse any transfer that *declares* a ladder, so every coin laddered is one they cannot
> receive. Note what that gate keys on — three SENDER-supplied fields (`protocol_version >= 2`,
> `tesr_ladder`, `child_tesr_bundle`; `transfer_receive.js`) — so it is a refusal of DECLARED
> ladders, not a structural one, and the flat path it falls through to is the un-laddered
> `num_sigs == backups.length` check against a coordinator-supplied `interval` (the very input the
> Rust side stopped trusting — `TesrParams::flat_ladder_params` is compiled in, §2.4). Those clients
> are therefore not an exempt population; they are the un-defended one until the port lands.
> Background: [COLORED-FORWARDING.md](COLORED-FORWARDING.md).

Normative specification of the Utexo system built on Mercury Layer statechains with RGB
assets and a single statechain entity (SE). Requirements are labelled **REQ-n**, invariants
**INV-n**, and error semantics **ERR-n**. Each is mapped to a verifying test in
[§12 Traceability](#12-traceability). Keywords MUST/SHOULD/MAY per RFC 2119.

Scope: the SE (Mercury server + lockbox), the client libraries (`mercurylib`, `mercuryrustlib`,
`mercury-rgb`), the wallet SDK (`mercury-utexo-sdk`), the SSP service (`mercury-ssp`), and their
Bitcoin/RGB/Lightning interactions. Companion docs: [core-concepts](learn/core-concepts.md),
[invalidation](learn/invalidation.md), [PARITY.md](PARITY.md).

**One protocol, two coin shapes.** There is a single protocol: `claim()` establishes a TES-R exit
ladder (§2.6) for every fresh confirmed **root** coin, unconditionally. The
`deposit_protocol_version` field and the `UTEXO_PROTOCOL_DEFAULT` escape hatch that could opt a
deposit back into the flat pre-TES-R shape are DELETED. Two coin SHAPES coexist, both current:

- **Laddered** — every plain deposit. Its exit is the relative-CSV tier chain (§9.2); it has no
  calendar deadline and costs 0 vB of idle rent (INV-27).
- **Un-laddered** — an RGB **carrier**, which must NEVER be laddered (a plain tier spend would
  destroy the allocation — terminal-freeze, INV-29), and a split sub-coin whose funding is
  un-broadcast and therefore cannot root a trigger [B0]. These keep the signed-once backup chain
  (§2.4) and transfer by backup-chain + branch handover. This path is LOAD-BEARING for RGB tokens.

Normative TES-R references: [PROTOCOL.md](PROTOCOL.md) (tiers, renewal, terminal-freeze),
[CHILDREN.md](CHILDREN.md) (first-class split children),
[LIGHTNING.md](LIGHTNING.md) (the Lightning latch).

---

## 1. Roles and trust

- **Owner** — a wallet holding one key share of a coin. Can spend only with the SE; can always
  exit unilaterally without it.
- **SE (statechain entity)** — server + lockbox holding the other key share of every coin. Blind
  MuSig2 co-signer: never sees amounts/addresses. Enforces single-use, spend budgets, epoch
  deadlines, the pending-transfer lock (REQ-36) and one signature per server nonce (INV-23) — it
  does NOT and cannot adjudicate rival states, because it blind-signs 32-byte hashes and never
  learns what it signed (INV-6). Cannot move funds alone; cannot block a unilateral exit.
- **SSP** — an application-level party (owner + Lightning node) bridging Mercury↔Lightning. Not
  trusted with custody: swaps are atomic (§8).
- **Issuer** — any owner that issues an RGB asset. No privileged runtime role beyond holding the
  contract issuance rights.

**REQ-1** The SE MUST NOT be able to move a coin's funds without the owner's co-signature (2-of-2).
**REQ-2** An owner MUST be able to exit to L1 without any SE cooperation (pre-signed material only).
**REQ-3** Trust reduces to: *the SE refuses to co-sign past a terminal budget, a passed epoch, a
single-use spend or an open transfer, and reports its co-signature count honestly* — and that count
is no longer taken on trust: it arrives under the enclave's `utexo/sig_count/v2` signature, verified
against the CHAIN-ANCHORED enclave key the receiver already bound to `tx0`, over a nonce the
receiver itself chose (§3.3, REQ-38). Plus a liveness duty on the owner (or a delegated tower) that
differs by coin shape — an **un-laddered**
coin must be exited before its backup locktime floor / epoch deadline; a **laddered** coin has no
deadline at all, but its defender must react within the CSV edge once someone publicly broadcasts
the trigger (§9.5, INV-28). No custody rests on the SE in either case, and nothing ever expires to
the operator.

---

## 2. Data model

### 2.1 Coin
A statechain coin is a Bitcoin P2TR UTXO whose key is the MuSig2 aggregate of `owner_pubkey` and
`se_pubkey`, plus SE-side state `{statechain_id, auth_pubkey, amount, locktime, single_use?,
epoch_deadline?, sig_budget?}` and client-side state (backup txs).

Coin status lifecycle (client): `INITIALISED → IN_MEMPOOL → UNCONFIRMED → CONFIRMED →
{IN_TRANSFER → TRANSFERRED | WITHDRAWING → WITHDRAWN | DUPLICATED | INVALIDATED}`.
A split child routed to a unilateral exit (§9.2) is booked `WITHDRAWING` with **no** withdrawal tx
and no withdrawal address — its progress is the pre-signed exit chain, not one watched txid — and
status polling MUST accept that combination (treating it as an error made every later poll fail for
the life of the coin; defect found and fixed during this migration).

**INV-1** A coin's `amount` equals the sats of its funding output.
**INV-2** A `CONFIRMED` coin has ≥ `confirmation_target` confirmations of its funding UTXO (or, for
an off-chain sub-coin, a validated exit branch — §2.3).

### 2.2 Sub-coin (off-chain)
A sub-coin is a coin whose funding tx is **un-broadcast**: it is an output of a split/combine tx
that the SE co-signed but nobody broadcast. Its `utxo_txid:vout` points at that un-broadcast tx.
An **in-ladder split child** (§6.1) is also funded by an un-broadcast tx, but its exit material is
the pre-signed child ladder (`ext_child` + `state_child`) hanging under the parent's tiers, not a
branch + absolute-locktime backup.

### 2.3 Exit branch
An exit branch is the chain of fully-signed split/combine txs from a spend of an **on-chain**
outpoint down to the tx that funds a sub-coin, stored root-first under `branch-<statechain_id>`.

**INV-3** Every tx in a branch is consensus-valid against its predecessor's outputs; the branch
root spends an on-chain, unspent, confirmed outpoint. (Enforced by `validate_branch`.)
**INV-4** Branch (structural) txs carry no relative/absolute locktime — they are immediately
broadcastable.

### 2.4 Backup ladder (absolute locktime)
Each coin has ≥1 pre-signed backup tx paying the owner's address, at absolute locktime
`h + initlock − interval·k`. The first backup (`k=0`) is at `deposit_height + initlock`; every
transfer hands the new owner a backup one `interval` lower.

**INV-5** For any coin, the current owner's latest backup locktime is strictly lower than every
previous owner's backup locktime (current owner wins the exit race), and each hop decrements by
EXACTLY `interval`. `initlock`/`interval` are **compiled in per network**
(`TesrParams::flat_ladder_params`: 10 000/100 on mainnet, testnet and signet; 1 000/10 on regtest —
100 hops of capacity either way). The coordinator's own copy is a cross-check only: `info_config`
REFUSES the call outright if the two disagree. Taking `interval` from the coordinator would let the
coordinator define the defence, and deriving it from the conveyed chain is circular — a padded chain
of uniform `interval/2` hops validates against itself, which is the padding INV-5 exists to stop.

For an **un-laddered** coin this chain IS the exit material, and it is a finite budget (`initlock`
blocks, spent by each transfer and by wall-clock time). When it nears the floor the coin must be
moved to L1 by exit (§9), materialized if it is a carrier (REQ-33), or COOPERATIVELY re-anchored
on-chain via `refresh` (§9.4, REQ-31).

For a **laddered** coin the backup chain is still built, conveyed and structurally validated on
every transfer — its COUNT is a term in the receiver's census (REQ-38) and INV-5 is the only defence
against a sender inverting the ladder or padding it with duplicates — but it is not the exit path:
`unilateral_exit` walks the tier chain and broadcasts NO absolute-locktime backup (`sdk50`), and the
coin's lifetime is not calendar-bounded (INV-27).

### 2.5 Ancestor record
For each sub-coin, its structural ancestors (the split/combine parents) are stored under
`parents-<statechain_id>` (parent id + inherited ancestors).

### 2.6 TES-R exit ladder (laddered shape)
Above the funding UTXO `F` sits a pre-signed, **un-broadcast** tier tree (PROTOCOL.md §5.2): a
**trigger** `T` (spends `F`, no timelock, signed once at deposit detection), **extensions**
`X_0…X_m` (mutually exclusive spends of `T.out[0]`, input nSequence = relative-CSV `E0 − m·δE`), and
**states** `S_0…S_k` on `X_m.out[0]` (nSequence = CSV `D0 − k·δ`) paying the current owner's own
seed-derived key. A split state `SP` (§6.1) is a state tier too, but it is a SPINE tier: it is
pinned at `SPINE_CSV = 0`, which is how it out-races the `S_0` it replaces over the same output —
and the builders refuse the split outright unless that `S_0` sits strictly above it (`s0_csv <=
SPINE_CSV` is a hard refusal, `tesr.rs`). Every tier is nVersion=3 (TRUC), carries a committed fee
(`committed_fee(rate)` over `TIER_VBYTES = 125` — the MEASURED signed vsize; the earlier 124
modelled a 64-byte `SIGHASH_DEFAULT` witness, and TES-R signs `SIGHASH_ALL`, so every tier carries
the explicit 65th byte [D4]) so it relays standalone, and a 240-sat P2A anchor for live-rate
fee-bumping.

**INV-27 (idle coins never age)** No tier is on-chain, and a BIP-112 relative lock does not tick
until its parent confirms, so nothing anywhere matures until someone broadcasts `T`. An idle
laddered coin — and an idle split DAG — therefore has no calendar deadline and costs 0 vB of rent.
Verified by `sdk30` (a) (300 blocks mined, exit chain byte-identical, `F` unspent) and `sdk40`.
**INV-28 (lower CSV wins)** Every transfer and every renewal co-signs a state (extension) at a
strictly LOWER CSV than the one it supersedes, so the current owner's tier matures first and each
superseded tier's parent becomes unconfirmable — invalidation at the CONSENSUS level. There is no
second, independent SE-side layer under it: a blind co-signer cannot tell a rival state from a
renewal, and the code does not try (INV-6). What actually stands beside consensus is the receiver's
census (REQ-38) — every co-signature the SE ever issued must be accounted for, against an
enclave-ATTESTED count — plus the pending-transfer lock (REQ-36). Verified by `sdk40` PART 2
(a stale ladder is defeated by a cooperative de-trigger) / PART 3 (a renewed extension supersedes
the old one at consensus level), `sdk41`, `sdk51`.

Renewal and rollover are **off-chain**: when the next state would fall below `D_floor` the SDK
co-signs a fresh extension `X_{m+1}` (two blind co-signs, zero on-chain bytes); at extension
exhaustion it rolls over into a fresh level via a self-split. Off-chain state transitions are
therefore unbounded with no mandatory chain touch (`sdk42` lifecycle + persistence, `sdk43`
rollover). No SE endpoint is added for this — renewal is ordinary blind co-signing (§3.2).

---

## 3. SE API (normative)

All endpoints are HTTP JSON on the Mercury server. Encrypted transfer messages are opaque to the
SE (owner-encrypted); the SE never deserializes `TransferMsg`.

### 3.1 Deposit / keygen
- `POST /deposit/init` `{token_id, auth_key, ...}` → `{server_pubkey, statechain_id, ...}`.
  Registers a new coin key-share. **REQ-4** MUST require a valid deposit token.
  `single_use` and `epoch_deadline` MAY be set at init.
- `POST /deposit/get_derived_token` `{statechain_id, auth_sig, count}` → `{token_ids}` — FREE
  **derived-slot** vouchers for slots created by SE-co-signed flows over the named EXISTING
  statechain (split pieces/change, combine outputs, refresh re-anchors). `auth_sig` is the
  single-use endpoint-bound owner challenge (`"<nonce>:<sig>"`, audit [15]); one consumed nonce
  authorizes the whole `count` batch. Never routed to the token server; works on any network.
  See REQ-35 / ERR-13.

### 3.2 Signing (blind MuSig2)
- `POST /sign/first` `{statechain_id, signed_statechain_id, ...}` → `{server_pubnonce}`.
- `POST /sign/second` `{statechain_id, session, server_pub_nonce, ...}` → `{partial_sig}`.

**REQ-5** `sign/first` MUST reject if `single_use` and the coin already has ≥1 finalized signature
(ERR-1).
**REQ-6** `sign/first` MUST reject if `epoch_deadline` is set and the SE clock ≥ it (ERR-2).
**REQ-7** `sign/first` MUST reject if `sig_budget` is set and finalized signatures ≥ budget (ERR-3).
**INV-6 (there is no single-active-state rule)** The SE does NOT refuse a second, conflicting state
for a coin that is within its budget, epoch and transfer lock: `sign/first` re-serves a pending
nonce while its challenge is NULL and otherwise issues a FRESH one, gated only by REQ-5/6/7 and
REQ-36 (`server/src/endpoints/sign.rs`). It could not do otherwise — it blind-signs a 32-byte
sighash and never learns that a tier is a tier (§3.2), so "conflicting" is not a predicate it can
evaluate. What IS enforced per coin is one signature per server nonce (INV-23, a key-leak defence,
not a rival defence), serialisation of concurrent `sign/first` calls, and the terminality gates
above. Rival prevention lives at consensus (INV-28) and in the receiver's census (REQ-38). Any
document that cites an SE single-active-state refusal as a second independent layer is describing a
mechanism this system does not have.
**REQ-36 (pending-transfer lock)** While a transfer of a coin is OPEN — conveyed, not yet completed
by the receiver, not yet expired at `batch_timeout` — `sign/first` and `sign/second` MUST refuse
every co-signature for that coin, and `/transfer/sender` MUST refuse to re-address an open transfer
to a different auth key (both ERR-14; fail CLOSED on a database error). Every legitimate sender
pre-sign (the backups and the receiver-paying state `S'`) happens BEFORE the transfer is opened, so
no honest co-sign falls inside the window. This is a RELEASABLE lock, not a monotonic budget
(INV-24): it closes the window in which a still-owner sender co-signs a lower-CSV rival that
out-races the state it just conveyed, and it is what lets a received split child be handed over
WITHOUT terminalizing it (§6.3). *Coverage:* the honest-path half is well covered (every transfer
and in-ladder-split E2E runs with the lock live, which is what proves the pre-sign re-ordering
correct); the adversarial refusal itself — a sender co-signing a rival, or re-addressing, inside the
open window — has no dedicated test today.

The tier co-signs of ladder establishment, renewal, rollover and in-ladder splits all use these two
endpoints unchanged — the SE blind-signs a 32-byte sighash and never learns that a tier is a tier —
and each one increments the public `num_sigs` the receiver's census reads (REQ-38).

### 3.3 Transfer relay
- `POST /transfer/sender` → `x1` (receiver-binding scalar).
- `POST /transfer/update_msg` `{statechain_id, auth_sig, new_user_auth_key, enc_transfer_msg}` —
  stores the encrypted message. **REQ-8** MUST validate the sender's auth signature.
- `GET /transfer/get_msg_addr/<auth_key>` → encrypted messages for a receiver.
- `POST /transfer/receiver` — rotates the SE key share to the new owner; **REQ-9** after this the
  previous owner's share MUST be unusable.
- `POST /transfer/unlock` — releases a batch-locked coin (owner or SE side).
- `GET /info/statechain/<statechain_id>?attestation_nonce=<32B hex>` → `{num_sigs,
  aggregate_pubkey, sig_budget, has_sig_budget, sig_count_attestation,
  sig_count_attestation_pubkey, enclave_public_key, …}` — the counter every receiver's census
  (REQ-38) is checked against. It is **attested, not asserted**: the count AND the budget travel in
  one enclave signature over `sha256("utexo/sig_count/v2" ‖ statechain_id ‖ u32_be(num_sigs) ‖
  u8(has_budget) ‖ u32_be(budget) ‖ nonce32)`, verified against the chain-anchored
  `enclave_public_key` the receiver already bound to `tx0` — never against the served
  `attestation_pubkey`, which the coordinator chooses — and over a nonce the CALLER generated, so a
  genuine older attestation cannot be replayed. A missing attestation, a mismatched key, or a
  `has_sig_budget` the enclave cannot state is REFUSED, not defaulted (`get_statechain_info`,
  `verify_sig_count_attestation`; there is no phased rollout, D23). Without it a coordinator that
  under-reported `num_sigs` by `k` would hide `k` co-signed rival states while the exact-equality
  census still balanced.

### 3.4 Withdraw
- `POST /withdraw/...` — SE co-signs a fresh direct spend to an L1 address (cooperative exit).

### 3.5 Lightning latch (SE-minted preimage + external hash)
- `POST /transfer/paymenthash` `{statechain_id, auth_sig, batch_id}` → `{hash}` — SE generates a
  preimage, returns `sha256(preimage)`; the coin transferred under `batch_id` is claim-locked.
- `GET /transfer/paymenthash/<batch_id>` → `{hash}` — the batch's hash (external hash if set, else
  `sha256(SE preimage)`).
- `POST /transfer/transfer_preimage` — returns the SE preimage **iff the batch is unlocked**
  (`locked=false`). **REQ-10** MUST NOT reveal the preimage while locked (ERR-4).
- `POST /transfer/paymenthash/external` `{statechain_id, auth_sig, batch_id, payment_hash}` — bind
  a latch to an EXTERNAL 32-byte hash (BOLT11). **REQ-11** MUST validate `payment_hash` is 32-byte
  hex and the auth signature.
- `POST /transfer/unlock/preimage` `{batch_id, preimage}` — unlock the batch iff
  `sha256(preimage)` equals the stored external hash. **REQ-12** MUST reject a non-matching
  preimage (ERR-5); on match, MUST unlock every coin in the batch (sender-side confirm).

### 3.6 Spend budget (terminal nodes)
- `POST /statechain/spend_budget` `{statechain_id, auth_sig, remaining∈{0,1}}` → `{sig_budget}` —
  owner-signed; sets an absolute co-signature ceiling. **REQ-13** MUST reject `remaining ∉ {0,1}`
  and a bad auth signature. Irreversible (budget only tightens).
- `GET /statechain/spend_budget/<id>` → `{sig_budget, finalized, terminal}` — public;
  `terminal = budget set ∧ finalized ≥ budget`.

---

## 4. Deposit

**Flow.** `get_deposit_address(amount)` → `/deposit/init` → P2TR aggregate address. Owner funds it.
The background watcher detects the UTXO (`update_coins`), waits `confirmation_target`, creates the
first backup tx (`create_tx1`, locktime `h+initlock`), flips the coin to `CONFIRMED`, emits
`DepositConfirmed`, then establishes the coin's TES-R ladder (§2.6) and emits `LadderEstablished`.

**REQ-14** A deposit slot MUST consume a deposit token; if payment is required the SDK MUST surface
`SdkError::TokenPaymentRequired` rather than silently proceeding (ERR-6).
**REQ-37 (ladder establishment)** `claim()` MUST establish a TES-R ladder for every CONFIRMED,
non-duplicate, non-`single_use` ROOT coin that has none — unconditionally, and idempotently (a coin
that already carries a ladder is skipped, so repeated passes never double-sign). The exit payee MUST
be the coin's own seed-derived `backup_address`, never an out-of-wallet address. Two exclusions are
BY DESIGN, not leftovers: an RGB **carrier** (INV-29) and a coin whose funding `F` is not on-chain
([B0] — a sub-coin's trigger would have no prevout to spend, leaving it unexitable). Both exclusions
MUST fail CLOSED: if the carrier set or `F`'s on-chain status cannot be resolved, skip the pass and
retry next claim — a missed ladder is harmless, a laddered carrier or sub-coin is not. An
establishment failure leaves the coin un-laddered and still exitable by its signed-once backup.
`sdk48` (auto-established, exits to the seed-derived key, a second `claim()` does not
double-establish), `sdk52` (carrier never laddered).
**REQ-35 (derived slots)** A slot minted by an SE-co-signed flow over an existing statechain — an
off-chain split piece/change, a `transfer_many` recipient/change, a combine output, a refresh
re-anchor — is a **derived slot**: it re-houses value already inside the SE, so the SDK MUST fund
it with a FREE derived token (`deposit/get_derived_token`, vouched by the parent statechain) and
MUST NOT draw on pooled/prepaid onboarding tokens (in a token-server deployment those cost the
onboarding fee — a 2-output split must not cost 2× it). The SE MUST gate issuance on (i) the
parent's CURRENT-owner auth (single-use nonce, consumed only on a valid signature), (ii) a
per-parent LIFETIME cap (`max_derived_tokens_per_statechain`, default 64; 0 disables), and (iii)
the global outstanding-token cap (audit [26]), and MUST mark issued tokens with their parent
(`tokens.derived_from`). Fresh ON-CHAIN onboarding (a deposit address, a token-issuance carrier)
still consumes a normal token per REQ-14. Fallback: when the SE predates/disables the endpoint or
the allowance is exhausted, the SDK falls back to onboarding tokens (pre-REQ-35 behaviour).
The blind SE cannot verify how a slot is later funded (TRUST-MODEL §7 records the residual).
**INV-7** After a deposit confirms, `get_balance().available_sats` increases by the deposit amount.

---

## 5. Transfer (sats)

**Flow.** Sender: pre-sign everything first (the receiver's backup at locktime = previous −
interval, and — on the laddered lane — the receiver-paying state `S'`), then `/transfer/sender` (get
`x1`, which OPENS the transfer and arms the pending lock, REQ-36), then
`create_transfer_update_msg[_with_branch]` → `/transfer/update_msg`. Receiver (async): fetch
messages, validate, `/transfer/receiver` (SE rotates its share). The aggregate key `A` and the
funding UTXO `F` are INVARIANT across the rotation — that is what keeps the pre-signed exit material
valid for the new owner while locking the old one out (`sdk41`).

**Laddered lane (Model A).** A whole-coin handover of a laddered coin additionally conveys the
ladder (`tesr_ladder`): the sender co-signs the receiver-paying state `S'` one δ BELOW the LOWEST
rival over the current extension's payload output — its own live state, every disclosed superseded
state and every still-outstanding conveyed state, not merely its own retained one
(`next_rival_state_csv`; at the `d_floor` the call REFUSES and the coin must be renewed, rolled over
or re-anchored) — so the receiver out-races all of them (INV-28), and discloses the state it
supersedes. The receiver runs `verify_bundle` — the census (REQ-38) — and rejects unless the final
state pays the RECEIVER's own seed-derived key. `sdk47`, `sdk49`.

`TransferMsg.protocol_version` is a **message-shape tag, not a protocol version of the system**
(there is one protocol): `0` = branch/backup message (un-laddered), `2` = a conveyed TES-R ladder,
`4` = a split-child bundle carrying the key handover (`3` is the superseded no-handover child
conveyance). The receiver dispatches its validation on this tag.

**REQ-15** `transfer(address, amount)` MUST move exactly `amount`: either an exact subset of coins
(§5.1) or an off-chain split minting the exact piece (§6). No dust or overpayment.
**REQ-16** The receiver MUST validate: transfer signature binds the coin to its key; tx0/branch is
valid; latest backup pays the receiver; backup locktimes decrement correctly (INV-5, enforced on
BOTH shapes); and the co-signature count reconciles — `num_sigs == backups` on the un-laddered
shape, the census (REQ-38) on the laddered one.
**REQ-17 (G1)** For a branch-carrying (sub-coin) transfer, the receiver MUST verify every
`terminal_parents` ancestor is terminal at the SE (`GET spend_budget`, `terminal==true`) and
reject otherwise (ERR-7). That endpoint is the COORDINATOR's own answer, computed from its Postgres;
this un-laddered lane still rests on it (`verify_terminal_parents`). On the laddered/child lane it
has been DEMOTED: terminality is derived from the enclave-signed `(num_sigs, sig_budget)` payload
(`budget exists ∧ num_sigs ≥ budget`, `attested_terminal`), and the coordinator's answer is kept
only as a cross-check that REFUSES on disagreement — the two stores hold the same absolute quantity,
so a mismatch means one was written behind the other's back.
**REQ-38 (census)** A receiver of a laddered coin, or of a split child (§6.3), MUST reject unless
the SE's ATTESTED co-signature count (§3.3 — an unattested count MUST be refused, since the census
rests entirely on it) equals EXACTLY the tiers it was shown:
`se_num_sigs == flat_backups + Σ conveyed tiers + Σ disclosed superseded tiers`, summed over every hop
of the conveyed ancestor chain (N-hop for a re-transferred child). Each disclosed superseded tier
MUST be parsed, linked to the ladder, signature-checked, and carry a strictly HIGHER CSV than the
tier that replaces it — a `.len()`-only count is paddable, and an unparsed `csv: None` skipped the
race check. Any hidden co-signed state shows up as a count mismatch and MUST reject (ERR-15). The
count is retry-safe: a repeated `sign/second` returns the cached partial signature and does NOT
advance `sig_count` (`sdk56`), so an in-flight retry cannot brick the equation. Verified by `sdk46`
(count formula against the real SE), `sdk54`/`sdk55` (padding/spoof attacks REJECT), `sdk58`
(11 child-bundle attacks REJECT).
**INV-8** Claiming is idempotent: repeated `claim()` passes book each transfer at most once.

### 5.1 Coin selection
`select::plan(coins, target)` returns `Exact(subset)` if a subset sums to `target`, else
`WithSplit{whole, split, split_amount}`, else `Insufficient{available}`.
**INV-9** `Exact(s)` ⟹ `Σ coins[s] = target`. `WithSplit` ⟹ `Σ whole < target ∧ split_amount =
target − Σ whole ∧ coins[split] > split_amount`. `Insufficient` ⟸ `Σ coins < target` — but the
reverse does **not** hold: since audit [29] the planner also returns `Insufficient` when the
remainder can only be minted as an unviable piece (no split candidate covers
`remainder + fee_reserve + min_split_output`, where `min_split_output = 330` (dust) `+` the sub-coin's
own backup fee at the live rate = `330 + ceil(112 · fee_rate)`; the planner also requires
`remainder ≥ min_split_output` so the minted piece can fund its own backup —
`select::plan_with_floor`, `transfer::min_split_output`).
See [GRANULARITY-SPEC.md](GRANULARITY-SPEC.md) GRN-REQ-5 (whose fee arithmetic describes the
un-laddered/colored split; the in-ladder split has its own model, §6.1).

**Executor floor (laddered parent).** `min_split_output` is the PLANNER's floor. When the chosen
parent is laddered, the in-ladder executor applies a second, strictly larger floor and the larger of
the two binds: a child funds its OWN extension + state tier before it can clear dust, so
`min_child_value = 2·(committed_fee(rate) + 240) + 330` — **1310 sat** at the default 2 sat/vB
committed rate (`2·(250 + 240) + 330`; the superseded 1306 priced a 124-vB tier, before [D4]
measured 125), versus the old 442. Both the piece and the change MUST clear
`max(min_split_output, min_child_value)` or the split is refused UP-FRONT (ERR-16). Up-front is
load-bearing: `establish_child` runs AFTER the parent's spend budget is consumed and `SP` is
co-signed, so admitting a child below the floor terminalized the parent and THEN failed, stranding
it to unilateral-exit-only. (Defect found and fixed during this migration; it also broke
`refresh_sponsored`, §9.4.)

---

## 6. Off-chain split & combine

### 6.1 In-ladder split (laddered coins)
A non-exact payment out of a laddered coin is an **in-ladder split** (`in_ladder_pay`; `transfer()`
routes here automatically). `SP` is a SPINE state tier spending `X_m.out[0]` at `SPINE_CSV = 0` — a
DESCENDANT of the trigger, never a rival for `F`, and strictly below the `S_0` it replaces on that
output — carrying one resting output per child plus the P2A anchor; each child then hosts its own
extension + state tiers (`establish_child`). The parent is terminalized
before the co-sign and its superseded state disclosed for the receiver's census (REQ-38).

**REQ-39 (in-ladder split)** A laddered coin MUST NOT be split as plain BTC: a prior owner's
retained no-timelock trigger could spend `F` and void a split of it while the ladder still paid the
splitter the whole coin [B1]. The split MUST descend from the trigger, value MUST be conserved
exactly — `Σ children == tier_out_total(X_m.out[0], n) = X_m.out[0] − committed_fee_for_outputs(n)
− 240`, where `committed_fee_for_outputs` adds 43 vB per extra child so the tier still relays
standalone — and every child MUST clear the §5.1 executor floor before the parent's budget is
consumed (ERR-16). Verified by `sdk58` (accept + 11 adversarial cases REJECT: aggregates,
hidden-state, Model-A payee, parent terminality, child-superseded race, count-padding, value-spoof),
`sdk59` (end-to-end split payment), `sdk04` (the terminalized parent refuses a second spend at both
the wallet and the SE).

### 6.2 Branch split & combine (un-laddered coins, colored splits)
This is the shape RGB rides (§7) and the only one left for a coin that cannot be laddered.
`split_coin(id, piece)` builds one SE-co-signed, un-broadcast tx spending the coin into
`{piece sub-coin, change sub-coin}` (minus a fee reserve), records both as sub-coins with their own
backup ladders, the shared exit branch, and ancestor records; sets the parent's spend budget to 1.

**REQ-18** Before co-signing a split/combine, the SDK MUST set the parent(s)' `spend_budget` to 1
(exactly one more co-signature). After the split, each parent MUST be terminal.
**INV-10** `piece_sats + fee_reserve < parent_sats`; `change_sats = parent_sats − piece_sats −
fee_reserve`. `fee_reserve = clamp(parent_sats/100, 300, 2000)`.
**INV-11** A split tx has exactly one input (the parent) and one output per split entry plus, for
colored splits, one OP_RETURN; `output_vouts.len() == splits.len()`.
**Combine.** N coins → M outputs in one SE-co-signed (per-input) tx; each input matched to its coin
by outpoint; per-input MuSig2 over all prevouts.

### 6.3 First-class split children
A RECEIVED in-ladder split child is a **first-class coin**, not an exit-only claim
([CHILDREN.md](CHILDREN.md)).

**REQ-40 (child handover)** Conveying a child MUST include the standard SE key-handover material,
and the receiver's claim MUST COMPLETE that handover (`/transfer/receiver`) after the census passes:
the SE rotates its share so `A_child` is INVARIANT (every pre-signed child tier stays valid) and the
sender is permanently locked out (auth rotated). The child is deliberately left NON-terminal — its
safety is the census (REQ-38) against any pre-conveyance rival plus the pending-transfer lock
(REQ-36) against a post-conveyance one. These MUST hold together: a non-terminal child conveyed
WITHOUT a completed handover and held past the lock's expiry could be out-raced by the still-owner
sender. The one exception is a Lightning-latched piece, which stays terminal (INV-30).
**REQ-41 (onward payment)** A first-class child MUST be payable onward off-chain, either WHOLE
(`child_retransfer` — co-sign a fresh state over `ext_child.out[0]` at a strictly lower CSV paying
the new recipient, disclosing the state it replaces) or SPLIT (`child_in_ladder_pay` — the child's
state is replaced by a split state paying two grandchildren, giving a depth-2 ancestor chain). Each
hop costs exactly ONE co-signature and discloses exactly ONE superseded state, which the next
receiver's N-hop census counts and proves out-raced. Verified by `sdk60` (alice → bob → carol, the
funding outpoint unspent throughout, carol exits to her own key) and `sdk17` (multi-hop with a
partial second hop). A cooperative `withdraw` of a child is not possible — its funding `SP.out[j]`
is un-broadcast, so there is no confirmed outpoint to spend — and MUST be routed to the unilateral
exit instead (§9.2).

---

## 7. Tokens (RGB)

Assets are RGB contracts (NIA fixed-supply, IFA inflatable). Allocations ride coins/sub-coins on the
UN-LADDERED shape (§6.2) — that path exists for tokens and is load-bearing, not legacy.

**INV-29 (terminal freeze)** An RGB **carrier** is NEVER laddered: a plain T/X/S tier spend is
sats-only and would destroy the allocation, so carriers are structurally excluded from ladder
establishment (REQ-37), from plain re-anchor (REQ-32), from plain withdraw/unilateral exit, and from
watch bundles that carry a sats-sweeping backup (REQ-34). Correspondingly, a colored tx only ever
spends outputs of TERMINALIZED structure (terminalization precedes the colored co-sign, and the SE
refuses renewal on a terminal node), so no ancestor of an RGB anchor is ever re-signed and **no
superseded colored witness exists anywhere in the system** — consignments carry un-broadcast witness
txs, which is the model rgb-lib already supports. (PROTOCOL.md §5.10.) Verified by `sdk52` (in one
wallet the plain coin carries a ladder, the carrier carries none, and an off-chain RGB transfer
still settles) and `sdk32`.

**Issuance.** `issue_token`/`issue_inflatable_token`: issue in the RGB engine, then fund + register
a statechain coin as the carrier in one colored on-chain tx.
**REQ-19** IFA issuance MUST create one colorable UTXO per allocation (fungible + each
inflation-right) before issuing.
**INV-12** After issuance the carrier holds the full fungible `supply`; IFA inflation-right stays
free in the engine.

**Mint (IFA).** `mint_tokens`: on-chain inflate in the engine, then bind the newly-minted allocation
to a fresh statechain coin.
**REQ-20** `mint_tokens` MUST isolate the newly-minted allocation (pre-inflate snapshot) so binding
never consumes already-bound supply.
**Burn.** `burn_tokens` burns engine-held free balance (on-chain). Statechain-bound supply must be
exited first.

**Transfer.** `transfer_tokens`/`batch_transfer_tokens`: a colored off-chain split carves the
recipient piece(s) + change; the consignment rides `BackupTx.rgb_consignment` as a
`ConsignmentEnvelope{c, a, s}`. When no single carrier holds the requested amount, the transfer
automatically COMBINEs several carriers of the same asset (`colored_combine_transfer`) into one
SE-co-signed colored combine tx (N input carriers → recipient piece + change), conserving the
asset's allocation across all combined inputs.
**REQ-21 (G2)** The receiver MUST book the amount the CONSIGNMENT assigns to its own witness
outpoint (`accept_offchain_amount`), treating the envelope amount `a` only as a cross-checked hint;
a mismatch MUST reject the transfer (ERR-8).
**REQ-22** The receiver MUST book under the consignment's cryptographically-verified `contract_id`,
not a sender-claimed id.
**INV-13** Token conservation: for a (batch) transfer, `Σ recipient amounts + change =
Σ allocations of the combined input carriers` (a single-carrier transfer is the N=1 case).
**N/A** RGB has no issuer freeze (no consensus enforcement point); documented, not faked.

---

## 8. Lightning swaps (SSP)

Both directions work on the laddered lane, via a HODL-invoice latch
([LIGHTNING.md](LIGHTNING.md)). Each direction has an EXACT lane (the wallet already holds, or can
mint, a coin of the exact size — the whole coin is latch-transferred) and a NON-EXACT lane (the coin
is split IN-LADDER and the latched PIECE is conveyed, §6.1).

### 8.1 Pay (Mercury → Lightning)
`pay_lightning_invoice(ssp, invoice)`: obtain the exact coin, `create_external_hash_latch` bound to
the invoice's payment hash, hand the coin to the SSP; the SSP pays the BOLT11; the LN preimage
`unlock_by_preimage`s the coin and is returned to the payer as proof.

**REQ-23** The SSP MUST verify the latch hash equals the invoice payment hash before paying, and
MUST run its pre-payment value gate — `verify_bundle` / `verify_conveyed_child` over the conveyed
ladder — BEFORE `send_payment`, pricing against the value the ladder cryptographically commits to
(`sdk37`, `sdk63`).
**REQ-42 (one-call pay routes both lanes)** `pay_lightning_invoice` MUST NOT depend on minting an
exact coin: when no exact coin can be minted it MUST fall back to the non-exact in-ladder lane
(`pay_lightning_invoice_inladder`), the same way the receive side does. Without that fallback the
one-call API refused every laddered coin — i.e. every coin — and was unusable (defect found and
fixed during this migration). `sdk63` (exact), `sdk65` (non-exact).
**INV-14 (atomicity)** The SSP can claim the coin **iff** it holds the preimage, which exists **iff**
the invoice was paid. No payment ⟹ latch expires ⟹ payer keeps the coin. The returned preimage
MUST satisfy `sha256(preimage) == invoice_hash`.
**REQ-43 (failed pay is recoverable)** A pay that fails after the coin was latched MUST leave the
value fully recoverable. Non-exact: `pay_lightning_invoice_inladder` MUST ROLL BACK — the optimistic
booking is wrong while `SP` is un-broadcast, so the parent is restored as exitable and the piece +
optimistic change are dropped, returning the WHOLE parent (`sdk66`). Exact: the orphan `S'` co-sign
inflates `sig_count`, so `reclaim_lightning_payment` MUST restore the coin locally as exitable
(value intact via the ladder; onward re-transfer is census-bricked until a `refresh`) rather than
attempt a self-transfer that would fail `verify_bundle` (`sdk68`).

### 8.2 Receive (Lightning → Mercury)
`create_lightning_invoice(ssp, amount)`: the SSP latch-transfers a coin to the user under an
SE-minted preimage and issues a HODL invoice on that hash; on payment the SSP confirms the latch
(releasing the coin) then retrieves the preimage and claims the HTLC. When the SSP holds no coin of
the exact size it fronts an in-ladder split PIECE instead; `settle_receive` is unchanged (it
operates on the piece's statechain id).

**INV-15 (atomicity)** The SE reveals the preimage only after the latch is unlocked (coin released),
so the SSP can take the HTLC money **only after** the user's coin is claimable. No payment ⟹ latch
expires ⟹ SSP keeps its coin. A wallet with zero on-chain presence can receive. No operator trust is
needed in this direction: the SSP owns the coin throughout its risk window. `sdk64` (exact),
`sdk67` (non-exact), `sdk19`/`sdk24`/`sdk25` (unpaid, cancelled, delayed-claim).
**INV-30 (latched-piece terminality)** A latched in-ladder piece is deliberately left unclaimed
until a preimage lands — precisely the situation the TEMPORARY pending-transfer lock (REQ-36) does
not cover, since it expires with the batch window and the receiver cannot complete the handover
until the latch releases. So for the latched lane, and ONLY there, the piece child is terminalized
at the SE before conveyance (while the sender still holds its auth key), permanently closing the
post-expiry rival window. Plain in-ladder payments rely on the pending lock plus the receiver's
prompt handover instead (REQ-40).

---

## 9. Exit

### 9.1 Cooperative (normal)
`withdraw(address, coins?)`: the SE co-signs a fresh direct spend to L1. For sub-coins the branch
is materialized first (branch txs are locktime-free). One on-chain tx per coin, no wait. A token
carrier MUST be excluded from the withdraw-everything default and hard-error if named (an
RGB-unaware sweep destroys the allocation, INV-29). A split child has no confirmed outpoint to
spend, so it is routed to the unilateral exit instead and booked `WITHDRAWING` (§2.1, §9.2).

### 9.2 Unilateral (SE gone)
`unilateral_exit(coins?)` dispatches on the coin's shape:
- **Laddered** — walk the tier chain: broadcast the trigger, then each extension/state as its
  relative-CSV matures (`exit_pass`). No absolute-locktime backup is broadcast. Idempotent and
  incremental: call once per block until `complete`. `sdk50` (SDK surface), `sdk40` PART 1
  (consensus: each tier is REJECTED before its CSV is met, accepted after).
- **Split child** — the same walk over the full pre-co-signed chain
  `T → X_m → SP → ext_child → state_child` (`exit_child_pass`), whose final state already pays this
  wallet's own key. This is also where a cooperative `withdraw` of a child is routed (§6.3).
- **Un-laddered** — broadcast the exit branch (instant, locktime-free) then the coin's latest
  pre-signed backup, subject to its absolute locktime.

**REQ-24** `unilateral_exit` MUST require no SE interaction.
**REQ-25** A tier or backup whose timelock is unreached MUST be reported as
`ExitStatus{complete:false, wait_blocks>0}`, not an error; callable again after the wait.
**REQ-44** `unilateral_exit` MUST refuse a coin that is not `CONFIRMED` even when named explicitly —
exiting a parent already consumed by a split would kill the tx funding the receiver's child [B1] —
and MUST refuse a token carrier (an RGB-unaware spend destroys the allocation, INV-29).
**INV-16** After the chain confirms, funds are at the owner's address; RGB allocations settle
on-chain.

### 9.3 Cost
`estimate_exit_cost(coin)` → `{branch_txs, branch_vbytes, backup_vbytes, total_vbytes, wait_blocks,
exit_deadline_block}`.
**INV-17** `total_vbytes = branch_vbytes + backup_vbytes` (measured from the actual pre-signed txs);
`fee_sats_at(rate) = ceil(total_vbytes · rate)`; `wait_blocks = max(0, backup_locktime − tip)`.
**Scope (stated honestly).** `estimate_exit_cost` measures the UN-LADDERED material only — the
stored branch plus the latest absolute-locktime backup — and it is also what feeds the calendar
deadline used by REQ-33. It does NOT yet account for a laddered coin's tier chain, whose cost and
wait are structural instead: 3 pre-signed tiers = 375 vB (3 × `TIER_VBYTES` 125, plus up to 3 P2A
fee children in a spike) and a sequential `E_m + Δ_k` CSV wait; each split level adds 2 tiers
(293 vB — an `SP` with two payload outputs, plus an extension) and ONE extension CSV, because the
`SP` itself is a spine tier at CSV 0 and waits only for its parent to confirm
(`config::tesr_exit_vbytes` / `tesr_exit_wait_blocks`, PROTOCOL.md §5.9).
A laddered coin has no calendar deadline at all (INV-27), so `exit_deadline_block` is `None` for it.

### 9.4 Refresh (cooperative on-chain re-anchor)
`refresh(id, fee_rate?)` / `refresh_sponsored(id, sponsor, fee_rate?)`: one SE-co-signed single-input
spend of the coin's current 2-of-2 outpoint into a FRESH deposit aggregate (a new `statechain_id`,
same owner; a sub-coin's exit branch is materialized first).

Refresh is **no longer a deadline reset** — a laddered coin has no calendar deadline to reset
(INV-27). It is the **re-anchor primitive**: the escape hatch that moves a coin out of its current
ladder/branch and permanently kills every exit right rooted at the old outpoint. For an un-laddered
coin it is still the way to escape the backup-ladder floor without going to L1 (§2.4).

**REQ-31** `refresh` MUST spend the current outpoint into a fresh aggregate, which then gets a fresh
full ladder of its own (REQ-37); because the old outpoint is now spent, EVERY exit right rooted at
it — every previous owner's backup and every old tier — is permanently invalidated. It is
COOPERATIVE (it needs the SE); if the SE is gone the owner exits unilaterally (§9.2) instead. The
fee is drawn from the coin (single-input, blind SE), so the user-pays variant yields `amount − fee`.
`refresh_sponsored` reimburses that fee OFF-CHAIN from a funded sponsor; because the rebate is a
non-exact payment out of the sponsor's own (laddered) coin it is minted by an in-ladder split, so
the rebate MUST be sized to `max(fee + dust, min_child_value)` — 1310 sat at 2 sat/vB, not the old
442. Sizing it into that dead window made every sponsored refresh fail AFTER the user had already
paid the on-chain fee (defect found and fixed during this migration). The operator absorbs the
difference; the user ends ≥ whole. `sdk30` (a)/(c), `sdk38` (a broke sponsor loses boundedly).

**REQ-32 (auto-refresh)** When `SdkConfig::auto_refresh` is set (default), the SDK MUST re-anchor a
coin nearing its BACKUP-ladder floor before it is spent, transparently: `auto_refresh_due(margin)`
re-anchors every confirmed, non-carrier coin whose headroom (`locktime − tip`) is ≤
`auto_refresh_margin_blocks`, and `transfer`/`transfer_many` MUST run it (and await the fresh coins'
confirmation) before selecting coins. Token CARRIERS are excluded (a plain re-anchor would destroy
the allocation, INV-29). Routine BACKGROUND refreshing is default-OFF
(`background_auto_refresh = false`): the re-anchor is paid on demand as part of a payment's fee, so
an idle wallet never silently shrinks; deadline safety for idle wallets is REQ-33.

> **Coverage note.** This requirement's dedicated E2E (`sdk33`) was retired with the one-protocol
> migration and has NO live replacement — the pass itself is not exercised end-to-end today. The
> underlying re-anchor is covered by `sdk30`, and the property REQ-32 was created to guarantee (a
> coin never becoming un-spendable by aging) is now STRUCTURAL for laddered coins rather than
> maintained by this pass: idle coins never age (INV-27) and renewal is off-chain and unbounded
> (`sdk43`). REQ-32 remains normative for the un-laddered shape, whose backup ladder still ages.

### 9.5 Watchtower (automatic deadline protection)
Two passes, one per shape.

**Calendar pass (un-laddered).** `auto_exit_due(margin)` protects any owned coin with a branch that
is within `margin` blocks of its deposit-anchored exit-race deadline (§9.3), before an ancestor can
broadcast a stale backup. The background watcher MUST run it each poll when `SdkConfig::auto_exit`
is set (default), with `auto_exit_margin_blocks` — **derived, never chosen**:
`k_max·interval + tesr_exit_txs(d)·144`, i.e. **860** blocks on regtest (`14·10 + 5·144`) and
**2 120** on mainnet (`14·100 + 5·144`), because the exit walk lands `3 + 2d` transactions ONE AFTER
ANOTHER and each must confirm before the next tier's relative lock starts counting
(`config::auto_exit_margin_blocks_for`). The superseded literal was 288 (≈ 2 days): one
confirmation window for a whole walk, against a `k·interval` gap that on mainnet alone is 1 400
blocks. The walk's own `Σ csv` is deliberately NOT folded in here — `auto_exit_due` takes that head
start per coin, off the coin's own chain. A laddered coin has no such deadline and is not its
subject.

**Alarm pass (laddered).** `defend_ladders()` is event-driven, not calendar-driven: it is a no-op
while the coin sits un-broadcast (nothing ages), and reacts when someone ELSE spends the coin's
funding `F` — a hostile trigger by a prior owner or a griefer — by broadcasting the owner's own
tiers as each relative-CSV matures. Because the adopted current state carries the strictly lowest
CSV (INV-28), it matures first and the funds land at the OWNER's key. `sdk51`.
**REQ-33** For a **plain** sub-coin the watchtower MUST force a unilateral exit (§9.2). For a
**received token carrier** — which the plain exit refuses — it MUST instead MATERIALIZE the coin by
broadcasting ONLY its exit branch (settling the RGB allocation on-chain and spending the shared
root), NEVER the sats-sweeping backup; it emits `TokenCarrierMaterialized`. An issued/flat carrier
has no exit branch (no ancestor, no clawback risk) and MUST be skipped. This gives a received token
the same automatic clawback protection plain coins already have. `sdk34`.

**REQ-34 (keyless watch delegation)** A watch bundle MUST emit, per off-chain coin, only pre-signed
exit material and public metadata — the exit branch/tier chain, the timelock schedule, and (plain
coins only) the latest backup tx — and MUST contain NO key material; a token carrier's entry MUST
omit the backup tx entirely (structurally denying an RGB-destroying sweep, INV-29). A tower MUST be
able to protect the bundled coins with only an electrum connection (no wallet, DB, SE, or keys),
tolerate idempotent re-broadcasts (so N independent towers compose without conflicting), and surface
genuine rejections. Every tier pays the owner, so a malicious or buggy tower can only settle funds
to the owner early or do nothing. The full trust analysis is [TRUST-MODEL.md](TRUST-MODEL.md) §5.
Verified by `sdk45` (a keyless tower loaded from the persisted bundle alone drives an offline
owner's exit against a griefer's trigger; the bundle asserted to carry zero key material; a SECOND
independent tower over the same bundle is harmlessly idempotent), `sdk51` (the in-wallet pass),
`sdk52` (carriers structurally excluded), plus `unit::watchtower::tests`.

---

## 10. Invalidation & security invariants

> Invalidation now has TWO mechanisms, one per coin shape.
> - **Laddered**: relative-CSV replacement — a lower-CSV tier out-races and orphans the one it
>   supersedes (INV-28), disclosed to the receiver and checked by the census (REQ-38). The
>   normative treatment is [PROTOCOL.md](PROTOCOL.md) §5.5/§5.7/§5.11. There is no ladder
>   formula, no calendar deadline and no re-anchor rent on this shape (INV-27), so the
>   deposit-anchored deadline arithmetic (including the audit-[17] `k·interval` gap) does not
>   apply to it.
> - **Un-laddered**: the absolute-locktime decrementing ladder — the mechanism specified in
>   [INVALIDATION-SPEC.md](INVALIDATION-SPEC.md) (IVL-REQ/IVL-INV/IVL-ERR numbering), which
>   remains authoritative FOR THAT SHAPE where it overlaps with the summary below.

**INV-18 (no old state)** Split/combine spend into NEW outpoints; a child cannot confirm before its
parent (its input is the parent's output), so there is no old-vs-new race within a tree. On the
laddered shape the split state `SP` additionally DESCENDS from the trigger rather than racing it,
which is what closes [B1] — a prior owner's retained no-timelock trigger can only start the clock on
the current owner's own chain, never void the split. Verified by `sdk58`/`sdk59`.
**INV-19 (fork prevention)** The SE refuses a second spend of any node (single-use / spend budget),
so a node cannot be forked into two conflicting children. Verified by `sdk04` (a terminalized
in-ladder parent is refused a second split at the SE, and the refusal is pinned to terminality
rather than to an incidental plumbing error), `rgb04` (single-use).
**INV-20 (terminal ancestors)** A sub-coin's receiver only accepts it if every structural ancestor
is terminal at the SE (REQ-17) — a malicious sender cannot double-spend a parent afterwards. The
receiver derives the required ancestor count from the branch itself: it requires at least one named
terminal ancestor **per branch hop** (`n_parents ≥ branch_len`, `≥ 1`), so a sender cannot hide a
non-terminal, double-spendable ancestor by shipping an empty or short `terminal_parents` list
(ERR-7). Verified by `unit::terminal_parents_tests` (the count binding, including the
empty/short-list cases) and, on the laddered shape, by `sdk58`'s parent-terminality attack (a child
whose parent is not terminal is REJECTED). Honest-accept paths: `sdk29`/`sdk31`/`sdk39` (token
transfers over real branches). *Coverage gap:* the branch-lane E2E that rejected a non-terminal
ancestor end-to-end (`sdk10`) was retired with the migration; the guard is unchanged in code
(`verify_terminal_parents`) but only the unit test and the laddered-lane equivalent now exercise it.
**INV-21 (bounded lifetime)** With `epoch_deadline` set, the SE stops co-signing new state past the
deadline; unilateral exit still works forever (needs no SE), so funds are never swept.
**INV-22 (UTXO granularity)** Exact amounts are native (1-sat resolution) via off-chain split —
strictly finer than fixed-denomination leaves. The resolution is unchanged by TES-R; only the
minimum viable PIECE moved, from the backup-fee floor to `min_child_value` on the laddered shape
(§5.1), because a child now funds two exit tiers instead of one backup.
**INV-23 (nonce single-use)** The SE binds each server nonce to exactly ONE challenge: `sign/second`
sets the challenge atomically only if it was NULL (or identical — idempotent retry) and otherwise
refuses (ERR-12). A second finalize over one nonce with a different message is therefore impossible,
which is what makes the blind-MuSig2 scheme safe against an owner who controls the raw signing
requests — without it, two partial signatures over one secnonce would leak the SE's key share and
yield two co-signed conflicting spends while `count_finalized_signatures` (and hence single-use /
budget / epoch enforcement) counted only one. Verified by `sdk12` Part C.
**INV-24 (budget monotonic)** `set_spend_budget` may only TIGHTEN a coin's `sig_budget`
(`new = min(existing, count+remaining)`); it can never raise it, so an already-terminal node cannot
be re-opened for a second conflicting spend. This is why a first-class split child is handed over
with a KEY ROTATION and a releasable pending lock (REQ-36/REQ-40) instead of a budget re-open: any
re-open would resurrect exactly the fork class this clamp prevents. Verified by `sdk04` (a terminal
node stays terminal and refuses a second spend) and `unit::invalidation_model::terminal_predicate_matrix`.
**INV-25 (branch value conservation)** The receiver's `validate_branch` rejects any exit branch whose
txs create value (`Σ outputs > Σ inputs` at any hop): `tx.verify` checks scripts but not the fee
rule, so without this a sender could hand over a coin whose branch is script-valid yet un-broadcastable
(the receiver could never exit on-chain while the sender keeps the funds). The guard is unchanged in
code (`transfer_receiver::validate_branch`) and honest branches are still accepted by every token
E2E (`sdk29`/`sdk31`/`sdk39`). *Coverage gap:* the E2E that fed a value-INFLATING branch and asserted
the rejection (`sdk10`) was retired with the migration and has no live replacement — the reject side
of this invariant is currently unexercised. The laddered shape's analogue (a value-spoofed tier) IS
covered, by `sdk54`/`sdk58`.
**INV-26 (received amount = spendable only)** A transfer's received token amount counts only
`Fungible` assignments, never `InflationRight` (the right to mint). Booking an inflation right as
spendable balance would let a right-holder inflate a receiver's balance out of nothing
(conserves INV-12/INV-13). Verified by `sdk09`.

---

## 11. Error semantics

- **ERR-1** single-use second spend → HTTP 410 `single-use coin already spent`.
- **ERR-2** past epoch deadline → HTTP 4xx epoch refusal.
- **ERR-3** spend budget exhausted → HTTP 410 `spend budget exhausted`.
- **ERR-4** preimage requested while latch locked → HTTP 404 `not available ... still locked`.
- **ERR-5** wrong preimage on `unlock/preimage` → HTTP 403.
- **ERR-6** deposit token requires payment → `SdkError::TokenPaymentRequired{token_id,
  deposit_address, fee_sats}`.
- **ERR-7** non-terminal ancestor → receiver validation error `structural parent ... is NOT
  terminal`, transfer not booked.
- **ERR-8** consignment/envelope amount mismatch → receiver rejects `consignment assigns X ...
  envelope claimed Y`.
- **ERR-9** `InsufficientBalance{requested, available}` on over-balance transfer.
- **ERR-10** double-withdraw / spend of a non-CONFIRMED coin → refused with the coin's status.
- **ERR-12** second `sign/second` reusing a server nonce over a different message → HTTP 409
  `server nonce already finalized with a different challenge`.
- **ERR-13** derived-token refusals: bad, replayed, or non-owner `auth_sig` → HTTP 401; `count`
  outside `1..=cap` → HTTP 400 `count must be between`; lifetime allowance exceeded → HTTP 429
  `lifetime derived tokens`; issuance disabled (`cap = 0`) → HTTP 403.
- **ERR-14** co-sign or re-address attempted while a transfer of the coin is open (REQ-36) → HTTP 409
  `coin has an open transfer ...` / `coin already has an open transfer to a different recipient`.
  A same-recipient retry is idempotent, not an error; the lock releases on `key_updated` or at
  `batch_timeout`.
- **ERR-15** census mismatch (REQ-38) → receiver validation error from `verify_bundle` /
  `verify_child_bundle`, transfer not booked (`num_sigs`/tier-count mismatch, an unlinked or
  unsigned superseded tier, a superseded CSV that ties or wins, or a ladder not exiting to the
  receiver's own key).
- **ERR-16** in-ladder split below the admission floor (§5.1) → refused BEFORE the parent's budget is
  consumed: `in-ladder split refused — the piece falls short` / `… the change falls short` / `… both
  legs fall short`, naming each leg's own floor. The two legs are floored independently
  (`SplitFloors { piece, change }`): a piece always funds two rungs, the change funds whatever
  `change_leg_role()` says THAT LANE's builder gives it. At HEAD that is ONE rung
  (`min_spine_tip_value` = 820 sat plain / `colored_spine_tip_floor` = 906 coloured, at 2 sat/vB) on
  the plain-root, spine-batch AND coloured lanes — CATS change 2 has landed on all three — and two
  rungs only on the plain-CHILD lane, where the change is still carved as a `Piece`.

---

## 13. Query, utility & invoice API

Client-side conveniences (no new SE state); mirror Spark's query/signing/invoice surface.

**REQ-26** `sign_message_with_identity_key(msg)` MUST produce a BIP340 Schnorr signature over
`sha256(msg)` under a STABLE identity key (derived at `m/1000h/0h/0h`, unchanged as coins come and
go); `validate_message_with_identity_key(msg, sig, pubkey)` MUST verify it and reject a tampered
message.
**REQ-27** `transfer_many(recipients)` MUST pay each recipient its exact amount from one off-chain
split (N pieces + change), with the same branch + terminal-parent guarantees as a single transfer
(REQ-17/REQ-18).

`transfer_many` MUST dispatch on the parent's shape exactly as single-recipient `transfer()` does: a
laddered ROOT coin through a MULTI-CHILD in-ladder split (one `SP` over `X_m.out[0]` carving N
recipient children plus change), a received CHILD through the child-level equivalent, and only an
un-laddered coin through the plain N+1 branch split (§6.2). A plain split of a laddered parent is the
shape REQ-39 forbids — the split tx and the coin's trigger both spend `F` — so it MUST NOT be built.

> **Was a known divergence, now FIXED.** `transfer_many` used to build the plain split directly on
> any parent, carrying neither the routing nor the `split_coin` refusal. Harmless when the parent was
> self-deposited (only the owner holds `T`), but a RECEIVED laddered coin left its previous owner
> holding a broadcastable no-timelock `T` that could void the split after the pieces were handed
> over: live [B1]. `sdk69` now proves the fixed shape by executing that attack — the retained trigger
> is broadcast and spends `F`, and both recipients still exit unilaterally for their exact amounts,
> because `SP` descends from the trigger instead of racing it. `sdk11` asserts the route as well as
> the amounts.

**REQ-28** `create_sats_invoice`/`create_tokens_invoice` MUST encode {address, amount, asset?,
memo?, expiry?} into a `utexoinv1…` string that round-trips through `decode_utexo_invoice`;
`fulfill_utexo_invoice` MUST reject an expired invoice (ERR-11) and otherwise pay the embedded
amount/asset to the embedded address.
**REQ-29** `list_coins`/`get_transfers`/`get_transfer` MUST reflect the wallet's current coins and
activity; `get_withdrawal_fee_quote` MUST return a positive fee at the electrum-estimated rate.
**REQ-30** `get_token_l1_address` returns the RGB engine funding address; `query_token_transactions`
returns the contract's transfer history.

- **ERR-11** `fulfill_utexo_invoice` on an expired invoice → `invoice expired at …`.

## 12. Traceability

Each requirement/invariant is verified by at least one test. Pure-logic items have unit tests;
protocol items have E2E tests (regtest). See [testing-guide](build/testing-guide.md) for how to run.

| Item | Test |
|---|---|
| REQ-1, REQ-3 | design (2-of-2 keys); exercised by every co-sign flow |
| REQ-2, REQ-24, REQ-25, REQ-44, INV-16 | `sdk50` (SDK unilateral exit walks T→X→S to the owner's key), `sdk40` PART 1 (consensus: each tier rejected before its CSV, accepted after), `sdk58` (child chain exits to the receiver) |
| INV-17 (un-laddered exit cost) | `unit::types::tests::exit_cost_math`, `unit::invalidation_model::exit_cost_scaling_model`, `sdk39` (depth-2 token exit) |
| REQ-4, REQ-14, ERR-6, INV-7 | `sdk01` deposit; `unit::types::tests::error_semantics` |
| REQ-5, ERR-1 | `rgb04` (single-use refusal) |
| REQ-6, ERR-2, INV-21 | `rgb07` (epoch deadline) |
| REQ-7, REQ-13, REQ-18, ERR-3, INV-19 | `sdk04` (terminalized in-ladder parent refuses a second spend, at the wallet and at the SE), `unit::types::terminal_predicate`, `unit::invalidation_model::terminal_predicate_matrix` |
| REQ-8, REQ-9, REQ-15, REQ-16, INV-5, INV-8 | `sdk01`, `sdk04`, `sdk41` (receiver gains control, sender locked out), `sdk55` (backup chain cannot be padded or inverted), upstream `tb01/tb05/tm01/ta02/ta03` |
| REQ-36, ERR-14 (pending-transfer lock) | `sdk49`/`sdk41`/`sdk01` + `sdk58`/`sdk59` (green with the lock live — i.e. the sender pre-sign re-ordering is correct and no honest flow is blocked); `sdk60` (a child conveyed under the lock is claimed and re-transferred). **Gap:** no test drives the adversarial refusal itself (a sender co-signing a rival, or re-addressing, inside the open window) |
| REQ-37 (ladder establishment) | `sdk48` (auto-established, seed-derived payee, idempotent), `sdk52` (carrier excluded) |
| REQ-38, ERR-15 (census) | `sdk46` (count formula vs the real SE), `sdk47` (ladder carried across a transfer), `sdk54`/`sdk55` (padding/spoof REJECT), `sdk58` (11 child-bundle attacks REJECT), `sdk56` (retry does not advance the count) |
| INV-27 (idle coins never age), INV-28 (lower CSV wins) | `sdk30` (a) (300 blocks, chain byte-identical, `F` unspent), `sdk40` PART 2/PART 3, `sdk41`, `sdk51` |
| Off-chain renewal + rollover (§2.6) | `sdk42` (renew → persist → reload), `sdk43` (rollover to a fresh level, then exit the deep chain), `sdk44` (the whole cadence driven from the canonical `TesrParams` schedule via `establish_auto`/`renew_auto`/`rollover_auto`) |
| REQ-10, ERR-4 | `sdk19` (never paid → preimage withheld, receiver cannot claim), `sdk25` (a receiver who delays past the latch window loses the ability to claim), `sdk64`/`sdk67` (the release path) |
| REQ-11, REQ-12, ERR-5, REQ-23, INV-14 | `sdk63` (exact pay + SSP pre-pay census), `sdk65` (non-exact pay via a latched in-ladder piece), `unit::ssp::swap_tests::preimage_matches_hash` |
| REQ-42 (one-call pay routes both lanes) | `sdk63` (exact), `sdk65` (non-exact fallback) |
| REQ-43 (failed pay recoverable) | `sdk66` (non-exact rollback: whole parent recovered), `sdk68` (exact reclaim: coin restored as exitable) |
| INV-15, INV-30 | `sdk64` (exact receive), `sdk67` (non-exact receive via a latched piece), `sdk24` (payer paid, SSP aborts), `sdk25` (delayed-claim attacker fails) |
| REQ-15, INV-9 | `sdk01`; `unit::select` (exact/split/insufficient) |
| REQ-17, INV-20, ERR-7 | `unit::terminal_parents_tests` (count binding); `sdk58` (parent-terminality attack REJECT); `sdk29`/`sdk31`/`sdk39` (honest branches accepted). **Gap:** the branch-lane non-terminal-ancestor REJECT E2E (`sdk10`) was retired with no replacement |
| REQ-18, INV-10, INV-11 | `sdk29`/`sdk31` (colored splits/combines); `unit::split_math` |
| REQ-39, ERR-16 (in-ladder split) | `sdk58` (accept + 11 REJECTs), `sdk59` (end-to-end split payment), `sdk12` Part B (value flow), `sdk30` (c) (the `min_child_value` floor in a sponsored rebate) |
| REQ-40, REQ-41 (first-class children) | `sdk60` (alice→bob→carol off-chain, `F` unspent throughout), `sdk17` (multi-hop, partial second hop), `sdk04` (a spent parent is refused) |
| INV-18, INV-19 | `sdk58`/`sdk59` (SP descends from the trigger — [B1]), `rgb03`/`rgb06` (off-chain DAG), `rgb04` |
| REQ-19, REQ-20, INV-12, INV-13 | `sdk09` (IFA issue + mint + batch) |
| REQ-21/INV-13 (multi-carrier combine) | `sdk31` (token combine) |
| REQ-21, REQ-22, ERR-8 | `sdk02`, `sdk09`; `unit::envelope` |
| INV-29 (terminal freeze / carrier ⊥ ladder) | `sdk52` (plain coin laddered, carrier not, RGB transfer still settles), `sdk32` (tokens over time), `sdk39` |
| ERR-9 | `sdk04` (`unit::select` insufficient) |
| ERR-10 | `sdk04` (double-withdraw / split-parent refusal) |
| INV-22 | `sdk01`/`sdk09` (exact-amount splits) |
| REQ-26 | `sdk11`; `unit::identity_tests::sign_validate_roundtrip` |
| REQ-27 | `sdk11` (multi-recipient) — see the divergence note in §13 |
| REQ-28, ERR-11 | `sdk11`; `unit::invoice::tests` (roundtrip, reject) |
| REQ-29, REQ-30 | `sdk11` (query API + fee quote) |
| REQ-31 (refresh / re-anchor) | `sdk30` (a) (idle coin unchanged, then re-anchored) / (c) (sponsored rebate sized to `min_child_value`), `sdk38` (broke sponsor, bounded loss) |
| REQ-32 (auto-refresh in transfer) | **no live test** — `sdk33` was retired with no replacement; see the coverage note in §9.4. Re-anchor itself: `sdk30`; unbounded off-chain renewal: `sdk43` |
| REQ-33 (watchtower carrier materialize) | `sdk34` (received-carrier auto-materialize, clawback defeated) |
| REQ-34 (keyless watch delegation) | `sdk45` (keyless bundle carries zero key material, a 2nd independent tower is idempotent, an offline owner is defended against a hostile trigger), `sdk51` (in-wallet pass), `sdk52` (carriers excluded); `unit::watchtower::tests` |
| REQ-35, ERR-13 (derived slots) | `sdk36` (poisoned-pool split/refresh, onboarding still charges, direct mint, caps, garbage/replayed/non-owner auth); `mercurylib unit::deposit::derived_token_tests` |
| INV-20 (ancestor-count binding), ERR-7 | `unit::terminal_parents_tests`, `sdk58` (laddered-lane equivalent) |
| INV-23, ERR-12 | `sdk12` Part C (nonce-reuse refused) |
| INV-24 | `sdk04` (terminal node stays terminal), `unit::invalidation_model::terminal_predicate_matrix` |
| INV-25 | honest branches accepted: `sdk29`/`sdk31`/`sdk39`. **Gap:** the value-inflating-branch REJECT (`sdk10`) was retired with no replacement; the laddered analogue is `sdk54`/`sdk58` |
| INV-26 | `sdk09` (IFA received amount = fungible only) |
| Concurrency / chaos | `chaos22` (N users act in parallel) |

## 14. Known limitations (adversarial review)

Findings from the adversarial review that are **documented assumptions**, not code changes:

- **Blind-SE ancestor binding.** The SE stores no per-`statechain_id` funding outpoint (it is blind),
  so the receiver cannot cryptographically bind `terminal_parents` ids to specific branch outpoints.
  INV-20's count check defeats omission; full defence against *substitution* of terminal decoys relies
  on the receiver holding the fully-signed branch and being able to exit immediately (win the race for
  the on-chain root). Honest senders always set each node terminal. On the laddered shape this is
  replaced by the census (REQ-38), which binds to the SE's enclave-ATTESTED counter (§3.3) rather
  than to named ids — and the attesting key is held by the same party the receiver is being
  protected from, so what the attestation closes is the gap between what the enclave signed and what
  the client reads, not operator collusion.
- **Batch atomicity.** `transfer_many` / `batch_transfer_tokens` hand off pieces independently; there
  is no all-or-nothing guarantee across recipients. A dropped hand-off leaves that piece reclaimable
  by the sender (the split parent is terminal, so no double-spend), but the batch is not atomic.
  This is the only remaining `transfer_many` caveat: its laddered-parent routing is fixed (REQ-27).
- **Perpetual watching.** A laddered coin's unconditional no-watch window is gone: nothing ages while
  un-broadcast (INV-27), but once a hostile trigger IS broadcast the defence is a race the owner (or
  a tower) must enter within the CSV edge. No theft tx can become valid until at least
  `e_floor + d_floor` blocks after a PUBLICLY visible on-chain trigger — **288** on the mainnet
  schedule (144 + 144, `TesrParams::mainnet`), and every rung's confirmation on top — and nothing
  ever expires to the operator, but the trade is real and deliberate: alarm-driven perpetual
  watching in exchange for zero idle rent (PROTOCOL.md §5.7).
- **Amount width.** Coin sats are booked as `u32` (`utxo.value as u32`, `coin_status.rs`); a single
  coin above ~42.9 BTC would truncate. Out of range for the intended per-coin sizes; not guarded.
- **Mint concurrency.** `mint_tokens` isolates the freshly-minted allocation by a before/after snapshot
  and does NOT hold the wallet lock across its (minutes-long) on-chain confirmation wait, to avoid
  blocking the background claim watcher. A concurrent same-asset receive into the *same* wallet during
  a mint could be misattributed — issuers must not mint and receive the same asset concurrently.
- **Unilateral-exit fees.** An un-laddered exit broadcasts pre-signed fixed-fee branch/backup txs
  with no CPFP/RBF fee-bump; in a fee spike it may confirm slowly. The decrementing-locktime ladder
  (INV-5) still guarantees the latest state wins the race. A laddered exit is better off but not
  immune: each tier is v3/TRUC, carries a committed fee so the base case relays standalone, and
  exposes a 240-sat P2A anchor anyone can attach a live-rate fee child to — so the fee-bump path
  exists, but a tower must be funded to use it.

> **P0 remediation status (2026-07-05 review).** The second adversarial review's six P0 blockers are
> now **FIXED on `feat/spark`** and verifiable in code: the enclave/challenge nonce-reuse crypto break
> (C1 — challenge-binding refuses reuse, `sign.rs`), the two SSP fund-loss bugs (C2/C3 — SSP
> pre-payment recipient/amount gate, `ssp.rs`), the split-locktime exit-race inversion (H5 — branch
> txs are now locktime-free, INV-4), branch-conflict masking (H1 — `reject_non_tree_branch`,
> `transfer_receiver.rs`), token-carrier destruction (H2 — carrier excluded from plain-BTC split,
> `transfer.rs`), and the mnemonic-only-backup durability gap (H3 — recovery bundle).
>
> The caveat this note used to carry — "the **SGX lockbox** must be rebuilt and redeployed for the
> enclave-side single-use secnonce to take effect" — is closed in code, and was mis-stated besides:
> the lockbox is a plain C++ service, not an SGX enclave, and it is the lane that runs. It has had
> the atomic consume since P0-1 (`load_and_consume_secnonce`, `lockbox/src/db_manager.cpp`, called
> from `server.cpp`), and `9cfe48f` applied the same consume to the SGX enclave lane
> (`enclave/App/database/db_manager.cpp`, `statechain/sign.cpp`), which had silently never had it.
> The coordinator-side challenge binding (INV-23 / ERR-12, `server/src/endpoints/sign.rs`) is a
> third, independent stop and needs no enclave rebuild at all. One caveat remains: the **full E2E
> suite (regtest + lockbox + RLN) must be re-run and the result re-reviewed**.
> See [REVIEW.md](REVIEW.md#second-adversarial-review-2026-07-05--full-protocol-production-readiness-pass).

Unit tests live in `clients/libs/rust-sdk/src/*` (`#[cfg(test)]`); E2E dispatch via
`SDK_E2E`/`RGB_E2E` in `clients/tests/rust`; upstream Mercury suite runs by default.
