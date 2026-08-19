#pragma once

#include <cstdint>
#include <map>
#include <string>
#include <vector>

#include "tx.h"

/// The REQ-56 collapse predicate, as pure functions over a leaf set.
///
/// # Why this is separated from the database
///
/// REQ-56 is the load-bearing rule of the discharge round: the SE MUST refuse `collapse_grant`
/// unless, for the FRONTIER of a root (every node that is not the parent of another), every node not
/// marked `released` is paid its FULL funding value to its OWN exit key. Everything that makes that
/// rule right or wrong is arithmetic over a set of nodes and a transaction's outputs — no I/O. Kept
/// pure, it can be tested exhaustively in the image build, including the cases that matter most and
/// are hardest to reach through a live stack: a fork, a released sibling, two leaves sharing one
/// key, and an underpayment of a single satoshi.
///
/// The failure this shape guards against is specific. A predicate that is subtly permissive does not
/// announce itself: the honest path keeps working, and the only observable difference appears when
/// an operator collapses a tree without paying someone — at which point, per REQ-67, the absentee
/// has no recourse at all, because every tier beneath `F` is dead once `C` confirms.
namespace registry {

/// One node of a root's tree, as the SE recorded it at establishment.
///
/// `exit_key` is the payee's x-only key, read from the witnessed payload output of the state tier
/// (REQ-65) — never from a client assertion, and never at a client-supplied index (REQ-61(a)).
struct Leaf {
    std::string statechain_id;
    /// **[REQ-56a] EVERY parent, because a node can genuinely have several.**
    ///
    /// Empty for a node whose parent is the root itself. More than one for the child of a COMBINE:
    /// that transaction spends N coins into M children, so all N have stopped existing and all N
    /// are parents of each child. Measured on the live lane, where a 4-input migration-hatch
    /// combine produced exactly this shape.
    ///
    /// A single-parent field is not a simplification of this, it is a wrong answer: it marks ONE
    /// input as spent and leaves the other N-1 looking untouched, so each stays in its own frontier
    /// and a collapse is required to pay coins whose value already moved into the children.
    std::vector<std::string> parents;
    /// The FULL funding value (REQ-60), not the exit value: the two tier rungs are never broadcast,
    /// so their burn is never realised and is not the SSP's to keep.
    uint64_t fund_value = 0;
    /// 32-byte x-only key.
    std::vector<unsigned char> exit_key;
    /// Form (a)/(b) of the discharge: the holder migrated and signed a release, so `C` owes them
    /// nothing on chain.
    bool released = false;
};

/// The frontier: every node that is NOT the parent of another node in the set.
///
/// Order is preserved from the input so a caller's diagnostics stay stable. Nodes are identified by
/// `statechain_id`; a node listed twice is a malformed set and is reported by `validate`.
std::vector<Leaf> frontier(const std::vector<Leaf>& nodes);

/// What `C` must pay, keyed by exit key (hex), summed across unreleased frontier nodes.
///
/// Summing is INV-P: two leaves that happen to share an exit key are owed the TOTAL, and a
/// transaction paying only one of them is short. Released nodes contribute nothing.
std::map<std::string, uint64_t> owed(const std::vector<Leaf>& nodes);

/// Why a leaf set cannot be used for a predicate decision. Any of these MUST fail closed: a
/// malformed set is not a set with zero obligations.
enum class SetError {
    Ok,
    DuplicateStatechainId,
    MissingExitKey,
    BadExitKeyLength,
    ParentNotInSet,
    /// The SE has NO leaves for this root. Distinct from "this root owes nothing", and conflating
    /// the two is the most dangerous mistake available here — see `validate_for_grant`.
    UnknownRoot,
};

/// The check a `collapse_grant` MUST make before any payment arithmetic: does the SE know this root
/// at all?
///
/// # Why this is separate from `validate`, and why it is the difference between safe and catastrophic
///
/// An EMPTY leaf set produces an empty `owed`, and `pays_all_owed` satisfies an empty obligation set
/// vacuously — correctly, as arithmetic. But "I have no leaves for this root" does not mean "this
/// root owes nobody". It means **the SE does not know**, and those two must never be conflated:
/// while nothing populates the registry, EVERY root is empty, so a predicate that treats empty as
/// satisfied would grant every collapse ever asked of it, paying no one. That is precisely the
/// outcome REQ-56 exists to prevent, arrived at by a function that is individually correct.
///
/// SPEC §5.4 says this in its pseudocode — `T = tree[root_sid]  # absent => REFUSE (fail closed)` —
/// and the rule is stated here in code so a caller cannot reach the arithmetic without passing it.
SetError validate_for_grant(const std::vector<Leaf>& nodes);

/// A short, stable reason string for a `SetError`. The SSP has to know WHICH leaf set problem it is
/// looking at to fix a round; a bare "no" makes one un-debuggable.
const char* describe(SetError e);

/// Structural checks on the leaf set itself, before any payment arithmetic.
///
/// `ParentNotInSet` matters more than it looks: if a node names a parent the SE does not have, the
/// SE's view of the tree is incomplete, and a frontier computed from an incomplete tree can mark a
/// node as a leaf that actually has children — which would drop a real obligation.
SetError validate(const std::vector<Leaf>& nodes);

/// Does `tx` discharge every obligation in `owed`?
///
/// Outputs are matched to keys by P2TR scriptPubKey, and each output is consumed by at most one key
/// (INV-Q), so a single output cannot be counted twice to satisfy two obligations. A key is
/// satisfied when the outputs assigned to it total at least what is owed.
///
/// On refusal, `why` (when non-null) receives the first unmet key and the shortfall — the SSP needs
/// to know which leaf to add, and a bare "no" makes a round un-debuggable.
bool pays_all_owed(const tx::Transaction& tx,
                   const std::map<std::string, uint64_t>& obligations,
                   std::string* why);

}  // namespace registry
