
use chrono::Utc;
use electrum_client::ElectrumApi;
use mercurylib::{transfer::receiver::StatechainInfoResponsePayload, utils::{InfoConfig, ServerConfig}, wallet::Activity, withdraw::WithdrawCompletePayload};
use anyhow::{anyhow, Result, Ok};
use reqwest::StatusCode;
use crate::client_config::ClientConfig;

pub async fn info_config(client_config: &ClientConfig) -> Result<InfoConfig>{

    let path = "info/config";

    let client = client_config.get_reqwest_client()?;
    let request = client.get(&format!("{}/{}", client_config.statechain_entity, path));

    let response = request.send().await?;
    // Status BEFORE body. `info/config` is served unconditionally today, but a coordinator behind a
    // proxy, mid-restart or mid-migration answers `{"message": …}` with a non-2xx — and parsing that
    // as a `ServerConfig` reports "missing field `initlock`", which says nothing about what
    // happened. See `server_refusal`.
    let status = response.status().as_u16();
    let value = response.text().await?;
    if !(200..300).contains(&status) {
        return Err(crate::utils::server_refusal("info/config", status, &value));
    }

    let server_config: ServerConfig = serde_json::from_str(value.as_str())?;

    // [D8(f)] THE COORDINATOR'S COPY IS A CROSS-CHECK, NOT THE SOURCE.
    //
    // `interval` is what INV-5 measures every flat-backup hop against, and that check is the defence
    // against a sender padding the backup vector to inflate `flat_backups` and absorb a hidden
    // co-signed state. Taking it from the coordinator let the coordinator define the defence.
    //
    // Deriving it from the conveyed chain instead would be circular — a padded chain with uniform
    // `I/2` decrements derives `I/2` and validates against itself — so the value is compiled in per
    // network, and what the coordinator says must MATCH or the call is refused by name. A mismatch is
    // not something to paper over: it means this client and that coordinator disagree about what a
    // valid ladder is, and proceeding would validate against the wrong yardstick.
    let network = client_config.network.to_string();
    let (initlock, interval) = match mercurylib::tesr::TesrParams::flat_ladder_params(&network) {
        Some(p) => p,
        None => {
            return Err(anyhow!(
                "unknown network {:?}: refusing to guess the flat-ladder parameters. `interval` is \
                 what INV-5 measures every backup hop against, so a wrong value silently changes \
                 which ladders this wallet accepts.",
                network
            ));
        }
    };
    if server_config.initlock != initlock || server_config.interval != interval {
        return Err(anyhow!(
            "the coordinator reports initlock={} interval={} but this client compiles in \
             initlock={} interval={} for network {:?}. These govern INV-5 — the rule that each flat \
             backup decrements by EXACTLY `interval`, which is what stops a sender padding the chain \
             to hide a co-signed state — so they cannot be taken on the coordinator's word, and a \
             disagreement means one of us is validating against the wrong ladder. Refusing.",
            server_config.initlock, server_config.interval, initlock, interval, network
        ));
    }

    let number_blocks = 3;
    let mut fee_rate_btc_per_kb = client_config.electrum_client.estimate_fee(number_blocks)?;

    // Why does it happen?
    if fee_rate_btc_per_kb <= 0.0 {
        fee_rate_btc_per_kb = 0.00001;
    }

    let fee_rate_sats_per_byte = fee_rate_btc_per_kb * 100000.0;

    Ok(InfoConfig {    
        initlock,
        interval,
        fee_rate_sats_per_byte,
    })
}

pub fn create_activity(utxo: &str, amount: u32, action: &str) -> Activity {

    let date = Utc::now(); // This will get the current date and time in UTC
    let iso_string = date.to_rfc3339(); // Converts the date to an ISO 8601 string

    let activity = Activity {
        utxo: utxo.to_string(),
        amount,
        action: action.to_string(),
        date: iso_string
    };

    activity
}

pub async fn get_statechain_info(statechain_id: &str, client_config: &ClientConfig) -> Result<Option<StatechainInfoResponsePayload>> {

    // [D8-CLOSE] ASK THE SE TO SIGN THE COUNT, against a challenge THIS call chooses.
    //
    // `num_sigs` is the right-hand side of the receiver's anti-theft census
    // (`se_num_sigs == flat_backups + tiers + superseded`, exact equality). Unattested, a coordinator
    // that under-reports it by k hides k co-signed rival states: the census still balances exactly
    // and the receiver accepts a coin the sender can still reclaim.
    //
    // The nonce is per-REQUEST and random. Without it the attestation is a static value a
    // coordinator could capture when the count was legitimately lower and replay forever.
    let nonce: [u8; 32] = rand::random();
    let nonce_hex = hex::encode(nonce);

    let path = format!("info/statechain/{}", statechain_id.to_string());

    let client = client_config.get_reqwest_client()?;
    let request = client.get(&format!(
        "{}/{}?attestation_nonce={}",
        client_config.statechain_entity, path, nonce_hex
    ));

    let response = request.send().await?;

    // 404 is the ANSWER "no such statechain", not a failure — it stays `Ok(None)`.
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }

    // Every OTHER non-2xx is a refusal with the coordinator's own words in it — `statechain_info`
    // answers a missing enclave index with a 500 and `{"message": "Enclave index for statechain …"}`.
    // Deserialising that as a `StatechainInfoResponsePayload` reported "missing field
    // `enclave_public_key`" and threw the sentence away. See `server_refusal`.
    let status = response.status().as_u16();
    let value = response.text().await?;
    if !(200..300).contains(&status) {
        return Err(crate::utils::server_refusal("info/statechain", status, &value));
    }

    let response: StatechainInfoResponsePayload = serde_json::from_str(value.as_str())?;

    // [D8-CLOSE] VERIFY THE ATTESTATION, AND REQUIRE IT. Under D23 there is no phased rollout: an
    // unattested count is refused rather than recorded-and-accepted, because "accept for now" is
    // indistinguishable from the hole it replaces.
    //
    // The verifying key is the CHAIN-ANCHORED one — `enclave_public_key`, which the receiver already
    // binds to the on-chain tx0 output (`validate_tx0_output_pubkey`). Verifying against the SERVED
    // `attestation_pubkey` instead would accept a coordinator signing with a key of its own, which
    // is exactly the attack; `verify_sig_count_attestation` refuses if the two disagree.
    match (&response.sig_count_attestation, &response.sig_count_attestation_pubkey) {
        (Some(sig), Some(pk)) => {
            // [D8(i)] The budget is verified TOGETHER with the count, in one signature. `has_sig_budget`
            // is what decides the shape: `Some(false)` is the enclave saying "this coin has no
            // budget", `None` is an enclave that cannot say — and the latter must not be silently
            // read as the former, so it is refused below rather than defaulted here.
            let attested_budget = match response.has_sig_budget {
                Some(true) => match response.sig_budget {
                    Some(b) => Some(b),
                    None => {
                        return Err(anyhow::anyhow!(
                            "the coordinator reported that {statechain_id} HAS a spend budget but \
                             served no value for it. Terminality is what a receiver's census rests \
                             on, so a half-stated budget is refused rather than guessed."
                        ));
                    }
                },
                Some(false) => None,
                None => {
                    return Err(anyhow::anyhow!(
                        "the coordinator served no spend-budget field for {statechain_id}. That is \
                         not the same as 'no budget': it means the enclave cannot say whether this \
                         coin is terminal, and terminality is what the receiver's census rests on. \
                         Upgrade the enclave."
                    ));
                }
            };
            mercurylib::transfer::receiver::verify_sig_count_attestation(
                statechain_id,
                response.num_sigs,
                attested_budget,
                &nonce_hex,
                sig,
                pk,
                &response.enclave_public_key,
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "the enclave's attestation over num_sigs={} for {statechain_id} did NOT verify \
                     ({e}). This count is the right-hand side of the anti-theft census, so an \
                     unverified one lets a coordinator hide co-signed rival states while the census \
                     still balances — refusing rather than proceeding on it.",
                    response.num_sigs
                )
            })?;
        }
        _ => {
            return Err(anyhow::anyhow!(
                "the coordinator returned num_sigs={} for {statechain_id} with NO enclave \
                 attestation. The count is the census's right-hand side; unattested, a coordinator \
                 that under-reports it by k hides k co-signed rival states and the exact-equality \
                 census still balances. Either the coordinator or the enclave predates the \
                 attestation — upgrade both.",
                response.num_sigs
            ));
        }
    }

    Ok(Some(response))
}

/// Fetch a single-use SE auth challenge and return an endpoint-bound owner-auth token
/// `"<nonce>:<sig>"` for `coin` (audit [15]). Signs `sha256(nonce|endpoint)` with the coin's auth
/// key so a captured signature cannot be replayed against an irreversible owner op. Use in place of
/// the static `coin.signed_statechain_id` on nonce-protected endpoints.
pub async fn fresh_auth(
    client_config: &ClientConfig,
    statechain_id: &str,
    coin: &mercurylib::wallet::Coin,
    endpoint: &str,
) -> Result<String> {
    let client = client_config.get_reqwest_client()?;
    let resp = client
        .get(&format!("{}/auth/challenge/{}", client_config.statechain_entity, statechain_id))
        .send()
        .await?;
    if resp.status() != StatusCode::OK {
        return Err(anyhow!("auth challenge failed: {}", resp.text().await?));
    }
    let v: serde_json::Value = resp.json().await?;
    let nonce = v
        .get("nonce")
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow!("no nonce in auth challenge response"))?;
    let sig = mercurylib::transfer::receiver::sign_message(&format!("{nonce}|{endpoint}"), coin)?;
    Ok(format!("{nonce}:{sig}"))
}

pub async fn complete_withdraw(statechain_id: &str, signed_statechain_id: &str, client_config: &ClientConfig) -> Result<()> {

    let endpoint = client_config.statechain_entity.clone();
    let path = "withdraw/complete";

    let client = client_config.get_reqwest_client()?;
    let request = client.post(&format!("{}/{}", endpoint, path));

    let delete_statechain_payload = WithdrawCompletePayload {
        statechain_id: statechain_id.to_string(),
        signed_statechain_id: signed_statechain_id.to_string(),
    };

    let response = request.json(&delete_statechain_payload).send().await?;

    if response.status() != 200 {
        let response_body = response.text().await?;
        return Err(anyhow!(response_body));
    }

    Ok(())

}
// ==================================================================================================
// A SERVER REFUSAL MUST NOT ARRIVE AS A PARSE ERROR.
//
// Every coordinator endpoint answers a REFUSAL with `{"message": "…"}` (sometimes with an `error`
// field beside it) and a non-2xx status — a different shape from the success payload the client
// deserialises. A client that deserialises the body as the success payload without first looking at
// the status therefore converts a well-formed, meaningful refusal into
// `missing field 'x' at line 1 column N`: the server's sentence is discarded, the status is
// discarded, and the failure reads as a CLIENT parsing bug. Whoever meets it goes looking in the
// wrong half of the system.
//
// This was found on `POST /transfer/cancel`, where the coordinator's generic auth refusal surfaced
// as "could not read the cancel response (missing field `code`)". The audit that followed found the
// same assumption on `info/config`, `info/statechain/<id>` and `transfer/get_msg_addr/<key>`, all
// of which have server paths that return exactly that shape. These two helpers are the single place
// the degradation is defined, so the sites cannot drift apart from one another.
// ==================================================================================================

/// The most specific thing the server said, out of a body that is not the expected success payload.
///
/// Order matters: `message` is the field every coordinator refusal carries, `error` is the second
/// field a few endpoints add beside it. Only if neither is present does the raw body stand in — and
/// then bounded, because a body that is not the coordinator's JSON at all (a reverse proxy's HTML
/// error page, a gateway timeout) must not be pasted whole into an error string.
pub fn server_message_from_body(text: &str) -> String {
    if let core::result::Result::Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        for field in ["message", "error"] {
            if let Some(s) = v.get(field).and_then(|m| m.as_str()) {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "the coordinator returned an empty body".to_string();
    }
    const MAX: usize = 512;
    if trimmed.chars().count() > MAX {
        let head: String = trimmed.chars().take(MAX).collect();
        format!("{head}…")
    } else {
        trimmed.to_string()
    }
}

/// The error a non-success HTTP response becomes: what was being asked for, the status, and the
/// server's own words — never a serde complaint about the success payload's fields.
pub fn server_refusal(what: &str, status: u16, body: &str) -> anyhow::Error {
    anyhow!("{what} failed ({status}): {}", server_message_from_body(body))
}

// ==================================================================================================
// THE AUDIT, PINNED ON THE CALL SITES.
//
// These are I/O functions — they need a live coordinator, which this crate's test suite does not
// have — so the assertion is made on their SOURCE, which is the same thing this file's neighbours
// do for `transfer_cancel`'s consent binding and for `post_cancel`. It is a weaker kind of pin than
// a behavioural one and is stated as such; the DEGRADATION ITSELF is pinned behaviourally, by
// calling `server_message_from_body`, in `server_message_tests` below.
// ==================================================================================================
/// [D8(f)] `info_config` looks the flat ladder up by `client_config.network.to_string()`, so the
/// spelling `bitcoin::Network` produces must be a spelling the table knows — for EVERY variant.
///
/// This is pinned because the failure is silent in the wrong direction: an unrecognised spelling
/// makes `info_config` return `Err`, which bricks every wallet call on that network rather than
/// weakening anything, and it would not show up until someone actually ran that network. A unit test
/// over the table alone cannot catch it, because the table is keyed on strings and the bug is in
/// which string arrives.
#[cfg(test)]
mod network_spelling_tests {
    use bitcoin::Network;

    #[test]
    fn every_network_variant_resolves_in_the_flat_ladder_table() {
        // Listed explicitly rather than iterated: `Network` is `#[non_exhaustive]`, so a new variant
        // must be added here deliberately — which is the point.
        for net in [Network::Bitcoin, Network::Testnet, Network::Signet, Network::Regtest] {
            let spelling = net.to_string();
            assert!(
                mercurylib::tesr::TesrParams::flat_ladder_params(&spelling).is_some(),
                "bitcoin::Network::{net:?} renders as {spelling:?}, which the flat-ladder table does \
                 not know — `info_config` would refuse every coordinator on that network"
            );
        }
    }

    /// And the two live profiles must resolve to the numbers the deployed stacks actually serve.
    /// `docker-compose-lockbox.yml` (the running regtest stack) sets 1000/10; the mainnet stack sets
    /// 10000/100. `ci-guards/tests/deny_flat_ladder_config_drift.rs` checks the files themselves.
    #[test]
    fn the_live_profiles_resolve_to_the_deployed_numbers() {
        let f = |n: Network| mercurylib::tesr::TesrParams::flat_ladder_params(&n.to_string());
        assert_eq!(f(Network::Regtest), Some((1_000, 10)));
        assert_eq!(f(Network::Bitcoin), Some((10_000, 100)));
    }
}

#[cfg(test)]
mod server_response_reading_tests {
    /// A function body, comments stripped, from a source file in this crate.
    fn body_of(src: &'static str, signature: &str) -> String {
        let at = src
            .find(signature)
            .unwrap_or_else(|| panic!("`{signature}` must exist"));
        let rest = &src[at..];
        let body = &rest[..rest
            .find("\n}\n")
            .unwrap_or_else(|| panic!("`{signature}` must be terminated"))];
        body.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    const UTILS: &str = include_str!("utils.rs");
    const RECEIVER: &str = include_str!("transfer_receiver.rs");
    const SENDER: &str = include_str!("transfer_sender.rs");

    /// Every client read of a coordinator response must LOOK AT THE STATUS before it tries to
    /// deserialise the success payload, and must report a non-success through `server_refusal` so
    /// the server's own sentence reaches the caller.
    #[test]
    fn a_coordinator_refusal_never_reaches_the_caller_as_a_parse_error() {
        for (src, signature) in [
            (UTILS, "pub async fn info_config("),
            (UTILS, "pub async fn get_statechain_info("),
            (RECEIVER, "async fn get_msg_addr("),
        ] {
            let code = body_of(src, signature);
            assert!(
                code.contains("server_refusal("),
                "`{signature}` must degrade a non-success response through `server_refusal`, which \
                 carries the status and the server's `message`. Deserialising the body as the \
                 success payload turns a refusal into `missing field …` and loses both.\n---\n{code}\n---"
            );
            assert!(
                code.contains(".status()"),
                "`{signature}` must read the HTTP status before parsing the body.\n---\n{code}\n---"
            );
        }
    }

    /// `get_new_x1` DOES check the status — its defect is the other end of the same assumption: a
    /// 2xx body it cannot parse is `.expect()`ed, so a coordinator that answers something
    /// unexpected PANICS the caller's task instead of returning an error it can handle. A response
    /// from the network is never a reason to abort.
    #[test]
    fn get_new_x1_does_not_panic_on_a_body_it_cannot_parse() {
        let code = body_of(SENDER, "pub async fn get_new_x1(");
        assert!(
            !code.contains(".expect("),
            "a response body is network input: parsing it must produce an `Err`, never a \
             panic.\n---\n{code}\n---"
        );
    }
}

#[cfg(test)]
mod server_message_tests {
    use super::{server_message_from_body, server_refusal};

    /// The exact bodies the coordinator produces for a refusal, and the exact sentence that must
    /// come out of each.
    #[test]
    fn the_servers_own_sentence_is_what_survives() {
        for (body, expected) in [
            (
                r#"{"message":"Signature does not match authentication key."}"#,
                "Signature does not match authentication key.",
            ),
            (r#"{"message":"Statechain Id key not found."}"#, "Statechain Id key not found."),
            (
                r#"{"error":"Internal Server Error","message":"Invalid authentication public key"}"#,
                "Invalid authentication public key",
            ),
            // `error` alone still beats falling back to the raw body.
            (r#"{"error":"Internal Server Error"}"#, "Internal Server Error"),
        ] {
            assert_eq!(server_message_from_body(body), expected, "body {body}");
        }
    }

    /// A body that is not the coordinator's JSON at all still has to say something a human can act
    /// on, and must never be a serde complaint.
    #[test]
    fn a_body_that_is_not_json_still_reads_legibly_and_is_bounded() {
        assert!(server_message_from_body("<html>502 Bad Gateway</html>").contains("502"));
        assert!(!server_message_from_body("").is_empty());
        assert!(!server_message_from_body("   ").is_empty());
        // An empty `message` is not a message — fall through rather than report nothing.
        assert_eq!(server_message_from_body(r#"{"message":""}"#), r#"{"message":""}"#);
        // Unbounded bodies are truncated, so a proxy's error page cannot become the error string.
        let huge = "x".repeat(10_000);
        let msg = server_message_from_body(&huge);
        assert!(msg.chars().count() <= 513, "got {} chars", msg.chars().count());
        for body in ["", "   ", "not json", "{}", "[]", "null", "<html/>"] {
            assert!(
                !server_message_from_body(body).contains("missing field"),
                "a serde complaint is not an answer: {body}"
            );
        }
    }

    /// The refusal names what was being asked for, the status, and the server's words — the three
    /// things a caller needs and the three things a parse error threw away.
    #[test]
    fn a_refusal_carries_what_status_and_words() {
        let e = server_refusal(
            "info/statechain",
            500,
            r#"{"message":"Enclave index for statechain abc ID not found."}"#,
        );
        let s = e.to_string();
        assert!(s.contains("info/statechain"), "{s}");
        assert!(s.contains("500"), "{s}");
        assert!(s.contains("Enclave index for statechain abc ID not found."), "{s}");
        assert!(!s.contains("missing field"), "{s}");
    }
}
