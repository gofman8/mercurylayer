use mercurylib::transfer::receiver::StatechainInfo;
use secp256k1_zkp::{PublicKey, Secp256k1, XOnlyPublicKey, SecretKey};

use sqlx::Row;

pub async fn get_statechain_info(pool: &sqlx::PgPool, statechain_id: &str) -> Vec::<StatechainInfo> {

    let mut result = Vec::<StatechainInfo>::new();

    // Only FINALISED sessions belong in receiver-verification info. A row whose `challenge` is NULL is a
    // dangling session — sign/first ran but sign/second has not — so it carries no finalised signature
    // and no conveyed backup corresponds to it. Excluding it (a) matches count_finalized_signatures'
    // `partial_sig_issued = true` semantics and (b) removes a griefing DoS: a `row.get::<String>()` on a
    // NULL `challenge` panics the whole request worker, so anyone (or any interrupted honest flow) that
    // left a dangling sign/first would make /info/statechain un-fetchable and the coin unverifiable.
    let query = "\
        SELECT statechain_id, server_pubnonce, challenge, tx_n \
        FROM statechain_signature_data \
        WHERE statechain_id = $1 AND challenge IS NOT NULL \
        ORDER BY created_at ASC";

    let rows = match sqlx::query(query).bind(statechain_id).fetch_all(pool).await {
        Ok(rows) => rows,
        // Fail closed: a DB error here must not panic the worker. An empty info set makes the receiver
        // refuse the coin (safe) rather than crash the endpoint for every caller.
        Err(_) => return result,
    };

    for row in rows {
        // Defensive second line against the panic: even with the SQL filter, read `challenge` as nullable
        // and skip any NULL rather than `get`-panicking.
        let challenge: String = match row.try_get(2) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let statechain_id: String = row.get(0);
        let server_pubnonce: String = row.get(1);
        let tx_n: i32 = row.get(3);

        let statechain_transfer = StatechainInfo {
            statechain_id,
            server_pubnonce,
            challenge,
            tx_n: tx_n as u32,
        };

        result.push(statechain_transfer);
    }

    result.sort_by(|a, b| a.tx_n.cmp(&b.tx_n));

    result
}

pub async fn get_enclave_pubkey(pool: &sqlx::PgPool, statechain_id: &str) -> Option<PublicKey> {

    let query = "SELECT server_public_key \
        FROM statechain_data \
        WHERE statechain_id = $1";

    let row = sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
        .unwrap();

    if row.is_none() {
        return None;
    }

    let row = row.unwrap();

    let enclave_public_key_bytes = row.get::<Vec<u8>, _>("server_public_key");
    let enclave_public_key = PublicKey::from_slice(&enclave_public_key_bytes).unwrap();

    Some(enclave_public_key)
}

/// [FATAL-B] The authoritative aggregate x-only key the server recorded for this coin at deposit, as
/// hex. `None` for coins deposited before the owner-share binding (the column is NULL). Fail-safe: any
/// DB/read error returns None, so the receiver falls back to the legacy path rather than the endpoint
/// panicking.
pub async fn get_aggregate_pubkey(pool: &sqlx::PgPool, statechain_id: &str) -> Option<String> {
    let query = "SELECT aggregate_xonly FROM statechain_data WHERE statechain_id = $1";

    let row = match sqlx::query(query).bind(statechain_id).fetch_optional(pool).await {
        Ok(Some(r)) => r,
        _ => return None,
    };

    let bytes: Option<Vec<u8>> = row.try_get::<Option<Vec<u8>>, _>("aggregate_xonly").ok().flatten();
    bytes.map(hex::encode)
}

/// The x1 public point for an OPEN transfer of this coin, or None.
///
/// `cancelled_at IS NULL`: a cancelled transfer has no x1 to advertise — `apply_cancel` NULLs the
/// column outright, so a stale `x1_pub` in `/info/statechain` would be both wrong and a hint that a
/// dead transfer is still live. The read is also NULL- and error-tolerant now that a NULL `x1` is
/// reachable: the old `row.get::<Vec<u8>>` and both `.unwrap()`s would have panicked the request
/// worker, which is a DoS on an endpoint every receiver calls to verify a coin.
pub async fn get_x1pub(pool: &sqlx::PgPool, statechain_id: &str) -> Option<PublicKey> {

    let query = "SELECT x1 \
        FROM statechain_transfer \
        WHERE statechain_id = $1 \
        AND cancelled_at IS NULL";

    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };

    // AUDITED-SWALLOW: an `x1` column that is NULL or does not parse is one this endpoint cannot
    // advertise — publishing a garbage `x1_pub` in `/info/statechain` would make the receiver derive
    // a t2 nobody can verify, which is strictly worse than not advertising it. Direction: withhold
    // an unusable value, never grant one. The transfer row itself is untouched and the coin is not
    // at risk; `apply_cancel` NULLing `x1` is exactly why the NULL case became reachable at all.
    let x1_secret_bytes: Vec<u8> = row.try_get::<Option<Vec<u8>>, _>("x1").ok().flatten()?;
    let secret_x1 = SecretKey::from_slice(&x1_secret_bytes).ok()?;

    Some(secret_x1.public_key(&Secp256k1::new()))
}

pub async fn get_statechain_transfer_messages(pool: &sqlx::PgPool, new_user_auth_key: &PublicKey) -> Vec::<String> {

    // `cancelled_at IS NULL` as well as the NOT NULL message check: `apply_cancel` clears the
    // message, so this filter is belt-and-braces — a cancelled transfer must never keep serving
    // claimable material from the mailbox, whichever of the two writes a future change forgets.
    let query = "\
        SELECT encrypted_transfer_msg \
        FROM statechain_transfer \
        WHERE new_user_auth_public_key = $1
        AND encrypted_transfer_msg IS NOT NULL \
        AND cancelled_at IS NULL \
        ORDER BY updated_at ASC";

    let rows = sqlx::query(query)
        .bind(new_user_auth_key.serialize())
        .fetch_all(pool)
        .await
        .unwrap();

    let mut result = Vec::<String>::new();

    for row in rows {
        let encrypted_transfer_msg: Vec<u8> = row.get(0);
        result.push(hex::encode(encrypted_transfer_msg));
    }

    result
}

pub async fn get_auth_pubkey_and_x1(pool: &sqlx::PgPool, statechain_id: &str) -> Option<(PublicKey, Vec<u8>)> {

    // `cancelled_at IS NULL`: a cancelled transfer is not claimable. The endpoint's existing
    // `is_none()` branch turns that into a 404 — and then consults the cancellation tombstone so a
    // caller that PROVES it holds the recorded recipient key gets a typed "cancelled" answer
    // instead of a bare not-found it cannot tell from an idle mailbox.
    let query = "\
        SELECT new_user_auth_public_key, x1 \
        FROM statechain_transfer \
        WHERE statechain_id = $1 \
        AND cancelled_at IS NULL";

    // fetch_optional, NOT fetch_one().unwrap(): fetch_one returns Err(RowNotFound) for an unknown
    // statechain_id, and this function runs BEFORE auth in POST /transfer/receiver, so the old
    // .unwrap() let any unauthenticated caller panic the handler (500) with a random statechain_id —
    // a pre-auth DoS and an existence oracle (a coin WITH a pending transfer answered differently
    // from one without). Returning None makes the endpoint's existing `is_none()` guard emit a
    // uniform 404 (adversarial-log review, MALFORM/REORDER).
    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };

    // Both columns are nullable and both `.unwrap()`s were reachable panics (a NULL `x1` became
    // reachable with migration 0010, which clears it on cancellation). A row we cannot read is a row
    // we cannot authorize a claim against: report absent, do not kill the worker.
    let new_user_auth_public_key_bytes: Vec<u8> =
        row.try_get::<Option<Vec<u8>>, _>(0).ok().flatten()?;
    // AUDITED-SWALLOW: the recorded recipient key is the ONLY thing a claim's signature is checked
    // against; if it does not parse there is nothing to check under, so the choice is "refuse" or
    // "accept unverified" — this refuses. Direction: strictly LESS privilege granted. The caller
    // turns `None` into the generic 404; the transfer row, its message and the coin are untouched.
    let new_user_auth_public_key = PublicKey::from_slice(&new_user_auth_public_key_bytes).ok()?;

    let x1_bytes: Vec<u8> = row.try_get::<Option<Vec<u8>>, _>(1).ok().flatten()?;

    Some((new_user_auth_public_key, x1_bytes))
}

pub async fn is_key_already_updated(pool: &sqlx::PgPool, statechain_id: &str) -> bool {

    let query = "\
        SELECT key_updated \
        FROM statechain_transfer \
        WHERE statechain_id = $1";

    // fetch_optional, not fetch_one().unwrap(): a missing row must not panic the handler. Absent
    // row → treat as not-yet-updated (adversarial-log review, panic-hardening).
    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return false,
    };

    let key_updated: bool = row.get(0);

    key_updated
}

pub async fn get_server_public_key(pool: &sqlx::PgPool, statechain_id: &str) -> Option<PublicKey> {

    let query = "\
        SELECT server_public_key \
        FROM statechain_data \
        WHERE statechain_id = $1";

    // fetch_optional, not fetch_one().unwrap(): a missing statechain_data row must not panic the
    // handler — return None so the endpoint reports it cleanly (adversarial-log review).
    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };

    let server_public_key_bytes: Vec<u8> = row.get(0);

    if server_public_key_bytes.len() == 0 {
        return None;
    }

    let server_public_key = PublicKey::from_slice(&server_public_key_bytes).unwrap();

    Some(server_public_key)
}

pub async fn update_statechain(pool: &sqlx::PgPool, auth_key: &XOnlyPublicKey, server_public_key: &PublicKey, statechain_id: &str)  {

    let mut transaction = pool.begin().await.unwrap();

    let query = "UPDATE statechain_data \
        SET auth_xonly_public_key = $1, server_public_key = $2 \
        WHERE statechain_id = $3";

    let _ = sqlx::query(query)
        .bind(&auth_key.serialize())
        .bind(&server_public_key.serialize())
        .bind(statechain_id)
        .execute(&mut *transaction)
        .await
        .unwrap();

    let query = "UPDATE statechain_transfer \
        SET key_updated = true \
        WHERE statechain_id = $1";

    let _ = sqlx::query(query)
        .bind(statechain_id)
        .execute(&mut *transaction)
        .await
        .unwrap();

    transaction.commit().await.unwrap();
}

/// Clear one lock bit of a transfer (owner → `locked2`, receiver → `locked`) and, once both are
/// clear, release any associated Lightning latch. Returns a typed `Err(message)` instead of
/// panicking when the statechain_id has no transfer row (external review finding 1: the old
/// `fetch_one().unwrap()` 500-crashed the handler on an unknown id) or on any DB error.
pub async fn update_unlock_transfer(pool: &sqlx::PgPool, is_current_owner: bool, statechain_id: &str) -> Result<(), String> {

    let locked_field = if is_current_owner { "locked2" } else { "locked" };

    let query = format!("UPDATE statechain_transfer \
        SET {} = false, updated_at = NOW() \
        WHERE statechain_id = $1", locked_field);

    let updated = sqlx::query(&query)
        .bind(statechain_id)
        .execute(pool)
        .await
        .map_err(|e| format!("could not update transfer lock: {e}"))?;

    // No row for this statechain_id: nothing to unlock — report cleanly rather than reading back a
    // row that does not exist.
    if updated.rows_affected() == 0 {
        return Err("no transfer found for this statechain id".to_string());
    }

    let query = "SELECT locked, locked2, batch_id \
        FROM statechain_transfer \
        WHERE statechain_id = $1";

    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("could not read transfer lock state: {e}"))?
    {
        Some(r) => r,
        None => return Err("no transfer found for this statechain id".to_string()),
    };

    let locked: bool = row.get(0);
    let locked2: bool = row.get(1);
    let batch_id: Option<String> = row.get(2);

    // if there is no lightning latch operation, the update below will have no effect

    if let Some(batch_id) = batch_id {
        if !locked && !locked2 {
            let query = "UPDATE lightning_latch \
                SET locked = false, updated_at = NOW() \
                WHERE statechain_id = $1
                AND batch_id = $2";

            sqlx::query(query)
                .bind(statechain_id)
                .bind(batch_id)
                .execute(pool)
                .await
                .map_err(|e| format!("could not release lightning latch: {e}"))?;
        }
    }

    Ok(())
}

// ==================================================================================================
// THE PREMISE THE CANCEL ENDPOINT'S SENDER-AUTH ORDERING RESTS ON.
//
// `POST /transfer/cancel` authenticates the sender leg against the key that OPENED the transfer
// (`statechain_transfer.sender_auth_xonly_public_key`, migration 0011) rather than against the
// coin's LIVE auth key, because `update_statechain` below ROTATES that live key to the recipient's
// when a claim completes — which used to leave a sender unable to authenticate about its own
// transfer and made `CancelDecision::AlreadyClaimed` unreachable.
//
// That is only as strict as the live-key check because of THIS function's shape:
//
//     the key rotation and `key_updated = true` happen in ONE transaction.
//
// Given that, `key_updated = false` implies the live key has not moved since the row was opened, so
// the recorded opener IS the live key wherever a lock-releasing decision is possible
// (`mercurylib::transfer::cancel::every_lock_releasing_decision_requires_an_unclaimed_row` pins the
// other half). Split the two statements apart — commit the key rotation before the flag, say — and
// there is a window in which the coin's key has moved while `key_updated` still reads false, i.e. a
// window in which a SUPERSEDED key could reach a lock-RELEASING decision. That is the one way this
// ordering becomes more permissive, so it is pinned here, at the site that would cause it.
//
// Asserted on the source: `update_statechain` takes a live `sqlx::PgPool` and this repository has no
// test database, which is the same reason `endpoints::transfer_sender`'s consent-binding pin is
// written this way. Stated plainly rather than dressed up as a behavioural test.
// ==================================================================================================
#[cfg(test)]
mod claim_rotation_atomicity_tests {
    fn update_statechain_body() -> &'static str {
        const SIGNATURE: &str = "pub async fn update_statechain(";
        let src = include_str!("transfer_receiver.rs");
        let start = src.find(SIGNATURE).expect("`update_statechain` must exist");
        let rest = &src[start..];
        &rest[..rest.find("\n}\n").expect("`update_statechain` must be terminated")]
    }

    fn code_only(body: &str) -> String {
        body.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn check_rotation_is_atomic(body: &str) -> Result<(), String> {
        let code = code_only(body);

        let begin = code.find("pool.begin()").ok_or(
            "the claim's two writes must run inside ONE transaction — `pool.begin()` is missing",
        )?;
        let rotate = code
            .find("SET auth_xonly_public_key")
            .ok_or("the key rotation must happen here")?;
        let flag = code
            .find("SET key_updated = true")
            .ok_or("the claim flag must be set here")?;
        let commit = code
            .find("commit()")
            .ok_or("the transaction must be committed")?;

        if !(begin < rotate && begin < flag) {
            return Err("both writes must be issued AFTER `pool.begin()`".to_string());
        }
        if !(rotate < commit && flag < commit) {
            return Err("both writes must be issued BEFORE `commit()`: a rotation that lands \
                        without the claim flag opens a window in which a SUPERSEDED auth key could \
                        reach a lock-RELEASING cancel decision"
                .to_string());
        }
        // Two separate commits would defeat the point even with both writes present.
        let commits = code.matches("commit()").count();
        if commits != 1 {
            return Err(format!(
                "the claim must commit exactly ONCE, found {commits}: two commits is two windows"
            ));
        }
        Ok(())
    }

    /// **THE PIN.**
    #[test]
    fn the_key_rotation_and_the_claim_flag_are_one_transaction() {
        if let Err(why) = check_rotation_is_atomic(update_statechain_body()) {
            panic!(
                "a claim's key rotation and its `key_updated` flag are no longer atomic, which is \
                 the premise `transfer_cancel`'s sender-auth ordering depends on: {why}"
            );
        }
    }

    /// **THE PIN'S OWN TEST.** A pin that has stopped discriminating reports green.
    #[test]
    fn the_atomicity_pin_rejects_a_split_claim() {
        let real = update_statechain_body();
        assert!(check_rotation_is_atomic(real).is_ok(), "precondition: the real body passes");

        // The rotation committed on its own, ahead of the flag.
        let split = real.replacen(
            "let query = \"UPDATE statechain_transfer \\\n        SET key_updated = true",
            "transaction.commit().await.unwrap();\n    let query = \"UPDATE statechain_transfer \\\n        SET key_updated = true",
            1,
        );
        assert_ne!(split, real, "the fixture must actually differ from the real body");
        assert!(
            check_rotation_is_atomic(&split).is_err(),
            "an early commit between the two writes must be rejected"
        );

        // No transaction at all.
        let no_tx = real.replace("pool.begin()", "no_transaction_at_all()");
        assert!(check_rotation_is_atomic(&no_tx).is_err());

        // The flag dropped entirely — a claim that never marks the row claimed.
        let no_flag = real.replace("SET key_updated = true", "SET updated_at = NOW()");
        assert!(check_rotation_is_atomic(&no_flag).is_err());

        // A comment-only edit must NOT be reported as a defect.
        let recommented = format!("{real}\n    // commit() SET key_updated = true pool.begin()");
        assert!(check_rotation_is_atomic(&recommented).is_ok(), "the pin must measure code, not comments");
    }
}
