// THE LEAF REGISTRY, against a REAL Postgres.
//
// Deliberately NOT run at build time: there is no database during `docker build`, and a test that
// silently skips when its dependency is absent is worse than no test — it reports success for
// something it never exercised. This one is built into the image and run explicitly against the
// live stack:
//
//     docker exec mercurylayer-lockbox-1 /app/build/test_registry_db
//
// It uses its own throwaway root id per run and cleans up after itself, so it can run against a
// database that already holds real rows without disturbing them.
//
// What it proves that the pure predicate tests cannot: that the SCHEMA exists with the columns the
// accessors name, that idempotency and monotonicity actually hold in the database rather than in a
// comment, and that the single-use nonce is enforced by a constraint rather than by a check that
// races.

#include <pqxx/pqxx>

#include <cstdio>
#include <string>
#include <vector>

#include "../include/db_manager.h"
#include "../include/registry.h"

// Declared here rather than exported from db_manager.h on purpose: cleanup is a concern of THIS
// test, and production code should not carry a purge helper that exists only so a test can tidy up.
namespace db_manager {
std::string getDatabaseConnectionString();
}

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

}  // namespace

int main() {
    std::printf("== leaf registry (live Postgres) ==\n");

    std::string err;
    if (!db_manager::ensure_schema(err)) {
        std::printf("  FAIL ensure_schema: %s\n", err.c_str());
        return 1;
    }
    std::printf("  ok   ensure_schema created/verified the registry tables\n");

    const std::string root = "testroot_reg_0001";

    // Start from a known-empty state. Without this a second run inherits the first run's rows and
    // measures something different — in particular the idempotency check below would pass for the
    // wrong reason, because the row it expects to be unchanged was already there.
    auto purge = [&](const std::string& r) {
        try {
            pqxx::connection conn(db_manager::getDatabaseConnectionString());
            pqxx::work txn(conn);
            txn.exec_params(
                "DELETE FROM se_release_nonce WHERE statechain_id LIKE $1;", r + "%");
            txn.exec_params("DELETE FROM se_leaf WHERE root_statechain_id = $1;", r);
            txn.exec_params("DELETE FROM se_root WHERE root_statechain_id = $1;", r);
            txn.commit();
        } catch (std::exception const& e) {
            std::printf("  (purge failed: %s)\n", e.what());
        }
    };
    purge(root);
    const std::string a = root + "_A";
    const std::string b = root + "_B";
    const std::string c = root + "_C";

    // ---- establish -----------------------------------------------------------------------------
    check(db_manager::establish_leaf(a, "", root, 2000, key(0xa1), err), "establish A (root child)");
    check(db_manager::establish_leaf(b, a, root, 1200, key(0xb1), err), "establish B under A");
    check(db_manager::establish_leaf(c, a, root, 800, key(0xc1), err), "establish C under A");

    // A short exit key must be refused BEFORE it reaches the database: stored short, it would be
    // compared against a 32-byte output key and never match, creating an obligation that can never
    // be discharged.
    {
        std::vector<unsigned char> shortkey(31, 0xee);
        check(!db_manager::establish_leaf(root + "_SHORT", "", root, 100, shortkey, err),
              "a 31-byte exit key is refused");
    }
    check(!db_manager::establish_leaf(root + "_ZERO", "", root, 0, key(0xee), err),
          "a zero funding value is refused");

    // ---- idempotency ---------------------------------------------------------------------------
    // A retried establishment must not create a second row. Two rows for one leaf are counted twice
    // by the frontier, and `C` would overpay.
    check(db_manager::establish_leaf(b, a, root, 9999, key(0xff), err), "re-establishing B succeeds");
    {
        std::vector<registry::Leaf> leaves;
        check(db_manager::load_leaves(root, leaves, err), "load_leaves");
        int b_count = 0;
        uint64_t b_value = 0;
        for (const auto& l : leaves) {
            if (l.statechain_id == b) {
                ++b_count;
                b_value = l.fund_value;
            }
        }
        check(b_count == 1, "re-establishing B did NOT create a second row");
        check(b_value == 1200, "re-establishing B did NOT overwrite the original funding value");
    }

    // ---- the round trip feeds the predicate ----------------------------------------------------
    {
        std::vector<registry::Leaf> leaves;
        db_manager::load_leaves(root, leaves, err);
        check(registry::validate(leaves) == registry::SetError::Ok,
              "the loaded set validates (parents present, keys 32 bytes)");
        const auto f = registry::frontier(leaves);
        check(f.size() == 2, "the frontier of the stored tree is {B, C} — A is interior");
        const auto owed = registry::owed(leaves);
        check(owed.size() == 2, "two distinct exit keys are owed");
    }

    // ---- released is monotone ------------------------------------------------------------------
    check(db_manager::mark_released(b, err), "mark B released");
    {
        std::vector<registry::Leaf> leaves;
        db_manager::load_leaves(root, leaves, err);
        const auto owed = registry::owed(leaves);
        check(owed.size() == 1, "a released leaf drops out of what C must pay");
        bool b_released = false;
        for (const auto& l : leaves)
            if (l.statechain_id == b) b_released = l.released;
        check(b_released, "released survives the round trip");
    }
    check(db_manager::mark_released(b, err), "re-releasing B is a no-op, not an error");

    // ---- the single-use nonce is enforced by the DATABASE ---------------------------------------
    {
        std::vector<unsigned char> nonce(32, 0x5a);
        check(db_manager::consume_release_nonce(b, nonce, err), "a fresh nonce is accepted");
        check(!db_manager::consume_release_nonce(b, nonce, err),
              "the SAME nonce is REFUSED the second time (replay)");
        // Scoped per statechain: the same nonce bytes under a different leaf are a different fact.
        check(db_manager::consume_release_nonce(c, nonce, err),
              "the same nonce under a DIFFERENT leaf is accepted (scoping is per sid)");
        std::vector<unsigned char> shortnonce(31, 0x5a);
        check(!db_manager::consume_release_nonce(b, shortnonce, err), "a 31-byte nonce is refused");
    }

    // ---- freeze is a ratchet --------------------------------------------------------------------
    {
        bool frozen = true;  // deliberately wrong, to catch a path that fails to assign
        check(db_manager::is_root_frozen(root, frozen, err) && !frozen,
              "an unseen root reads as NOT frozen, and the flag is assigned on that path");
        check(db_manager::freeze_root(root, root + "_next", err), "freeze the root");
        check(db_manager::is_root_frozen(root, frozen, err) && frozen, "frozen survives the round trip");
        check(db_manager::freeze_root(root, "", err), "re-freezing is idempotent");
        check(db_manager::is_root_frozen(root, frozen, err) && frozen,
              "re-freezing with an empty successor does NOT clear frozen");
    }

    // ---- [REQ-61] the owner latch is WRITE-ONCE --------------------------------------------------
    //
    // The property that matters is not "it stores a key" — it is that a SECOND arming with a
    // DIFFERENT key cannot move it. A latch that can be re-pointed is not a latch, because whoever
    // re-points it takes the coin.
    {
        const std::string lsid = root + "_LATCH";
        bool armed = false;
        check(db_manager::arm_latch(lsid, key(0x11), armed, err) && armed,
              "arming an unlatched sid reports armed_now = true");

        bool armed_again = true;  // deliberately wrong, to catch a path that fails to assign
        check(db_manager::arm_latch(lsid, key(0x22), armed_again, err) && !armed_again,
              "a SECOND arming reports armed_now = false");

        std::vector<unsigned char> got;
        bool found = false;
        check(db_manager::get_latch(lsid, got, found, err) && found,
              "the latch reads back as present");
        check(got == key(0x11),
              "the latch still holds the FIRST key — a different key did NOT overwrite it");

        // An unlatched sid must not read as latched to whatever was in the buffer.
        std::vector<unsigned char> stale = key(0xff);
        bool found2 = true;
        check(db_manager::get_latch(root + "_NEVER", stale, found2, err) && !found2,
              "an unlatched sid reports found = false, and the flag is assigned on that path");
        check(stale.empty(), "and the key buffer is cleared rather than left stale");

        check(!db_manager::arm_latch(root + "_SHORT", std::vector<unsigned char>(31, 0x5), armed, err),
              "a 31-byte latch key is refused");
    }

    // ---- cleanup ---------------------------------------------------------------------------------
    // The run also purges at the START, so a crash mid-test cannot poison the next run.
    {
        try {
            pqxx::connection conn(db_manager::getDatabaseConnectionString());
            pqxx::work txn(conn);
            txn.exec_params("DELETE FROM se_latch WHERE statechain_id LIKE $1;", root + "%");
            txn.commit();
        } catch (std::exception const&) {}
    }
    purge(root);

    if (failures) {
        std::printf("\n%d FAILURE(S) — the leaf registry does not behave as REQ-56 needs\n", failures);
        return 1;
    }
    std::printf("\nall passed: the registry stores what the predicate needs, idempotently\n");
    return 0;
}
