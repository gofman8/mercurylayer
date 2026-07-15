# V2DEF-5/6 — migrate to V2-default + remove V1 (non-LN), keep V1 as the LN lane

Chosen direction (after the 6-round LN-latch verdict: SSP-mediated LN swap on V2 needs adaptor signatures,
a dedicated future effort — see `V2-LATCH-FIX.md`). This plan removes V1 for **every non-LN flow** now,
keeping V1 **solely** as the LN-swap lane behind the sdk53 guard.

## The product reality this rests on
`deposit_protocol_version` is a **per-`SdkConfig`** field. With the default flipped to **2**, new deposits
are V2 (laddered). But an LN swap of a V2 coin is refused by the sdk53 guard, and a V2 coin cannot be
downgraded (its tier co-signs inflate `num_sigs`). Therefore, **a wallet/coin destined for an LN swap must
be created V1** (`deposit_protocol_version = 1`). This is an accepted interim UX until adaptor-sig LN
(V2-LATCH-FIX §verdict) retires the last lane. It must be documented in the LN guide.

## Blast radius (measured)
- 42 tests construct `UtexoWallet` (affected by the default).
- 12 of those are LN-swap tests (`start_lightning_swap` / `execute_pay` / `send_payment`) → **pin to V1**.
- ~30 non-LN SDK tests → migrate to V2-aware assertions (num_sigs, backup counts, ladder presence).
- mercuryrustlib-level tests (sdk01/07, most rgb*) do NOT read `deposit_protocol_version` (they use the
  raw lib, not `UtexoWallet::claim()`), so they are unaffected by the flip — they migrate only in V2DEF-6
  when V1 lib code they exercise is deleted.

## V2DEF-5 — flip the default + migrate the suite (incremental, each batch validated green)
1. **Pin the 12 LN tests to V1 first** (`cfg.deposit_protocol_version = 1`). No-op today (default is 1),
   protective once the default flips — so the flip never breaks LN. Commit; run 1-2 LN tests to confirm.
2. **Flip the default** `deposit_protocol_default()` `1 → 2` (`config.rs:67`). Commit.
3. **Migrate the ~30 non-LN SDK tests** in batches (token, onboarding, granularity, invalidation, refresh,
   combine, derived-token, web-wallet): update V1-specific assertions to their V2 equivalents (a V2 coin
   has a ladder + higher num_sigs). Each batch: run green before the next.
4. **LN guide doc**: state that LN swaps require a V1 coin in this interim.
5. Re-run sdk40–53 (V2 suite) + the pinned LN suite → all green.

## V2DEF-6 — delete V1 non-LN code (LAST, only after V2DEF-5 fully green)
Delete only V1 code that V2 fully replaces AND that the LN lane does not use:
- V1 exit ladder / decrementing-locktime backup path for **non-carrier, non-LN** coins (V2 uses the TES-R
  ladder + reconcile; exit routing already V2 via sdk50).
- V1 `validate_signature_scheme` decrementing-ladder receiver check for non-LN transfers (already gated
  behind `protocol_version < 2`; the LN lane still needs the V1 path, so keep it reachable for
  `batch_id.is_some()` transfers).
- Purge the OLD design from docs (V1 refresh-as-rent narrative etc.), keeping "protocol version 2" naming.
**KEEP** (the LN lane): V1 deposit, V1 latched transfer (`transfer_sender::execute` with `batch_id`), the
sdk53 guard, `create_backup_tx_to_receiver`, the V1 receiver path for latched transfers. These retire only
when adaptor-sig LN lands.

## Empirical findings (from migrating sdk02 ✅ + probing sdk16)
Per-test migration divides into two kinds, learned by running under V2 on the live stack:
1. **Assertion tweak (trivial)** — e.g. sdk02 (DONE, green): a token carrier's sats are excluded from
   `available_sats` (carrier ⊥ ladder), so any `available_sats >= N` "carrier confirmed" check must
   become a settled-allocation / coin-status check. One-line change.
2. **Needs a V2-safe carrier-confirmed helper (non-trivial)** — e.g. sdk16: the engine token balance
   settles BEFORE the statechain carrier COIN confirms, and the old proxy `available_sats >= pre+10_000`
   (the carrier's sats appearing) no longer works under V2. Dropping it breaks the wait → the next
   `transfer_tokens` finds "0 carriers". **Reusable fix:** a helper that polls the RAW coin list for a
   CONFIRMED non-dup coin of the carrier's sat amount — exactly what sdk52 does
   (`get_wallet(...).coins.iter().find(|c| c.status==CONFIRMED && c.amount==Some(carrier_amt))`). This
   unblocks the whole token-test class (sdk02/16/29/31/32/34/36/39). Build it once in `utils`, reuse.
3. Some tests also exercise V2 **split + Model A + ladder exit** (sdk16's `transfer`+`unilateral_exit`)
   — paths beyond sdk49's direct transfer; migrating them doubles as validation that V2 split-transfer
   works, and may surface real gaps to fix (not just test edits).

Recommended order for the token/RGB class: land the shared `wait_carrier_coin_confirmed` helper first,
then migrate sdk02/16/29/31/32/34/36/39 using it; then the sats/adversarial class; then flip the default.

## Sequencing note
This is a ~42-test migration + a careful V1 deletion — substantial and best done in validated batches
against the live stack, not one big push. Each step above is independently committable and green-gated.
Status: LN tests pinned V1 (done); sdk02 migrated V2 (done); ~27 non-LN tests + the default flip remain.
