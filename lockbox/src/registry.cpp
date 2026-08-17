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
        if (!n.parent_statechain_id.empty() && n.parent_statechain_id != n.statechain_id) {
            is_parent.insert(n.parent_statechain_id);
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

SetError validate(const std::vector<Leaf>& nodes) {
    std::unordered_set<std::string> ids;
    for (const auto& n : nodes) {
        if (!ids.insert(n.statechain_id).second) return SetError::DuplicateStatechainId;
    }
    for (const auto& n : nodes) {
        if (n.exit_key.empty()) return SetError::MissingExitKey;
        if (n.exit_key.size() != 32) return SetError::BadExitKeyLength;
        if (!n.parent_statechain_id.empty() &&
            ids.find(n.parent_statechain_id) == ids.end()) {
            // The SE's view of this tree is incomplete. A frontier computed from it could mark a
            // node as a leaf that actually has children, dropping a real obligation — so refuse
            // rather than decide on partial state.
            return SetError::ParentNotInSet;
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
