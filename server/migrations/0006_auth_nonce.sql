-- Audit [15]: single-use, endpoint-bound owner-auth challenges. An owner requests a fresh nonce
-- (GET /auth/challenge/<statechain_id>) and signs sha256(nonce|endpoint) instead of the static
-- sha256(statechain_id). The nonce is consumed on first valid use, so a captured signature cannot
-- be replayed against irreversible owner operations (withdraw, set_spend_budget, ...).
CREATE TABLE IF NOT EXISTS server_auth_nonce (
    nonce          TEXT PRIMARY KEY,
    statechain_id  TEXT NOT NULL,
    expires_at     TIMESTAMPTZ NOT NULL,
    consumed       BOOLEAN NOT NULL DEFAULT false
);
CREATE INDEX IF NOT EXISTS idx_server_auth_nonce_sid ON server_auth_nonce (statechain_id);
