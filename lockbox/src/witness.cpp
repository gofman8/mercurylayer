#include "../include/witness.h"

#include <secp256k1.h>
#include <secp256k1_musig.h>

#include <crow.h>

#include <cstring>

#include "../include/tx.h"
#include "../include/utils.h"

namespace witness {
namespace {

/// The serialised session the client sends. Fixed by the route's own length check; named here so the
/// two cannot drift apart silently.
constexpr size_t SESSION_BYTES = 133;

std::vector<unsigned char> unhex(const std::string& s) {
    std::string h = s;
    if (h.size() >= 2 && h[0] == '0' && (h[1] == 'x' || h[1] == 'X')) h = h.substr(2);
    return utils::ParseHex(h);
}

bool json_str(crow::json::rvalue& o, const char* k, std::string* out) {
    if (o.count(k) == 0) return false;
    *out = o[k].s();
    return true;
}

}  // namespace

std::optional<Disclosure> parse_disclosure(const std::string& json_body) {
    auto body = crow::json::load(json_body);
    if (!body || body.count("disclosure") == 0) return std::nullopt;
    auto d = body["disclosure"];  // non-const: crow's accessors are not const-qualified

    Disclosure out;
    if (!json_str(d, "unsigned_tx", &out.unsigned_tx_hex)) return std::nullopt;
    if (!json_str(d, "agg_pubkey", &out.agg_pubkey_hex)) return std::nullopt;
    if (!json_str(d, "agg_nonce", &out.agg_nonce_hex)) return std::nullopt;
    if (!json_str(d, "blinding_factor", &out.blinding_factor_hex)) return std::nullopt;
    if (!json_str(d, "out_tweak", &out.out_tweak_hex)) return std::nullopt;

    if (d.count("input_index") == 0) return std::nullopt;
    out.input_index = static_cast<uint32_t>(d["input_index"].i());

    // The hash type is REQUIRED, not defaulted. The tiers sign at SIGHASH_ALL; silently assuming a
    // value here is how an implementation ends up binding against a hash nobody produces.
    if (d.count("hash_type") == 0) return std::nullopt;
    out.hash_type = static_cast<unsigned char>(d["hash_type"].i());

    if (d.count("prevout_values") == 0 || d.count("prevout_spks") == 0) return std::nullopt;
    auto vals = d["prevout_values"];
    auto spks = d["prevout_spks"];
    for (size_t i = 0; i < vals.size(); ++i) out.prevout_values.push_back(static_cast<uint64_t>(vals[i].u()));
    for (size_t i = 0; i < spks.size(); ++i) out.prevout_spks_hex.push_back(std::string(spks[i].s()));
    if (out.prevout_values.size() != out.prevout_spks_hex.size()) return std::nullopt;
    if (out.prevout_values.empty()) return std::nullopt;

    return out;
}

BindResult bind(const Disclosure& d,
                const std::vector<unsigned char>& serialized_session,
                std::vector<unsigned char>* out_sighash,
                std::string* out_detail) {
    auto fail = [&](BindResult r, const char* why) {
        if (out_detail) *out_detail = why;
        return r;
    };

    if (serialized_session.size() != SESSION_BYTES)
        return fail(BindResult::Malformed, "session is not 133 bytes");

    // ---- 1. the transaction, parsed by US, not described to us ---------------------------------
    const auto parsed = tx::parse_hex(d.unsigned_tx_hex);
    if (!parsed) return fail(BindResult::Malformed, "disclosed transaction does not parse");
    if (d.prevout_values.size() != parsed->vin.size())
        return fail(BindResult::Malformed, "prevout count does not match input count");
    if (d.input_index >= parsed->vin.size())
        return fail(BindResult::Malformed, "input_index out of range");

    std::vector<tx::Prevout> prevouts;
    prevouts.reserve(d.prevout_values.size());
    for (size_t i = 0; i < d.prevout_values.size(); ++i)
        prevouts.push_back({d.prevout_values[i], unhex(d.prevout_spks_hex[i])});

    // ---- 2. the sighash, computed HERE ----------------------------------------------------------
    const auto sighash =
        tx::taproot_key_path_sighash(*parsed, prevouts, d.input_index, d.hash_type);
    if (!sighash) return fail(BindResult::Malformed, "sighash could not be computed");

    // ---- 3. rebuild the blinded session ---------------------------------------------------------
    const auto agg_pk_bytes = unhex(d.agg_pubkey_hex);
    const auto agg_nonce_bytes = unhex(d.agg_nonce_hex);
    auto blinding = unhex(d.blinding_factor_hex);
    const auto tweak = unhex(d.out_tweak_hex);
    if (agg_pk_bytes.size() != 33) return fail(BindResult::Malformed, "agg_pubkey is not 33 bytes");
    if (agg_nonce_bytes.size() != 66) return fail(BindResult::Malformed, "agg_nonce is not 66 bytes");
    if (blinding.size() != 32) return fail(BindResult::Malformed, "blinding_factor is not 32 bytes");
    if (tweak.size() != 32) return fail(BindResult::Malformed, "out_tweak is not 32 bytes");

    secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_NONE);
    struct CtxGuard {
        secp256k1_context* c;
        ~CtxGuard() { secp256k1_context_destroy(c); }
    } guard{ctx};

    secp256k1_pubkey agg_pk;
    if (!secp256k1_ec_pubkey_parse(ctx, &agg_pk, agg_pk_bytes.data(), agg_pk_bytes.size()))
        return fail(BindResult::Malformed, "agg_pubkey does not parse");

    secp256k1_musig_aggnonce agg_nonce;
    if (!secp256k1_musig_aggnonce_parse(ctx, &agg_nonce, agg_nonce_bytes.data()))
        return fail(BindResult::Malformed, "agg_nonce does not parse");

    secp256k1_musig_session session;
    if (!secp256k1_blinded_musig_nonce_process_without_keyaggcoeff(
            ctx, &session, &agg_nonce, sighash->data(), &agg_pk, nullptr, blinding.data(),
            tweak.data())) {
        return fail(BindResult::Unsupported, "session could not be rebuilt from the disclosure");
    }

    // ---- 4. STRIP THE FIN NONCE, because that is what is actually on the wire --------------------
    //
    // The client does NOT send the session it built. It sends
    // `session.remove_fin_nonce_from_session().serialize()` — the fin nonce is removed first. So a
    // freshly-built session and the 133 bytes in the request differ, and comparing them directly
    // refuses every honest request.
    //
    // This was missed by the first differential because the vectors were generated from
    // `session.serialize()` rather than from the wire form: both sides carried the fin nonce, agreed
    // perfectly, and proved the wrong thing. Precisely the failure the differential exists to catch,
    // reached by testing against the wrong reference.
    //
    // The C symbol is the BLINDED one. The client appears to call a plain `..._musig_remove_...`,
    // but that Rust name is an alias: `secp256k1-zkp-sys/src/zkp.rs:673` declares it with
    // `link_name = "rustsecp256k1zkp_v0_8_1_blinded_musig_remove_fin_nonce_from_session"`. There is
    // one function; only the Rust-side spelling differs. Checked rather than assumed, because
    // picking the wrong one here would refuse every honest request.
    if (!secp256k1_blinded_musig_remove_fin_nonce_from_session(secp256k1_context_no_precomp,
                                                               &session)) {
        return fail(BindResult::Unsupported, "fin nonce could not be removed from the session");
    }

    // ---- 5. byte-compare -------------------------------------------------------------------------
    //
    // `secp256k1_musig_session` is an opaque 133-byte blob in this fork, which is exactly the length
    // the route accepts, so the comparison is over the whole object. A constant-time compare is used
    // not because the session is secret — it is not — but because there is no reason to leak a
    // prefix-match length to a caller probing for a near-miss.
    unsigned char diff = 0;
    const auto* got = reinterpret_cast<const unsigned char*>(&session);
    for (size_t i = 0; i < SESSION_BYTES; ++i) diff |= got[i] ^ serialized_session[i];
    if (diff != 0) {
        return fail(BindResult::Mismatch,
                    "the disclosed transaction does not produce the session being signed");
    }

    if (out_sighash) *out_sighash = *sighash;
    if (out_detail) out_detail->clear();
    return BindResult::Match;
}

std::optional<PayloadOutput> payload_output(const tx::Transaction& t) {
    std::optional<PayloadOutput> found;
    for (size_t i = 0; i < t.vout.size(); ++i) {
        const auto key = tx::p2tr_xonly_key(t.vout[i].script_pubkey);
        if (!key) continue;
        // A SECOND P2TR output means the payload is ambiguous. Refuse rather than take the first:
        // choosing by position is exactly the attacker-controlled selection this function exists to
        // remove, and a coloured tier with several payloads is out of scope here rather than
        // silently reduced to its first leg.
        if (found) return std::nullopt;
        found = PayloadOutput{static_cast<uint32_t>(i), *key, t.vout[i].value};
    }
    return found;
}

std::optional<PayloadOutput> payload_output(const std::string& unsigned_tx_hex) {
    const auto parsed = tx::parse_hex(unsigned_tx_hex);
    if (!parsed) return std::nullopt;
    return payload_output(*parsed);
}

bool output_pays_xonly(const std::string& unsigned_tx_hex, uint32_t vout,
                       const std::vector<unsigned char>& xonly) {
    const auto parsed = tx::parse_hex(unsigned_tx_hex);
    if (!parsed || vout >= parsed->vout.size()) return false;
    const auto key = tx::p2tr_xonly_key(parsed->vout[vout].script_pubkey);
    if (!key || key->size() != xonly.size()) return false;
    return std::memcmp(key->data(), xonly.data(), xonly.size()) == 0;
}

}  // namespace witness
