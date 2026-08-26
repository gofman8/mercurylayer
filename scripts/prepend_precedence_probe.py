#!/usr/bin/env python3
"""[REQ-77 / §5.4] PLANT-AND-RUN of the prepend's precedence claim.

REQ-77 says a zero-timelock PREPEND "fires ahead of every rival state in existence, all of which
carry a non-zero timelock, so after it confirms every prior rival is spending an output that no
longer exists". That is a claim about BIP-68 and about what a node does — §0.2 says a source scan
can never establish it. So this asks bitcoind.

The experiment, on one funding output:
  1. a RIVAL spend carrying a relative timelock (nSequence = 10 blocks), as every superseded state does
  2. a PREPEND spend at sequence 0 — no relative lock at all
  3. broadcast the prepend, mine it, then re-offer the rival

Run: python3 scripts/prepend_precedence_probe.py   (needs the regtest stack up)
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

def u32(n): return n.to_bytes(4,'little')
def u64(n): return n.to_bytes(8,'little')
def vi(n):
    if n < 0xfd: return bytes([n])
    return b'\xfd'+n.to_bytes(2,'little')

FUND = 50_000
addr = cli("getnewaddress","","bech32m")
spk  = bytes.fromhex(cli("getaddressinfo", addr)["scriptPubKey"])
ftxid = cli("sendtoaddress", addr, f"{FUND/1e8:.8f}")
cli("generatetoaddress","1",cli("getnewaddress"))
raw = cli("getrawtransaction", ftxid, "true")
vout = next(o["n"] for o in raw["vout"] if int(round(o["value"]*1e8)) == FUND)

out_addr = cli("getnewaddress","","bech32m")
out_spk  = bytes.fromhex(cli("getaddressinfo", out_addr)["scriptPubKey"])

def spend(sequence):
    tx  = u32(2) + vi(1)
    tx += bytes.fromhex(ftxid)[::-1] + u32(vout) + vi(0) + u32(sequence)
    tx += vi(1) + u64(FUND - 1_000) + vi(len(out_spk)) + out_spk
    return (tx + u32(0)).hex()

# A superseded state: BIP-68 relative lock of 10 blocks (bit 22 clear = block-based).
RIVAL_SEQ   = 10
# The prepend: NO relative lock. Bit 31 set disables BIP-68 entirely.
PREPEND_SEQ = 0xfffffffd

rival   = cli("signrawtransactionwithwallet", spend(RIVAL_SEQ))
prepend = cli("signrawtransactionwithwallet", spend(PREPEND_SEQ))

r = cli("testmempoolaccept", json.dumps([rival["hex"]]))[0]
print(f"RIVAL   (nSequence={RIVAL_SEQ}, relative lock)  -> allowed={r.get('allowed')} reason={r.get('reject-reason','')}")
p = cli("testmempoolaccept", json.dumps([prepend["hex"]]))[0]
print(f"PREPEND (nSequence=0x{PREPEND_SEQ:08x}, no lock) -> allowed={p.get('allowed')} reason={p.get('reject-reason','')}")

# 3. The prepend confirms. What is the rival now?
sent = cli("sendrawtransaction", prepend["hex"])
cli("generatetoaddress","1",cli("getnewaddress"))
after = cli("testmempoolaccept", json.dumps([rival["hex"]]))[0]
print(f"RIVAL after the prepend confirmed        -> allowed={after.get('allowed')} reason={after.get('reject-reason','')}")
