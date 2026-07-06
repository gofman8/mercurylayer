use sqlx::Row;

pub async fn get_enclave_index_from_database(pool: &sqlx::PgPool, statechain_id: &str) -> Option<i32> {

    let query = "SELECT enclave_index \
        FROM statechain_data \
        WHERE statechain_id = $1";

    // Do NOT unwrap: under load the pool can time out (PoolTimedOut). A panic here crashes the
    // worker thread and returns an unparseable 500. Return None gracefully so the endpoint replies
    // with a clean "enclave index not found" error instead of panicking.
    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("get_enclave_index_from_database query error: {e}");
            return None;
        }
    };

    let row = match row {
        Some(r) => r,
        None => return None,
    };

    let enclave_index: i32 = row.get("enclave_index");

    Some(enclave_index)
}
