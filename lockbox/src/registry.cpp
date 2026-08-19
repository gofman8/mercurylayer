#include "../include/registry.h"

#include <set>
#include <unordered_set>

namespace registry {
namespace {

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

}  // namespace

std::vector<Leaf> frontier(const std::vector<Leaf>& nodes) {
    // A node is interior iff some OTHER node names it as parent. Built as a set first so the cost is
    // linear rather than quadratic, and so a node naming itself as parent cannot make itself
    // disappear from its own frontier by accident.
    std::unordered_set<std::string> is_parent;
    for (const auto& n : nodes) {
        for (const auto& p : n.parents) {
            // Self-parenthood is ignored rather than trusted: a coin's own tier chain is several
            // transactions under one sid, so a naive resolver produces it, and a node that is its
            // own parent would delete itself from its own frontier and never be paid.
            if (!p.empty() && p != n.statechain_id) is_parent.insert(p);
        }
    }

    std::vector<Leaf> out;
    for (const auto& n : nodes) {
        if (is_parent.find(n.statechain_id) == is_parent.end()) out.push_back(n);
    }
    return out;
}

std::map<std::string, uint64_t> owed(const std::vector<Leaf>& nodes) {
    std::map<std::string, uint64_t> out;
    for (const auto& n : frontier(nodes)) {
        if (n.released) continue;  // form (a)/(b): discharged off-chain, owed nothing here
        out[to_hex(n.exit_key)] += n.fund_value;  // INV-P: FULL value, summed per key
    }
    return out;
}

const char* describe(SetError e) {
    switch (e) {
        case SetError::Ok: return "ok";
        case SetError::DuplicateStatechainId: return "a statechain id appears twice";
        case SetError::MissingExitKey: return "a leaf has no exit key";
        case SetError::BadExitKeyLength: return "a leaf's exit key is not 32 bytes";
        case SetError::ParentNotInSet: return "a leaf names a parent the SE does not have";
        case SetError::UnknownRoot: return "the SE has no leaves for this root";
    }
    return "unknown";
}

SetError validate(const std::vector<Leaf>& nodes) {
    std::unordered_set<std::string> ids;
    for (const auto& n : nodes) {
        if (!ids.insert(n.statechain_id).second) return SetError::DuplicateStatechainId;
    }
    for (const auto& n : nodes) {
        if (n.exit_key.empty()) return SetError::MissingExitKey;
        if (n.exit_key.size() != 32) return SetError::BadExitKeyLength;
        for (const auto& p : n.parents) {
            // EVERY named parent must be present, not merely the first. A combine's child names all
            // of its inputs, and a set missing even one of them is a set in which that input still
            // looks unspent — so it would sit in a frontier and be paid for a second time.
            if (!p.empty() && ids.find(p) == ids.end()) {
                // The SE's view of this tree is incomplete. A frontier computed from it could mark
                // a node as a leaf that actually has children, dropping a real obligation — so
                // refuse rather than decide on partial state.
                return SetError::ParentNotInSet;
            }
        }
    }
    return SetError::Ok;
}

SetError validate_for_grant(const std::vector<Leaf>& nodes) {
    // ORDER MATTERS: unknown-root is checked FIRST, so an empty set can never fall through to the
    // structural checks and come back Ok. `validate({})` is legitimately Ok — an empty set is not
    // malformed — which is exactly why a grant must not use `validate` alone.
    if (nodes.empty()) return SetError::UnknownRoot;
    return validate(nodes);
}

bool pays_all_owed(const tx::Transaction& t,
                   const std::map<std::string, uint64_t>& obligations,
                   std::string* why) {
    // INV-Q: each output may satisfy AT MOST ONE key. Without this, one output paying key K could be
    // counted again for key K' and two holders would be "paid" by one payment.
    std::set<size_t> used;

    for (const auto& [key_hex, amount] : obligations) {
        uint64_t got = 0;
        for (size_t i = 0; i < t.vout.size(); ++i) {
            if (used.count(i)) continue;
            const auto k = tx::p2tr_xonly_key(t.vout[i].script_pubkey);
            if (!k) continue;  // not P2TR: cannot pay an exit key, so it is not a candidate
            if (to_hex(*k) != key_hex) continue;
            got += t.vout[i].value;
            used.insert(i);
        }
        if (got < amount) {
            if (why) {
                *why = "key " + key_hex + " is owed " + std::to_string(amount) +
                       " but the transaction pays it " + std::to_string(got);
            }
            return false;
        }
    }

    if (why) why->clear();
    return true;
}

}  // namespace registry
