#!/usr/bin/env python3
"""[REQ-54 R6 / REQ-56] The collapse-grant DIFFERENTIAL, against a running lockbox.

Run:  python3 lockbox/tests/collapse_grant_probe.py
Needs the compose stack up (lockbox on :18080, its Postgres reachable via docker exec).

WHY A PROBE AND NOT A UNIT TEST. The predicate's arithmetic is already covered by `test_registry`
(pure) and `test_registry_db` (storage). What neither can reach is the ROUTE: the order its gates
run in, and whether each refuses on its own cause rather than being masked by the next. That is only
observable from outside, over HTTP, against a seeded tree.

The four cases are a DIFFERENTIAL, not a smoke test. Each differs from the granted case in exactly
one respect, so a refusal cannot be explained by anything else:

  (a) pays both leaves in full         -> reaches the SESSION gate. Since #169 the route also issues
                                          the partial signature, so a grant now requires a `session`
                                          that reproduces the disclosed transaction (REQ-57). This
                                          probe supplies no real MuSig2 session, so (a) is expected to
                                          refuse 400 AT THE SESSION GATE — which is itself the
                                          assertion that matters here: the predicate PASSED (or it
                                          would have refused 403 first) and no signature is issued
                                          without a bound session
  (b) one satoshi short for one leaf   -> 403 from the PAYMENT gate, naming the key and the shortfall
  (c) pays everyone, wrong funding tx  -> 403 from the OUTPOINT gate. A `C` that pays every owed key
                                          out of SOMEBODY ELSE'S money still is not a collapse of
                                          this root, and this case is what proves the two gates are
                                          independent rather than one standing in for the other
  (d) omits a leaf entirely            -> 403 from the PAYMENT gate, naming the unpaid key

ORDERING IS THE POINT. The predicate runs BEFORE the session bind, so (b), (c) and (d) still refuse
with their own 403 rather than being masked by a session failure. If a future edit moved the bind
earlier, those three would flip to 400 and this probe would say so.

Expected — EIGHT cases, each refusing on its OWN named gate. A differential that collapses to one
answer is not a differential, and this file has twice caught exactly that: once when REQ-74's check
sat behind the signature step, and once when a missing aggregate masked every later gate.

    pays both in full     : HTTP 400 ... session ...        (verdict passed, signature gated)
    one satoshi short     : HTTP 403 ... is owed 3000 ...   (payment gate)
    wrong funding outpoint: HTTP 403 ... does not spend ... (outpoint gate)
    omits a leaf          : HTTP 403 ... is owed 2000 ...   (payment gate)
    released, no next root: HTTP 400 ... session ...        (verdict passed)
    next root underfunded : HTTP 403 ... (REQ-74) ...       (self-funding gate)
    next root funded fully: HTTP 400 ... session ...        (verdict passed)
    pays all, WRONG coin  : HTTP 403 ... (REQ-68) ...       (coin binding, BEFORE the secnonce)
    one satoshi short     : HTTP 403 ... is owed 3000 ...
    wrong funding outpoint: HTTP 403 ... does not spend this root's funding output ...
    omits a leaf          : HTTP 403 ... is owed 2000 ...
"""
import hashlib, json, subprocess, sys
def ser_u32(n): return n.to_bytes(4,'little')
def ser_u64(n): return n.to_bytes(8,'little')
def vi(n):
    if n < 0xfd: return bytes([n])
    return b'\xfd'+n.to_bytes(2,'little')
def p2tr(xonly_hex): return bytes([0x51,0x20])+bytes.fromhex(xonly_hex)
def build(prev_txid_hex, prev_vout, outs):
    tx = ser_u32(2) + vi(1) + bytes.fromhex(prev_txid_hex) + ser_u32(prev_vout) + vi(0) + ser_u32(0xfffffffd)
    tx += vi(len(outs))
    for val,key in outs:
        spk = p2tr(key); tx += ser_u64(val) + vi(len(spk)) + spk
    return (tx + ser_u32(0)).hex()

ROOT="cgroot00000000000000000000000001"
KID1="cgkid10000000000000000000000001"
KID2="cgkid20000000000000000000000001"
K1="11"*32; K2="22"*32
AGG="ab"*32          # the root's stored aggregate (x-only)
AGG33="02"+AGG     # as a disclosure carries it: 33 bytes, parity prefix + x-only
FUND=("aa"*32, 0)

sql = f"""
DELETE FROM se_leaf_parent WHERE child_statechain_id LIKE 'cg%' OR parent_statechain_id LIKE 'cg%';
DELETE FROM se_leaf WHERE statechain_id LIKE 'cg%';
DELETE FROM se_root WHERE root_statechain_id LIKE 'cg%';
INSERT INTO se_leaf (statechain_id,parent_statechain_id,root_statechain_id,fund_value,exit_key,fund_txid,fund_vout)
 VALUES ('{ROOT}',NULL,'{ROOT}',5000,decode('{"33"*32}','hex'),decode('{FUND[0]}','hex'),{FUND[1]});
INSERT INTO se_leaf (statechain_id,parent_statechain_id,root_statechain_id,fund_value,exit_key)
 VALUES ('{KID1}','{ROOT}','{ROOT}',3000,decode('{K1}','hex'));
INSERT INTO se_leaf (statechain_id,parent_statechain_id,root_statechain_id,fund_value,exit_key)
 VALUES ('{KID2}','{ROOT}','{ROOT}',2000,decode('{K2}','hex'));
INSERT INTO se_leaf_parent VALUES ('{KID1}','{ROOT}'),('{KID2}','{ROOT}');
-- [REQ-68] The root needs a stored aggregate, or the grant refuses before any later gate is
-- reached and the differential collapses to one answer. Seeding it is what keeps the OTHER gates
-- observable; the wrong-aggregate case below is what proves this one still bites.
INSERT INTO se_aggregate (statechain_id, aggregate_xonly) VALUES ('{ROOT}', decode('{AGG}','hex'))
  ON CONFLICT (statechain_id) DO UPDATE SET aggregate_xonly = EXCLUDED.aggregate_xonly;
"""
subprocess.run(["docker","exec","-i","mercurylayer-db_lockbox-1","psql","-U","postgres","-d","enclave","-c",sql],
               capture_output=True)

def post(txhex, label, agg=AGG33):
    # A 133-byte PLACEHOLDER session. Cases (b)-(d) never reach the bind — the predicate refuses
    # first — so a real MuSig2 session is not needed to exercise them, and the granted case reaching
    # the bind and refusing there is exactly what this probe asserts.
    body = {"root_statechain_id": ROOT,
            "session": "00"*133,
            "disclosure": {"unsigned_tx": txhex, "input_index":0,
                           "prevout_values":[6000], "prevout_spks":["5120"+"33"*32],
                           "agg_pubkey": agg, "agg_nonce":"00"*66,
                           "blinding_factor":"00"*32, "out_tweak":"00"*32, "hash_type":1}}
    r = subprocess.run(["curl","-s","-w","\\n%{http_code}","-X","POST","http://localhost:18080/collapse_grant",
                        "-H","Content-Type: application/json","-d",json.dumps(body)],
                       capture_output=True, text=True)
    out = r.stdout.strip().rsplit("\n",1)
    print(f"{label}: HTTP {out[-1]}  {out[0][:150]}")

# (a) pays both in full -> GRANT
post(build(FUND[0], FUND[1], [(3000,K1),(2000,K2)]), "pays both in full     ")
# (b) one satoshi short for KID1 -> REFUSE
post(build(FUND[0], FUND[1], [(2999,K1),(2000,K2)]), "one satoshi short     ")
# (c) pays everyone but spends SOMEONE ELSE'S outpoint -> REFUSE
post(build("bb"*32, 0, [(3000,K1),(2000,K2)]), "wrong funding outpoint")
# (d) omits KID2 entirely -> REFUSE
post(build(FUND[0], FUND[1], [(3000,K1)]), "omits a leaf          ")

# ── [REQ-74] SELF-FUNDING ────────────────────────────────────────────────────────────────────────
# Mark KID2 released: it migrated, so it is NOT paid on chain and its 2000 is what the round
# RECOVERS. REQ-53 says that value funds the next root. These three cases are the difference
# between a claim and an invariant.
NEXT = "44" * 32
subprocess.run(["docker","exec","-i","mercurylayer-db_lockbox-1","psql","-U","postgres","-d","enclave",
                "-c", f"UPDATE se_leaf SET released = true WHERE statechain_id = '{KID2}'; "
                      f"DELETE FROM se_root WHERE root_statechain_id LIKE 'cg%';"],
               capture_output=True)

def post2(txhex, label, extra, agg=AGG33):
    # A 133-byte PLACEHOLDER session. Cases (b)-(d) never reach the bind — the predicate refuses
    # first — so a real MuSig2 session is not needed to exercise them, and the granted case reaching
    # the bind and refusing there is exactly what this probe asserts.
    body = {"root_statechain_id": ROOT,
            "session": "00"*133,
            "disclosure": {"unsigned_tx": txhex, "input_index":0,
                           "prevout_values":[6000], "prevout_spks":["5120"+"33"*32],
                           "agg_pubkey": agg, "agg_nonce":"00"*66,
                           "blinding_factor":"00"*32, "out_tweak":"00"*32, "hash_type":1}}
    body.update(extra)
    r = subprocess.run(["curl","-s","-w","\\n%{http_code}","-X","POST","http://localhost:18080/collapse_grant",
                        "-H","Content-Type: application/json","-d",json.dumps(body)],
                       capture_output=True, text=True)
    out = r.stdout.strip().rsplit("\n",1)
    print(f"{label}: HTTP {out[-1]}  {out[0][:170]}")

# (e) no next root named: still a valid collapse, but the round is NOT self-funding and says so.
post2(build(FUND[0], FUND[1], [(3000,K1),(3000,"99"*32)]), "released, no next root", {})
# (f) names a next root, funds it with LESS than recovered -> REFUSE.
post2(build(FUND[0], FUND[1], [(3000,K1),(1999,NEXT)]), "next root underfunded ", {"next_root_key": NEXT})
# (g) names a next root and funds it in full -> GRANT, self_funding true.
post2(build(FUND[0], FUND[1], [(3000,K1),(2000,NEXT)]), "next root funded fully", {"next_root_key": NEXT})

# (e) [REQ-68 / REQ-82] PAYS EVERYONE, WRONG COIN. Byte-identical to the granted case except the
#     disclosed aggregate. It must refuse BEFORE the root's secnonce is consumed — otherwise a
#     stranger could burn a root's nonce and leave it unable to sign its own collapse.
post(build(FUND[0], FUND[1], [(3000,K1),(2000,K2)]), "pays all, WRONG coin  ", agg="02"+"cd"*32)
