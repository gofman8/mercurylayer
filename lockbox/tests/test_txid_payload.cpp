// DIFFERENTIAL: txid, and the STRUCTURAL selection of a tier's payload output.
//
// Both exist so the leaf registry's facts are SE-authored rather than client-asserted:
//
//   * `tx::txid` is how a child's tier input `(SP.txid, j)` resolves to the sid the SE co-signed
//     `SP` under. Accepting a parent id from the request would let a caller graft its leaf onto
//     someone else's tree — and the frontier decides who gets paid.
//   * `witness::payload_output` is where `exit_key` and `fund_value` come from. REQ-61(a): the
//     client's `payload_vout` is attacker-supplied, so a key read at that index is a key the
//     attacker picked.
//
// The txid vectors come from `bitcoin`'s own `Transaction::txid()`
// (`cargo test -p ci-guards --test emit_txid_vectors`), including a WITNESS-BEARING transaction —
// the case that separates txid from wtxid. Without it, an implementation that hashes the segwit
// serialisation agrees with every unsigned tier the SE sees today and breaks on the first signed one.

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "../include/tx.h"
#include "../include/witness.h"

namespace {

int failures = 0;

void check(bool cond, const char* what) {
    if (cond) {
        std::printf("  ok   %s\n", what);
    } else {
        std::printf("  FAIL %s\n", what);
        ++failures;
    }
}

std::string to_hex(const std::vector<unsigned char>& d) {
    static const char* t = "0123456789abcdef";
    std::string s;
    s.reserve(d.size() * 2);
    for (unsigned char c : d) {
        s.push_back(t[c >> 4]);
        s.push_back(t[c & 0xf]);
    }
    return s;
}

struct Vector {
    const char* name;
    const char* tx_hex;
    const char* txid_internal;
    const char* txid_display;
    uint32_t payload_vout;
    const char* payload_xonly;
    uint64_t payload_value;
};

const Vector kVectors[] = {
#include "txid_vectors.inc"
};

tx::TxOut p2tr_out(unsigned char b, uint64_t v) {
    tx::TxOut o;
    o.value = v;
    o.script_pubkey.push_back(0x51);
    o.script_pubkey.push_back(0x20);
    for (int i = 0; i < 32; ++i) o.script_pubkey.push_back(b);
    return o;
}

tx::TxOut p2a_out() {
    tx::TxOut o;
    o.value = 240;
    o.script_pubkey = {0x51, 0x02, 0x4e, 0x73};
    return o;
}

}  // namespace

int main() {
    std::printf("== txid + structural payload selection ==\n");

    const size_t n = sizeof(kVectors) / sizeof(kVectors[0]);
    if (n == 0) {
        std::printf("  FAIL no vectors — this test would pass while checking nothing\n");
        return 1;
    }

    for (const auto& v : kVectors) {
        const auto parsed = tx::parse_hex(v.tx_hex);
        if (!parsed) {
            std::printf("  FAIL %s: did not parse\n", v.name);
            ++failures;
            continue;
        }

        // 1. txid matches bitcoin's, in the byte order an outpoint carries.
        const auto id = tx::txid(*parsed);
        char buf[160];
        std::snprintf(buf, sizeof(buf), "%s: txid matches bitcoin (internal order)", v.name);
        check(to_hex(id) == std::string(v.txid_internal), buf);

        // 2. And is NOT the reversed display form — the mix-up this pins.
        std::snprintf(buf, sizeof(buf), "%s: txid is not the reversed display form", v.name);
        check(to_hex(id) != std::string(v.txid_display), buf);

        // 3. The payload output is selected structurally, and matches what the builder intended —
        //    including the case where the ANCHOR COMES FIRST, which any vout[0] rule gets wrong.
        const auto po = witness::payload_output(*parsed);
        std::snprintf(buf, sizeof(buf), "%s: payload output found", v.name);
        check(po.has_value(), buf);
        if (po) {
            std::snprintf(buf, sizeof(buf), "%s: payload vout is %u, derived not assumed", v.name,
                          v.payload_vout);
            check(po->vout == v.payload_vout, buf);
            std::snprintf(buf, sizeof(buf), "%s: payload key matches", v.name);
            check(to_hex(po->xonly) == std::string(v.payload_xonly), buf);
            std::snprintf(buf, sizeof(buf), "%s: payload value matches", v.name);
            check(po->value == v.payload_value, buf);
        }
    }

    // ---- selection controls: ambiguity and absence must REFUSE, never guess -----------------------
    {
        tx::Transaction t;
        t.version = 3;
        t.lock_time = 0;
        t.vout = {p2tr_out(0xaa, 1000), p2tr_out(0xbb, 2000)};
        check(!witness::payload_output(t).has_value(),
              "TWO P2TR outputs is ambiguous and is refused, not resolved by position");
    }
    {
        tx::Transaction t;
        t.version = 3;
        t.lock_time = 0;
        t.vout = {p2a_out()};
        check(!witness::payload_output(t).has_value(),
              "a transaction with ONLY the P2A anchor has no payload output");
    }
    {
        tx::Transaction t;
        t.version = 3;
        t.lock_time = 0;
        check(!witness::payload_output(t).has_value(), "a transaction with no outputs is refused");
    }
    {
        // A near-miss script: OP_1 followed by 31 bytes. Not P2TR, and must not be read as one —
        // p2tr_xonly_key would otherwise hand back a short key that never matches anything.
        tx::Transaction t;
        t.version = 3;
        t.lock_time = 0;
        tx::TxOut o;
        o.value = 500;
        o.script_pubkey.push_back(0x51);
        o.script_pubkey.push_back(0x1f);
        for (int i = 0; i < 31; ++i) o.script_pubkey.push_back(0xcc);
        t.vout = {o, p2a_out()};
        check(!witness::payload_output(t).has_value(),
              "OP_1 <31 bytes> is not P2TR and yields no payload output");
    }
    {
        // The honest single-payload shape, built by hand rather than from a vector, so the positive
        // case does not depend on the generated file being present.
        tx::Transaction t;
        t.version = 3;
        t.lock_time = 0;
        t.vout = {p2a_out(), p2tr_out(0xde, 4242)};
        const auto po = witness::payload_output(t);
        check(po && po->vout == 1 && po->value == 4242,
              "the payload is found at index 1 when the anchor is first");
    }

    // ---- txid is stable under witness stripping ---------------------------------------------------
    // The witness-bearing vector and an otherwise-identical unsigned one must NOT share a txid here
    // only because both were stripped — they have different inputs. What IS asserted: parsing a
    // segwit serialisation and re-serialising it legacy reproduces bitcoin's txid, which the loop
    // above already checked for `tier_WITH_witness`. This states it explicitly so the intent is not
    // lost in the loop.
    {
        bool found_witness_case = false;
        for (const auto& v : kVectors) {
            if (std::string(v.name) == "tier_WITH_witness") found_witness_case = true;
        }
        check(found_witness_case,
              "the vector set INCLUDES a witness-bearing transaction (else txid vs wtxid is untested)");
    }

    if (failures) {
        std::printf("\n%d FAILURE(S) — the SE cannot be trusted to identify a parent or a payload\n",
                    failures);
        return 1;
    }
    std::printf("\nall passed: txid matches bitcoin, and the payload output is derived not trusted\n");
    return 0;
}
