#!/usr/bin/env python3
"""[REQ-83 / §6.0] PLANT-AND-RUN of the relay claims. Asks bitcoind, rather than reasoning.

§6.0 rests on three claims about Bitcoin policy, and they were marked UNPROVEN because reading
policy source is not the same as running against a node. This probe settles them.

MEASURED against Bitcoin Core 30.2 on regtest:

    v3 TRUC, funded 240 anchor, 0 fee            -> allowed=False  reason=min relay fee not met
    v2 legacy, funded 240 anchor, 0 fee          -> allowed=False  reason=min relay fee not met
    v3 TRUC, ZERO-value anchor (Spark's shape)   -> allowed=False  reason=dust
    PACKAGE [0-fee tail parent, paying child]    -> package_msg=success

WHAT EACH LINE ESTABLISHES.

1. Our shape is refused for the FEE, not for DUST. That is the whole point: the funded 240 anchor is
   at its own standardness threshold, so it is not dust, and the transaction's one permitted dust
   output is left free for the tail. Had the slot been occupied the rejection would read `dust`.
2. Spark's shape — a ZERO-value anchor — IS refused as `dust`. Their anchor is itself the one
   permitted dust output, so a sub-dust payload makes a second. This is why a sub-dust child kills a
   whole branch there, measured rather than asserted.
3. The package is ACCEPTED. A 0-fee parent carrying a tail relays when its child pays for it, which
   is exactly how §6.0 says a tail travels.

Run: python3 scripts/tail_relay_probe.py   (needs the regtest stack up)
"""
import json, subprocess

def cli(*a):
    r = subprocess.run(["docker","exec","rgb-lightning-node-bitcoind-1","bitcoin-cli","-regtest",
                        "-rpcuser=user","-rpcpassword=password",*a], capture_output=True, text=True)
    if r.returncode != 0:
        return {"__error__": r.stderr.strip()}
    t = r.stdout.strip()
    try: return json.loads(t)
    except Exception: return t

def ser_u32(n): return n.to_bytes(4,'little')
def ser_u64(n): return n.to_bytes(8,'little')
def vi(n):
    if n < 0xfd: return bytes([n])
    return b'\xfd'+n.to_bytes(2,'little')

P2A = bytes([0x51,0x02,0x4e,0x73])          # OP_1 <0x4e73>, the ephemeral anchor
TAIL_SAT   = 329                             # one under DUST_LIMIT
ANCHOR_SAT = 240                             # FUNDED — its own standardness threshold
FUND = TAIL_SAT + ANCHOR_SAT                 # so the spend pays ZERO fee

addr_tail = cli("getnewaddress","","bech32m")
spk_tail = bytes.fromhex(cli("getaddressinfo",addr_tail)["scriptPubKey"])

# A UTXO worth EXACTLY tail+anchor, so the spend below is 0-fee by construction.
fund_txid = cli("sendtoaddress",addr_tail,f"{FUND/1e8:.8f}")
cli("generatetoaddress","1",cli("getnewaddress"))
raw = cli("getrawtransaction",fund_txid,"true")
vout = next(o["n"] for o in raw["vout"] if int(round(o["value"]*1e8)) == FUND)

def build(version, anchor_sat):
    tx  = ser_u32(version) + vi(1)
    tx += bytes.fromhex(fund_txid)[::-1] + ser_u32(vout) + vi(0) + ser_u32(0xfffffffd)
    outs = [(TAIL_SAT, spk_tail), (anchor_sat, P2A)]
    tx += vi(len(outs))
    for val, spk in outs:
        tx += ser_u64(val) + vi(len(spk)) + spk
    return (tx + ser_u32(0)).hex()

for label, version, anchor in [
    ("v3 TRUC, funded 240 anchor, 0 fee", 3, ANCHOR_SAT),
    ("v2 legacy, funded 240 anchor, 0 fee", 2, ANCHOR_SAT),
    ("v3 TRUC, ZERO-value anchor (Spark's shape)", 3, 0),
]:
    unsigned = build(version, anchor)
    signed = cli("signrawtransactionwithwallet", unsigned)
    if isinstance(signed, dict) and signed.get("hex"):
        res = cli("testmempoolaccept", json.dumps([signed["hex"]]))
        r0 = res[0] if isinstance(res, list) and res else res
        print(f"{label:44} -> allowed={r0.get('allowed')} reason={r0.get('reject-reason','')}")
    else:
        print(f"{label:44} -> could not sign: {signed}")

# ---- THE PACKAGE. The parent is 0-fee by construction; the CHILD spends the tail and the anchor,
# adds an external fee input, and pays for both. This is the claim §6.0 actually rests on.
print()
parent_unsigned = build(3, ANCHOR_SAT)
parent = cli("signrawtransactionwithwallet", parent_unsigned)["hex"]
ptxid = cli("decoderawtransaction", parent)["txid"]

FEE_IN = 20_000
fee_addr = cli("getnewaddress","","bech32m")
fee_txid = cli("sendtoaddress", fee_addr, f"{FEE_IN/1e8:.8f}")
cli("generatetoaddress","1",cli("getnewaddress"))
fraw = cli("getrawtransaction", fee_txid, "true")
fvout = next(o["n"] for o in fraw["vout"] if int(round(o["value"]*1e8)) == FEE_IN)

change_addr = cli("getnewaddress","","bech32m")
change_spk = bytes.fromhex(cli("getaddressinfo", change_addr)["scriptPubKey"])
CHILD_OUT = TAIL_SAT + ANCHOR_SAT + FEE_IN - 2_000     # 2 000 sat pays for the whole package

child  = ser_u32(3) + vi(3)
for (txid, n) in [(ptxid, 0), (ptxid, 1), (fee_txid, fvout)]:
    child += bytes.fromhex(txid)[::-1] + ser_u32(n) + vi(0) + ser_u32(0xfffffffd)
child += vi(1) + ser_u64(CHILD_OUT) + vi(len(change_spk)) + change_spk
child += ser_u32(0)

prevtxs = json.dumps([
    {"txid": ptxid, "vout": 0, "scriptPubKey": spk_tail.hex(), "amount": TAIL_SAT/1e8},
    {"txid": ptxid, "vout": 1, "scriptPubKey": P2A.hex(),      "amount": ANCHOR_SAT/1e8},
])
signed_child = cli("signrawtransactionwithwallet", child.hex(), prevtxs)
if not isinstance(signed_child, dict) or not signed_child.get("hex"):
    print("child could not be signed:", signed_child)
else:
    res = cli("submitpackage", json.dumps([parent, signed_child["hex"]]))
    print("PACKAGE [0-fee tail parent, paying child] ->", json.dumps(res)[:400])
