use chrono::{DateTime, Utc};
use secp256k1_zkp::PublicKey;

use sqlx::Row;

/// The re-address guard's query, as one named constant so the shape below can be asserted on
/// without a database.
///
/// `$3` is bound from `Option<String>`: **NULL on every non-batched transfer.** That is why the
/// batch comparison cannot be spelled `batch_id = $3` — under SQL three-valued logic `NULL = NULL`
/// is UNKNOWN, never TRUE, so a `=` here makes the whole conjunction UNKNOWN for exactly the rows a
/// non-batched open needs to be refused by, `COUNT(*)` comes back 0 unconditionally, and the guard
/// is dead code. `IS NOT DISTINCT FROM` is the NULL-aware equality: it is TRUE when both sides are
/// NULL and behaves as `=` otherwise, so the batched path is unchanged.
///
/// `cancelled_at IS NULL`: a CANCELLED transfer must not keep reporting "message already exists"
/// and block the sender from opening a fresh one. Reusing the same recipient key after a
/// cancellation is refused separately and by name (transfer_cancel::recipient_key_was_cancelled),
/// so relaxing this check does not admit a stale-x1 reopen.
pub const EXISTS_MSG_FOR_SAME_SID_AND_KEY_SQL: &str = "\
        SELECT COUNT(*) \
        FROM statechain_transfer \
        WHERE new_user_auth_public_key = $1 \
        AND statechain_id = $2 AND batch_id IS NOT DISTINCT FROM $3 \
        AND cancelled_at IS NULL";

pub async fn exists_msg_for_same_statechain_id_and_new_user_auth_key(pool: &sqlx::PgPool, new_user_auth_key: &PublicKey, statechain_id: &str, batch_id: &Option<String>) -> bool {

    let query = EXISTS_MSG_FOR_SAME_SID_AND_KEY_SQL.to_string();

    let serialized_new_user_auth_key = new_user_auth_key.serialize();

    let row = sqlx::query(&query)
        .bind(&serialized_new_user_auth_key)
        .bind(statechain_id)
        .bind(batch_id)
        .fetch_one(pool)
        .await
        .unwrap();

    let count: i64 = row.get(0);

    count > 0
}

pub async fn get_batch_time_by_batch_id(pool: &sqlx::PgPool, batch_id: &str) -> Option<DateTime<Utc>> {

    let query = "\
        SELECT batch_time \
        FROM statechain_transfer \
        WHERE batch_id = $1
        AND locked = true";

    let row = sqlx::query(query)
        .bind(batch_id)
        .fetch_optional(pool)
        .await
        .unwrap();

    match row {
        Some(row) => {
            let batch_time: DateTime<Utc> = row.get(0);
            Some(batch_time)
        }
        None => None
    }
}


/// [pending-transfer lock, CHILDREN.md] True if `statechain_id` has an OPEN transfer — a row
/// not yet completed (`key_updated = false`) and not yet expired (opened within the last hour). While a
/// transfer is open the SE refuses any co-sign of the coin (the sender's legitimate pre-signs all happen
/// BEFORE `get_new_x1` opens the transfer — see transfer_sender.rs re-order), so a still-owner sender
/// cannot co-sign a lower-CSV rival that would out-race the receiver's conveyed state. Expiry only affects
/// the never-claimed case (no victim: an unclaimed payment was never accepted). Returns Result so the
/// sign gate can FAIL CLOSED on a DB error, exactly like the single-use/budget/epoch gates.
///
/// A CANCELLED transfer (`cancelled_at IS NOT NULL`, migration 0010) is not open: cancellation is
/// exactly "release this lock now instead of at the expiry". What makes that safe is the
/// AUTHORIZATION the cancel endpoint demands before stamping `cancelled_at` — sender alone only
/// while nothing has been conveyed, otherwise the recorded recipient must co-sign
/// (`mercurylib::transfer::cancel`). By the time the flag is set, the only party who could be
/// defrauded has signed the release.
/// The openness predicate, shared verbatim by both halves of the lock so they cannot drift apart.
/// `{extra}` is where `has_open_transfer_to_other_auth` splices its key-inequality; everything that
/// decides WHEN the lock releases lives here and nowhere else.
///
/// Extracted to a constant so `open_window_invariant_tests` can assert on it. A behavioural test is
/// not available — there is no test database, no sqlx offline fixture and no embedded Postgres in
/// this repo, so this query is only ever executed against the orchestrator's live stack. The tests
/// below therefore model the window arithmetic in Rust and check the SQL carries the shape that
/// model describes. That is weaker than executing it, and it is labelled as such rather than dressed
/// up.
pub const OPEN_TRANSFER_WINDOW_SQL: &str = "\
    CASE WHEN batch_id IS NULL \
         THEN updated_at > NOW() - INTERVAL '1 hour' \
         ELSE COALESCE( \
                (SELECT MAX(l.expires_at) FROM lightning_latch l \
                  WHERE l.batch_id = statechain_transfer.batch_id), \
                batch_time + make_interval(secs => $TIMEOUT::int) \
              ) > NOW() \
    END";

pub async fn has_open_transfer(pool: &sqlx::PgPool, statechain_id: &str, batch_timeout_secs: i64) -> Result<bool, sqlx::Error> {
    // THE INVARIANT THIS QUERY MUST SATISFY: **no authorization expires before the latest claim it
    // protects.** This lock is the only thing stopping a still-owner sender from co-signing a rival
    // state while a conveyed-but-unclaimed receiver holds claimable material; the moment it lapses
    // early, the sender can inflate the enclave's `signature_count` and the receiver's exact-equality
    // census fails forever. The receiver keeps the money it paid for; the payer keeps the coin.
    //
    // THIS QUERY PREVIOUSLY VIOLATED THAT INVARIANT TWICE OVER, and the comment that stood here
    // asserted the opposite in good faith — it was true when written and was falsified by the H4 fix
    // that re-keyed the CLAIM gate:
    //
    //   1. the batch branch released at `batch_time + batch_timeout` (deployed at **20 s**), while
    //      `validate_batch` (server/src/endpoints/transfer_receiver.rs) lets an LN-latch receiver
    //      claim until `lightning_latch.expires_at` — 3 000 s classic, **90 000 s (25 h)** for the
    //      external-hash latch every pay-invoice swap uses;
    //   2. and `updated_at > NOW() - INTERVAL '1 hour'` was ANDed ACROSS BOTH branches, so even
    //      re-keying the batch branch would still have dropped the lock at 1 h against a 25 h claim
    //      window. The cap had to stop applying to latch batches, not just move.
    //
    // So the shape is now a CASE, not a conjunction: a non-batch transfer keeps exactly its old
    // 1-hour rule; a batch transfer is open for as long as its receiver can still claim.
    //
    // WHICH CLOCK, AND WHY IT IS THE CONSERVATIVE ONE. `validate_batch` closes the claim window a
    // grace period BEFORE `expires_at` (so the SSP always has time to settle the HTLC after the last
    // possible claim). We deliberately do NOT subtract that grace here: the lock must outlive the
    // claim window, never the reverse. `MAX` rather than `LIMIT 1` for the same reason — a batch_id
    // spans one latch row per statechain_id, and holding until the LAST of them is the safe
    // direction. `COALESCE` falls back to the old `batch_time + batch_timeout` for a plain
    // (non-Lightning) batch, which has no latch row: unchanged behaviour where there is no latch.
    //
    // Fail-closed on the read is the caller's job (this returns Result and the sign gates propagate),
    // which is why an absent latch row is distinguished from an unreadable one by COALESCE over a
    // subquery rather than by swallowing an error.
    // Built FROM the constant, not copied from it — otherwise the tests below would pin a string
    // nothing executes, and the two could drift apart silently while every test stayed green.
    let query = format!(
        "SELECT EXISTS(SELECT 1 FROM statechain_transfer \
         WHERE statechain_id = $1 AND key_updated = false \
         AND cancelled_at IS NULL \
         AND {})",
        OPEN_TRANSFER_WINDOW_SQL.replace("$TIMEOUT", "$2")
    );
    let row = sqlx::query(&query)
    .bind(statechain_id)
    .bind(batch_timeout_secs)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<bool, _>(0))
}

/// [re-address guard] True if `statechain_id` already has an OPEN transfer addressed to a DIFFERENT
/// receiver auth key than `new_user_auth`. `get_new_x1` rejects on this so a still-owner sender cannot
/// re-address an already-conveyed (victim-accepted) transfer to an attacker key it controls — the exact
/// self-reopen re-address vector the child-firstclass review found. A same-auth retry is allowed
/// (idempotent, before the message is posted).
pub async fn has_open_transfer_to_other_auth(
    pool: &sqlx::PgPool,
    statechain_id: &str,
    new_user_auth: &PublicKey,
    batch_timeout_secs: i64,
) -> Result<bool, sqlx::Error> {
    // Same openness rule as `has_open_transfer`, and it MUST stay the same one — these two are the
    // sender-side and re-address-side halves of a single lock, so a window that closes early in one
    // reopens the same hole. See the invariant written out there: no authorization expires before
    // the latest claim it protects. A latch batch is open until its receiver can no longer claim
    // (`lightning_latch.expires_at`, batch-wide MAX, grace NOT subtracted); a plain batch falls back
    // to `batch_time + batch_timeout`; a non-batch transfer keeps its 1-hour rule.
    // Same constant, same reason: one definition of WHEN the lock releases, spliced into both halves.
    let query = format!(
        "SELECT EXISTS(SELECT 1 FROM statechain_transfer \
         WHERE statechain_id = $1 AND key_updated = false \
         AND cancelled_at IS NULL \
         AND {} \
         AND new_user_auth_public_key <> $2)",
        OPEN_TRANSFER_WINDOW_SQL.replace("$TIMEOUT", "$3")
    );
    let row = sqlx::query(&query)
    .bind(statechain_id)
    .bind(new_user_auth.serialize())
    .bind(batch_timeout_secs)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<bool, _>(0))
}

pub async fn insert_new_transfer(
    pool: &sqlx::PgPool,
    new_user_auth_key: &PublicKey, x1: &[u8; 32],
    statechain_id: &String,
    batch_id: &Option<String>)
{

    // [OPENER KEY — migration 0011] The coin's auth key AS IT IS RIGHT NOW, i.e. the key of whoever
    // is opening this transfer. `POST /transfer/sender` has already verified a signature under it,
    // so this is a record of a party that authenticated, not an assertion by the caller.
    //
    // Why it is recorded: claiming ROTATES the coin's auth key to the recipient's
    // (`transfer_receiver::update_statechain`), which used to leave the sender unable to authenticate
    // to `POST /transfer/cancel` about its own transfer — so the `AlreadyClaimed` refusal could never
    // fire and the sender got a generic auth failure instead. `transfer_cancel` verifies the sender
    // leg against THIS key. See migration 0011 for why that cannot loosen anything: the coin's key
    // and `key_updated` move in one transaction, so this key differs from the live one only in states
    // where every possible decision REFUSES.
    //
    // The lookup is spelled out rather than swallowed with `.ok()`, because the direction it fails
    // in has to be legible: `POST /transfer/sender` has ALREADY verified a signature under this key
    // before reaching here, so an `Err` is a transient pool fault, not a missing coin. Recording NULL
    // for it costs no protection — the cancel endpoint then falls back to checking the sender against
    // the coin's LIVE key, which is precisely the pre-0011 check. The failure direction is "the old,
    // stricter path", never "no path".
    let opener_auth_key_result =
        crate::endpoints::utils::get_auth_key_by_statechain_id(&pool, &statechain_id).await;
    let opener_auth_key = match opener_auth_key_result {
        Ok(key) => Some(key),
        // The direction is fail-SAFE (a NULL opener sends the cancel endpoint back to the coin's
        // live key, i.e. the stricter pre-0011 check), but it must not be fail-SILENT: this arm is
        // very nearly unreachable — `pool.begin()` two lines below `.unwrap()`s, so a genuine pool
        // fault panics rather than arriving here — which means reaching it at all is a fact worth
        // seeing in the log rather than a routine outcome to shrug at.
        Err(e) => {
            log::warn!(
                "insert_new_transfer({statechain_id}): could not read the opening auth key, so no \
                 opener is recorded for this transfer. Cancellation of it will fall back to the \
                 coin's LIVE key (the stricter, pre-0011 check), so nothing is weakened — but a \
                 sender asking about this transfer after it is claimed will get the generic \
                 signature refusal instead of AlreadyClaimed: {e}"
            );
            None
        }
    };

    let mut transaction = pool.begin().await.unwrap();

    let query1 = "DELETE FROM statechain_transfer WHERE statechain_id = $1";

    let _ = sqlx::query(query1)
        .bind(statechain_id)
        .execute(&mut *transaction)
        .await
        .unwrap();

    // The two INSERTs differ only in the batch columns; both now carry
    // `sender_auth_xonly_public_key` (migration 0011) in position $3, immediately after the
    // recipient key, so the two keys of a transfer sit together and a reader cannot mistake one for
    // the other. Written out as constants rather than inline strings because the bind ORDER below is
    // shared between them and a positional drift is silent — `$3` would simply bind the wrong bytea.
    const INSERT_TRANSFER_SQL: &str = "\
        INSERT INTO statechain_transfer \
            (statechain_id, new_user_auth_public_key, sender_auth_xonly_public_key, x1, \
             locked, locked2) \
        VALUES ($1, $2, $3, $4, $5, $6)";

    const INSERT_TRANSFER_BATCHED_SQL: &str = "\
        INSERT INTO statechain_transfer \
            (statechain_id, new_user_auth_public_key, sender_auth_xonly_public_key, x1, \
             batch_id, batch_time, locked, locked2) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)";

    let query2 = if batch_id.is_none() {
        INSERT_TRANSFER_SQL
    } else {
        INSERT_TRANSFER_BATCHED_SQL
    };

    let ser_new_user_auth_key = new_user_auth_key.serialize();

    let mut ps_query = sqlx::query(query2)
        .bind(statechain_id)
        .bind(ser_new_user_auth_key)
        .bind(opener_auth_key.map(|k| k.serialize().to_vec()))
        .bind(x1);

    if batch_id.is_some() {

        let batch_id = batch_id.clone().unwrap();

        let mut batch_time = get_batch_time_by_batch_id(pool, &batch_id).await;// Utc::now();

        if batch_time.is_none() {
            batch_time = Some(Utc::now());
        }

        let sender_auth_key = crate::endpoints::utils::get_auth_key_by_statechain_id(&pool, &statechain_id).await.unwrap();
        let is_lightning_latch = crate::database::lightning_latch::is_lightning_latch(pool, statechain_id, &sender_auth_key, &batch_id).await;

        ps_query = ps_query
            .bind(batch_id)
            .bind(batch_time.unwrap())
            .bind(true)
            .bind(is_lightning_latch);
    } else {
        ps_query = ps_query
            .bind(false)
            .bind(false);
    }

    ps_query.execute(&mut *transaction)
        .await
        .unwrap();    

    transaction.commit().await.unwrap();
}

pub async fn update_transfer_msg(pool: &sqlx::PgPool, new_user_auth_key: &PublicKey, enc_transfer_msg: &Vec<u8>, statechain_id: &str)  {

    let query = "\
        UPDATE statechain_transfer \
        SET encrypted_transfer_msg = $1, updated_at = NOW() \
        WHERE \
            statechain_id = $2 AND \
            new_user_auth_public_key = $3 AND \
            updated_at = (SELECT MAX(updated_at) FROM statechain_transfer WHERE statechain_id = $2)";

    let _ = sqlx::query(query)
        .bind(enc_transfer_msg)
        .bind(statechain_id)
        .bind(&new_user_auth_key.serialize())
        .execute(pool)
        .await
        .unwrap();
}
// ==================================================================================================
// THE RE-ADDRESS GUARD, under SQL three-valued logic.
//
// WHY THESE TESTS ARE SHAPED LIKE THIS. `exists_msg_for_same_statechain_id_and_new_user_auth_key`
// takes a live `sqlx::PgPool`; this repository has no test database, no `sqlx` offline fixture and
// no embedded Postgres, so the function cannot be CALLED from a unit test. The only place the query
// really executes is the orchestrator's live stack.
//
// So these tests do the next strongest thing, and it is stated plainly rather than dressed up: they
// read the PRODUCTION query text, extract the operator it actually uses for the `batch_id`
// comparison, and evaluate the guard's WHERE clause under a model of SQL's three-valued logic. The
// model is not a paraphrase of the query — its decisive input IS the query, so it cannot silently
// drift from what Postgres will run, and an operator the model does not know makes the test PANIC
// rather than pass.
//
// What is still NOT covered here, and must not be claimed: that Postgres binds a Rust `None` as SQL
// NULL (it does, and `insert_new_transfer`'s own `batch_id.is_none()` branch is the codebase's
// evidence that a non-batched transfer stores NULL), and that the endpoint reaches this function
// with the payload's `batch_id`.
// ==================================================================================================
#[cfg(test)]
mod exists_msg_guard_tests {
    use super::EXISTS_MSG_FOR_SAME_SID_AND_KEY_SQL as SQL;

    /// SQL's three-valued logic. `Unknown` is the whole bug: a WHERE clause admits a row only on
    /// `True`, so `Unknown` and `False` are indistinguishable from the caller's side — which is
    /// precisely why a guard that silently degrades to `Unknown` looks like a guard that simply
    /// never has anything to complain about.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Sql3 {
        True,
        False,
        Unknown,
    }

    impl Sql3 {
        /// `AND` over three-valued logic.
        fn and(self, other: Sql3) -> Sql3 {
            match (self, other) {
                (Sql3::False, _) | (_, Sql3::False) => Sql3::False,
                (Sql3::Unknown, _) | (_, Sql3::Unknown) => Sql3::Unknown,
                _ => Sql3::True,
            }
        }

        /// A WHERE clause selects a row on `True` alone.
        fn selects(self) -> bool {
            self == Sql3::True
        }
    }

    /// `a = b`. NULL on either side poisons the comparison to UNKNOWN — including `NULL = NULL`.
    fn sql_equals(a: Option<&str>, b: Option<&str>) -> Sql3 {
        match (a, b) {
            (Some(a), Some(b)) if a == b => Sql3::True,
            (Some(_), Some(_)) => Sql3::False,
            _ => Sql3::Unknown,
        }
    }

    /// `a IS NOT DISTINCT FROM b`. NULL-aware equality: two NULLs ARE not distinct, so this is TRUE.
    /// Never UNKNOWN.
    fn sql_is_not_distinct_from(a: Option<&str>, b: Option<&str>) -> Sql3 {
        if a == b {
            Sql3::True
        } else {
            Sql3::False
        }
    }

    /// The operator the PRODUCTION query uses between `batch_id` and `$3`, lifted out of the query
    /// text itself. This is what stops the model below from being an independent restatement that
    /// could agree with a broken query.
    fn batch_id_operator() -> String {
        let after = SQL.split("batch_id").nth(1).expect("the guard must compare batch_id at all");
        let (op, _) = after.split_once("$3").expect("the batch comparison must be bound to $3");
        op.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Evaluate the guard's WHERE clause for one candidate row. Every conjunct is modelled, because
    /// the vacuous one only matters in the presence of the others: on its own it would just be a
    /// count of nothing.
    ///
    /// * `row_batch` / `param_batch` — the row's `batch_id` column and the bound `$3`.
    /// * the key and statechain id are modelled as MATCHING, which is the situation the guard exists
    ///   to refuse; a non-matching row is uninteresting.
    /// * `cancelled` — a cancelled row is deliberately excluded, and must stay excluded.
    fn guard_fires(row_batch: Option<&str>, param_batch: Option<&str>, cancelled: bool) -> bool {
        let batch = match batch_id_operator().as_str() {
            "=" => sql_equals(row_batch, param_batch),
            "IS NOT DISTINCT FROM" => sql_is_not_distinct_from(row_batch, param_batch),
            // Refusing to guess is the point: a rewrite that introduces an operator this model does
            // not understand must extend the model, not slip past it.
            other => panic!(
                "the guard compares batch_id with an operator this three-valued-logic model does \
                 not know: {other:?}. Teach the model before changing the query."
            ),
        };

        // new_user_auth_public_key = $1 AND statechain_id = $2: both non-NULL on both sides here.
        let key = Sql3::True;
        let sid = Sql3::True;
        // cancelled_at IS NULL — an IS test, so never UNKNOWN.
        let not_cancelled = if cancelled { Sql3::False } else { Sql3::True };

        key.and(sid).and(batch).and(not_cancelled).selects()
    }

    /// **DEFECT A.** A non-batched transfer binds `$3` as SQL NULL. The guard must still refuse a
    /// second `POST /transfer/sender` for the same (coin, recipient key).
    ///
    /// This is not a hypothetical requirement. `clients/libs/rust/src/tesr.rs` builds its whole
    /// crash-resume journal on it — "`transfer/sender` is **once per `(statechain_id,
    /// new_user_auth_key)`** — a second call is refused ('Transfer message already exists')" — and
    /// `is_transfer_already_open` matches that refusal text to decide whether a conveyance leg is
    /// STRANDED or retried. With the guard vacuous, that second call SUCCEEDS and mints a fresh
    /// `x1`, silently superseding conveyed material the recipient already holds. The endpoint even
    /// carries a non-batched variant of the refusal message
    /// ("Transfer message already exists for this statechain_id and new_user_auth_key.") that is
    /// currently unreachable.
    #[test]
    fn the_guard_fires_on_the_non_batched_path() {
        assert!(
            guard_fires(None, None, false),
            "a non-batched row and a non-batched request both carry batch_id NULL. With `batch_id = \
             $3` that conjunct is UNKNOWN, the WHERE never selects, COUNT(*) is unconditionally 0, \
             and the guard is dead code — a sender may re-open a conveyed transfer to the same \
             recipient key as often as it likes, each time minting a fresh x1. Operator in the \
             production query: {:?}",
            batch_id_operator()
        );
    }

    /// The batched path must be UNCHANGED by the fix: it works today and nothing here may loosen it.
    #[test]
    fn the_batched_path_keeps_its_existing_behaviour() {
        assert!(guard_fires(Some("batch-1"), Some("batch-1"), false), "same batch still refuses");
        assert!(!guard_fires(Some("batch-1"), Some("batch-2"), false), "a different batch is a different transfer");
    }

    /// A NULL on exactly one side is a genuine MISMATCH — a batched row against a non-batched
    /// request, or the reverse — and must NOT be treated as a match. `IS NOT DISTINCT FROM` is
    /// NULL-aware equality, not "NULL matches anything".
    #[test]
    fn one_sided_null_is_a_mismatch_not_a_wildcard() {
        assert!(!guard_fires(Some("batch-1"), None, false), "a batched row does not block a plain open");
        assert!(!guard_fires(None, Some("batch-1"), false), "a plain row does not block a batch join");
    }

    /// The cancelled-row exclusion survives: a cancelled transfer must stop reporting "already
    /// exists", otherwise a cancellation would permanently wedge the coin instead of releasing it.
    #[test]
    fn a_cancelled_row_still_does_not_block_a_fresh_open() {
        assert!(!guard_fires(None, None, true));
        assert!(!guard_fires(Some("batch-1"), Some("batch-1"), true));
    }

    /// The literal text, pinned. The model above is only as good as its one real input, so state the
    /// conclusion directly too: `batch_id = $n` against a parameter bound from `Option` is the
    /// defect shape, and it must not come back.
    #[test]
    fn the_query_text_uses_null_aware_equality_for_the_bound_option() {
        assert_eq!(
            batch_id_operator(),
            "IS NOT DISTINCT FROM",
            "$3 is bound from Option<String> and is NULL for every non-batched transfer; `=` \
             silently disables the guard rather than failing it"
        );
        assert!(
            !SQL.contains("batch_id = $"),
            "the vacuous comparison is back in the query: {SQL}"
        );
    }
}

/// **THE WINDOW INVARIANT: no authorization expires before the latest claim it protects.**
///
/// The pending-transfer lock is the only thing stopping a still-owner sender from co-signing a rival
/// state while a conveyed-but-unclaimed receiver holds claimable material. If it lapses before the
/// receiver's claim window closes, the sender can inflate the enclave's `signature_count` and the
/// receiver's EXACT-EQUALITY census fails permanently — the receiver keeps what it paid, the payer
/// keeps the coin. That is theft, and it needs no race finesse: any co-signature at all will do.
///
/// This class has now produced four instances in this codebase. These tests model the two clocks in
/// Rust and assert the relation, so the fifth is caught at authorship rather than by an auditor.
#[cfg(test)]
mod open_window_invariant_tests {
    use super::OPEN_TRANSFER_WINDOW_SQL as SQL;

    /// Seconds after `batch_time` at which each side stops acting, for one shape.
    struct Windows {
        /// When the SENDER-side lock releases (this query).
        lock_closes_at: f64,
        /// When the RECEIVER can no longer claim (`validate_batch`,
        /// server/src/endpoints/transfer_receiver.rs).
        claim_closes_at: f64,
    }

    impl Windows {
        /// The invariant. `>=` and not `>`: closing together is safe, closing early is not.
        fn holds(&self) -> bool {
            self.lock_closes_at >= self.claim_closes_at
        }
    }

    /// Deployed values. `BATCH_TIMEOUT: 20` in docker-compose-lockbox.yml (config default 120);
    /// classic latch 3 000 s (lightning_latch.rs); external-hash latch 90 000 s — the one every
    /// pay-invoice swap uses. `RECEIVE_LATCH_GRACE` defaults to 300 s and the CLAIM side closes that
    /// much EARLY, so the claim window is always the shorter of the two by construction.
    const BATCH_TIMEOUT: f64 = 20.0;
    const CLASSIC_LATCH: f64 = 3_000.0;
    const EXTERNAL_LATCH: f64 = 90_000.0;
    const GRACE: f64 = 300.0;

    /// What the query does TODAY, per shape.
    fn fixed(latch_expiry: Option<f64>) -> Windows {
        Windows {
            // The batch branch is no longer ANDed with the 1-hour cap, and it does not subtract the
            // grace period — the lock deliberately outlives the claim window.
            lock_closes_at: latch_expiry.unwrap_or(BATCH_TIMEOUT),
            claim_closes_at: latch_expiry.map_or(BATCH_TIMEOUT, |e| e - GRACE),
        }
    }

    /// What it did BEFORE, so these tests fail if anyone reinstates it.
    fn regressed(latch_expiry: Option<f64>) -> Windows {
        Windows {
            // `batch_time + batch_timeout`, further capped at 1 h by an ANDed `updated_at` clause.
            lock_closes_at: BATCH_TIMEOUT.min(3_600.0),
            claim_closes_at: latch_expiry.map_or(BATCH_TIMEOUT, |e| e - GRACE),
        }
    }

    #[test]
    fn the_lock_outlives_the_claim_window_for_every_latch_shape() {
        for shape in [None, Some(CLASSIC_LATCH), Some(EXTERNAL_LATCH)] {
            assert!(
                fixed(shape).holds(),
                "the lock must not release before the receiver's last possible claim (latch {shape:?})"
            );
        }
    }

    #[test]
    fn the_old_shape_is_pinned_as_violating_it_so_a_revert_cannot_pass() {
        // A non-latch batch was always fine — both sides used batch_timeout. That is why the defect
        // survived review: the shape it breaks is exactly the one the H4 fix introduced.
        assert!(regressed(None).holds(), "the plain-batch case was never the bug");

        for shape in [CLASSIC_LATCH, EXTERNAL_LATCH] {
            let w = regressed(Some(shape));
            assert!(
                !w.holds(),
                "the pre-fix query MUST fail this invariant for a {shape}s latch — if this passes, \
                 the model is not reproducing the defect and the other test proves nothing"
            );
        }

        // The external latch is the one every pay-invoice swap uses: the SSP pays irreversibly, then
        // has 25 h of exposure behind a 20 s lock.
        let w = regressed(Some(EXTERNAL_LATCH));
        assert_eq!(w.lock_closes_at, 20.0);
        assert_eq!(w.claim_closes_at, 89_700.0);
    }

    #[test]
    fn the_sql_carries_the_shape_the_model_describes() {
        // The batch branch must key on the latch, not on batch_timeout alone...
        assert!(SQL.contains("MAX(l.expires_at)"), "batch branch must key on the latch expiry");
        // ...must fall back to batch_time + timeout when there is no latch row...
        assert!(SQL.contains("batch_time + make_interval"), "plain batches keep their old rule");
        // ...must NOT subtract the receiver's grace period (the lock outlives the claim window)...
        assert!(!SQL.contains("RECEIVE_LATCH_GRACE"), "grace belongs to the claim side only");
        // ...and the 1-hour cap must apply ONLY to the non-batch branch. This is the half the
        // original fix proposal missed: re-keying the batch branch while leaving `updated_at` ANDed
        // across both would still have dropped the lock at 1 h against a 25 h claim window.
        let one_hour = SQL.find("INTERVAL '1 hour'").expect("the non-batch rule is still there");
        let else_arm = SQL.find("ELSE").expect("the CASE has an ELSE arm");
        assert!(
            one_hour < else_arm,
            "the 1-hour cap must sit inside the THEN (non-batch) arm, never across both"
        );
    }
}
