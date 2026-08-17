// THE REQ-56 PREDICATE, exhaustively — because a permissive predicate is silent.
//
// If this logic is subtly wrong in the permissive direction, nothing breaks: the honest path keeps
// working, every test that only exercises honest rounds stays green, and the single observable
// consequence is that one day an operator collapses a tree without paying somebody. By then it is
// unrecoverable — REQ-67: once `C` confirms, every tier beneath `F` is dead and the absentee has no
// recourse at all.
//
// So the cases below are chosen for the ways the rule can be quietly relaxed rather than for
// coverage of the happy path: an interior node mistaken for a leaf, a released sibling suppressing
// its neighbour, two leaves sharing a key, one output counted twice, and a shortfall of one satoshi.

#include <cstdio>
#include <string>
#include <vector>

#include "../include/registry.h"
#include "../include/tx.h"

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

std::vector<unsigned char> key(unsigned char b) { return std::vector<unsigned char>(32, b); }

std::string hex_of(unsigned char b) {
    static const char* t = "0123456789abcdef";
    std::string s;
    for (int i = 0; i < 32; ++i) {
        s.push_back(t[b >> 4]);
        s.push_back(t[b & 0xf]);
    }
    return s;
}

registry::Leaf leaf(const std::string& id, const std::string& parent, uint64_t v, unsigned char k,
                    bool released = false) {
    registry::Leaf l;
    l.statechain_id = id;
    l.parent_statechain_id = parent;
    l.fund_value = v;
    l.exit_key = key(k);
    l.released = released;
    return l;
}

/// A transaction paying `(key byte, value)` pairs to P2TR outputs, plus an optional non-P2TR output
/// to prove those are never mistaken for payment.
tx::Transaction paying(const std::vector<std::pair<unsigned char, uint64_t>>& outs,
                       bool add_p2a = true) {
    tx::Transaction t;
    t.version = 3;
    t.lock_time = 0;
    for (const auto& [k, v] : outs) {
        tx::TxOut o;
        o.value = v;
        o.script_pubkey.push_back(0x51);
        o.script_pubkey.push_back(0x20);
        for (int i = 0; i < 32; ++i) o.script_pubkey.push_back(k);
        t.vout.push_back(o);
    }
    if (add_p2a) {
        // The P2A anchor: `OP_1 <2-byte>`. Not P2TR, so it must never count toward an obligation.
        tx::TxOut a;
        a.value = 240;
        a.script_pubkey = {0x51, 0x02, 0x4e, 0x73};
        t.vout.push_back(a);
    }
    return t;
}

bool has_key(const std::map<std::string, uint64_t>& m, unsigned char k, uint64_t v) {
    auto it = m.find(hex_of(k));
    return it != m.end() && it->second == v;
}

}  // namespace

int main() {
    std::printf("== REQ-56 collapse predicate ==\n");

    // ---- frontier ---------------------------------------------------------------------------
    {
        // A chain A -> B -> C. Only C is a leaf. If the frontier wrongly included A or B, the
        // predicate would demand payment for nodes that have already been superseded.
        std::vector<registry::Leaf> chain{
            leaf("A", "", 1000, 0xa1), leaf("B", "A", 1000, 0xb1), leaf("C", "B", 1000, 0xc1)};
        const auto f = registry::frontier(chain);
        check(f.size() == 1 && f[0].statechain_id == "C", "a chain has exactly one frontier node");
    }
    {
        // A fork: A -> {B, C}. BOTH are leaves. Missing one here is the defect that loses a holder
        // their money, so it is checked by count AND by identity.
        std::vector<registry::Leaf> fork{
            leaf("A", "", 2000, 0xa1), leaf("B", "A", 1000, 0xb1), leaf("C", "A", 1000, 0xc1)};
        const auto f = registry::frontier(fork);
        const bool both = f.size() == 2 &&
                          ((f[0].statechain_id == "B" && f[1].statechain_id == "C") ||
                           (f[0].statechain_id == "C" && f[1].statechain_id == "B"));
        check(both, "a fork yields BOTH children and drops the interior parent");
    }
    {
        std::vector<registry::Leaf> one{leaf("A", "", 500, 0xa1)};
        check(registry::frontier(one).size() == 1, "a single node is its own frontier");
        check(registry::frontier({}).empty(), "an empty set has an empty frontier");
    }

    // ---- owed -------------------------------------------------------------------------------
    {
        std::vector<registry::Leaf> fork{
            leaf("A", "", 2000, 0xa1), leaf("B", "A", 1200, 0xb1), leaf("C", "A", 800, 0xc1)};
        const auto o = registry::owed(fork);
        check(o.size() == 2 && has_key(o, 0xb1, 1200) && has_key(o, 0xc1, 800),
              "owed lists each unreleased leaf at its FULL funding value");
    }
    {
        // A released sibling owes nothing — but must NOT suppress its neighbour.
        std::vector<registry::Leaf> fork{leaf("A", "", 2000, 0xa1),
                                         leaf("B", "A", 1200, 0xb1, /*released=*/true),
                                         leaf("C", "A", 800, 0xc1)};
        const auto o = registry::owed(fork);
        check(o.size() == 1 && has_key(o, 0xc1, 800),
              "a released leaf is owed nothing and does not suppress its sibling");
    }
    {
        // INV-P: two leaves sharing one exit key are owed the TOTAL. Paying one of them is short.
        std::vector<registry::Leaf> fork{
            leaf("A", "", 2000, 0xa1), leaf("B", "A", 1200, 0x77), leaf("C", "A", 800, 0x77)};
        const auto o = registry::owed(fork);
        check(o.size() == 1 && has_key(o, 0x77, 2000),
              "two leaves sharing an exit key are owed the SUM, not the larger");
    }

    // ---- validate ---------------------------------------------------------------------------
    {
        std::vector<registry::Leaf> dup{leaf("A", "", 1, 0xa1), leaf("A", "", 1, 0xa2)};
        check(registry::validate(dup) == registry::SetError::DuplicateStatechainId,
              "a duplicated statechain id is refused");

        std::vector<registry::Leaf> orphan{leaf("B", "MISSING", 1, 0xb1)};
        check(registry::validate(orphan) == registry::SetError::ParentNotInSet,
              "a parent outside the set is refused (the tree view is incomplete)");

        auto bad = leaf("A", "", 1, 0xa1);
        bad.exit_key.resize(31);
        check(registry::validate({bad}) == registry::SetError::BadExitKeyLength,
              "a 31-byte exit key is refused");

        auto none = leaf("A", "", 1, 0xa1);
        none.exit_key.clear();
        check(registry::validate({none}) == registry::SetError::MissingExitKey,
              "a missing exit key is refused");

        std::vector<registry::Leaf> good{leaf("A", "", 1000, 0xa1), leaf("B", "A", 1000, 0xb1)};
        check(registry::validate(good) == registry::SetError::Ok, "a well-formed set validates");
    }

    // ---- pays_all_owed ----------------------------------------------------------------------
    {
        std::vector<registry::Leaf> fork{
            leaf("A", "", 2000, 0xa1), leaf("B", "A", 1200, 0xb1), leaf("C", "A", 800, 0xc1)};
        const auto o = registry::owed(fork);
        std::string why;

        check(registry::pays_all_owed(paying({{0xb1, 1200}, {0xc1, 800}}), o, &why),
              "a transaction paying every leaf in full is accepted");

        // ONE SATOSHI SHORT. This is the case the whole predicate exists for.
        check(!registry::pays_all_owed(paying({{0xb1, 1199}, {0xc1, 800}}), o, &why),
              "one satoshi short on ONE leaf is refused");
        check(why.find("is owed 1200") != std::string::npos,
              "the refusal names the key and the shortfall, so the SSP can rebuild C");

        // Paying MORE is fine: `got >= amount`.
        check(registry::pays_all_owed(paying({{0xb1, 1300}, {0xc1, 800}}), o, &why),
              "overpaying a leaf is accepted");

        // A missing leaf entirely.
        check(!registry::pays_all_owed(paying({{0xb1, 1200}}), o, &why),
              "omitting a leaf altogether is refused");

        // The P2A anchor must never be mistaken for a payment: an empty payment set with only the
        // anchor present must still fail.
        check(!registry::pays_all_owed(paying({}, /*add_p2a=*/true), o, &why),
              "the P2A anchor does not discharge anything");
    }
    {
        // INV-Q, the subtle one: two leaves owed 1000 each, sharing NO key, but the transaction
        // pays one output of 2000 to the FIRST key only. The second must still be unpaid — a naive
        // "total outputs >= total owed" check would accept this and rob the second holder.
        std::vector<registry::Leaf> fork{
            leaf("A", "", 2000, 0xa1), leaf("B", "A", 1000, 0xb1), leaf("C", "A", 1000, 0xc1)};
        const auto o = registry::owed(fork);
        std::string why;
        check(!registry::pays_all_owed(paying({{0xb1, 2000}}), o, &why),
              "paying one key TWICE over does not discharge a different key");
    }
    {
        // INV-Q again, from the other side: one output cannot be counted for two keys. Two leaves
        // share key 0x77 and are owed 2000 total; a single 2000 output to 0x77 satisfies them, but a
        // single 1000 output must not.
        std::vector<registry::Leaf> fork{
            leaf("A", "", 2000, 0xa1), leaf("B", "A", 1000, 0x77), leaf("C", "A", 1000, 0x77)};
        const auto o = registry::owed(fork);
        std::string why;
        check(registry::pays_all_owed(paying({{0x77, 2000}}), o, &why),
              "one output covering the SUM for a shared key is accepted");
        check(!registry::pays_all_owed(paying({{0x77, 1000}}), o, &why),
              "one output covering only HALF the shared-key total is refused");
        check(registry::pays_all_owed(paying({{0x77, 1000}, {0x77, 1000}}), o, &why),
              "two outputs to the same key are summed");
    }
    {
        // An empty obligation set is vacuously satisfied AS ARITHMETIC — and that is exactly why a
        // grant must never reach the arithmetic without first asking whether the SE knows the root.
        std::string why;
        check(registry::pays_all_owed(paying({}), {}, &why),
              "no obligations is vacuously satisfied (arithmetic only)");
    }

    // ---- THE FAIL-CLOSED GATE: unknown root != owes nothing --------------------------------------
    //
    // This is the difference between a safe predicate and one that grants everything. Nothing
    // populates the registry in production yet, so EVERY root is currently empty; a grant that
    // treated empty as satisfied would authorise every collapse ever asked of it, paying no one —
    // the precise outcome REQ-56 exists to prevent, reached through functions that are each
    // individually correct.
    {
        check(registry::validate_for_grant({}) == registry::SetError::UnknownRoot,
              "an EMPTY leaf set is UnknownRoot for a grant, not Ok");
        check(registry::validate({}) == registry::SetError::Ok,
              "...while plain validate() still calls an empty set well-formed — which is why a "
              "grant must not use it alone");

        std::vector<registry::Leaf> good{leaf("A", "", 1000, 0xa1), leaf("B", "A", 1000, 0xb1)};
        check(registry::validate_for_grant(good) == registry::SetError::Ok,
              "a known root with well-formed leaves passes the grant gate");

        // A malformed non-empty set must still be caught by the grant gate, not just by validate.
        std::vector<registry::Leaf> orphan{leaf("B", "MISSING", 1, 0xb1)};
        check(registry::validate_for_grant(orphan) == registry::SetError::ParentNotInSet,
              "the grant gate still refuses a set whose parent is missing");
    }

    if (failures) {
        std::printf("\n%d FAILURE(S) — the collapse predicate would let a holder go unpaid\n",
                    failures);
        return 1;
    }
    std::printf("\nall passed: the predicate pays every unreleased frontier leaf, in full, per key\n");
    return 0;
}
