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

  (a) pays both leaves in full         -> 200, granted, root frozen
  (b) one satoshi short for one leaf   -> 403 from the PAYMENT gate, naming the key and the shortfall
  (c) pays everyone, wrong funding tx  -> 403 from the OUTPOINT gate. A `C` that pays every owed key
                                          out of SOMEBODY ELSE'S money still is not a collapse of
                                          this root, and this case is what proves the two gates are
                                          independent rather than one standing in for the other
  (d) omits a leaf entirely            -> 403 from the PAYMENT gate, naming the unpaid key

Expected:
    pays both in full     : HTTP 200
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
"""
subprocess.run(["docker","exec","-i","mercurylayer-db_lockbox-1","psql","-U","postgres","-d","enclave","-c",sql],
               capture_output=True)

def post(txhex, label):
    body = {"root_statechain_id": ROOT,
            "disclosure": {"unsigned_tx": txhex, "input_index":0,
                           "prevout_values":[6000], "prevout_spks":["5120"+"33"*32],
                           "agg_pubkey":"33"*32, "agg_nonce":"00"*66,
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
