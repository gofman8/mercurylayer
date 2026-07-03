//! Shared helper: dump the full **standard rgb-lib** view of a wallet/asset at a point in a flow.
//!
//! Used by every RGB e2e test so each step shows exactly what rgb-lib's standard methods report -
//! `get_asset_balance` (settled/future/spendable), `list_unspents` (with colorable/exists/allocations/
//! pending_blinded per UTXO), `list_allocations`, and `list_transfers` (kind/status/amount/txid).
//! This makes the on-chain-vs-statechain behavior auditable directly from the test output.

use mercury_rgb::RgbWallet;

/// Print the full rgb-lib state for `asset_id` in `rgb`, prefixed with `tag` (e.g. "sender before").
pub fn dump(tag: &str, rgb: &mut RgbWallet, asset_id: &str) {
    tokio::task::block_in_place(|| {
        println!("  --- rgb-lib state [{tag}] (asset {asset_id}) ---");
        match rgb.balance(asset_id) {
            Ok((s, f, sp)) => {
                println!("    get_asset_balance -> settled={s} future={f} spendable={sp}")
            }
            Err(e) => println!("    get_asset_balance -> <{e}>"),
        }
        match rgb.unspents_dump() {
            Ok(us) if !us.is_empty() => {
                for (op, sat, color, exists, nalloc, pending) in us {
                    println!(
                        "    list_unspents -> {op} sats={sat} colorable={color} exists={exists} allocations={nalloc} pending_blinded={pending}"
                    );
                }
            }
            Ok(_) => println!("    list_unspents -> (none)"),
            Err(e) => println!("    list_unspents -> <{e}>"),
        }
        if let Ok(a) = rgb.list_allocations(asset_id) {
            if a.is_empty() {
                println!("    list_allocations -> (none)");
            }
            for (op, amt, settled) in a {
                println!("    list_allocations -> {op} amount={amt} settled={settled}");
            }
        }
        if let Ok(ts) = rgb.transfers(asset_id) {
            if ts.is_empty() {
                println!("    list_transfers -> (none)");
            }
            for (kind, status, amt, txid) in ts {
                println!("    list_transfers -> kind={kind} status={status} amount={amt} txid={txid}");
            }
        }
    });
}
