# RGB test parity over Mercury statechain

This tracks how the upstream RGB test suites map onto our statechain-RGB implementation, so we know
RGB behaves over a Mercury statechain coin exactly as it does over a plain on-chain UTXO.

Upstream references:
- [`rgb-protocol/rgb-tests`](https://github.com/rgb-protocol/rgb-tests) @ `0b35fac` — the canonical
  rgb-lib integration suite (issuance, transfers, validation, reorg).
- [`UTEXO-Protocol/rgb-lightning-node`](https://github.com/UTEXO-Protocol/rgb-lightning-node) — RGB
  over Lightning (channels, swaps, HTLC).

## How RGB runs over our statechain

The RGB "witness UTXO" is a **statechain coin** (2-of-2 owner+SE, blind MuSig2). An asset is bound to
a coin with `fund_statechain` + `register_statechain`; transfers are assembled from the co-signed
colored-tx builders `create_colored_split_tx` (witness) and `create_colored_backup_tx` with a
`blinded` map (blinded), plus `color`/`color_blinded` in rgb-lib. Off-chain, the tx is not broadcast;
the receiver validates the exit branch with `validate_offchain_chain` and books the amount from the
consignment. This is a strict superset of on-chain RGB: the same consignment/validation machinery
runs, only the witness output lives on a statechain coin.

## Coverage summary

62 upstream tests classified: **13 MIRROR** (behave identically over a statechain coin, expressible
today), **23 ADAPT** (mirrorable but needing a bridge arg/getter or a reshaped assertion), **26 NA**
(pure rgb-lib/rgb-std engine internals, chain-net/AluVM surgery, Bitcoin-mempool RBF, tapret-descriptor
internals, or LN channels — identical whether the witness is a plain UTXO or a statechain coin).

## Implemented statechain-RGB tests

| Test | Mirrors (upstream) | What it proves over statechain |
|------|--------------------|--------------------------------|
| `rgb01` | single-recipient transfer | off-chain split of a colored coin (witness) |
| `rgb02` | multi-input transfer | combine N colored coins → recipient + change |
| `rgb03` | 2-hop transfer chain | 2-deep off-chain chain (split → combine), validated off-chain |
| `rgb04` | `rbf_transfer`, `same_transfer_twice*` (conflict) | SE refuses a 2nd conflicting spend of a node |
| `rgb05` / `rgb06` / `rgb08` | deep/wide transfer DAGs | combine-3, 3-level DAG, wide N-input combine |
| `rgb07` | bounded lifetime | SE epoch-deadline enforcement |
| **`rgb09`** | `transfer_loop`, `send_receive`, `validate_consignment_success` | **standard NIA transfer, BLINDED and WITNESS**, balance parity |
| **`rgb10`** | `check_fungible_history`, `send_to_oneself` | transfer history + self-transfer over statechain |
| **`rgb11`** | `issue_nia/uda/cfa/ifa` | all four schemas issue over statechain; CFA transfers like NIA |
| **`rgb12`** | `validate_consignment_unknown_tx`, `receive_from_unbroadcasted_transfer_to_blinded` | off-chain resolver rejects a consignment missing an ancestor witness |
| **`rgb13`** | `validate_consignment_*_fail` family (integrity) | receiver rejects a payload-tampered consignment and one presented against the wrong witness |
| **`rgb14`** | `issue_nia/cfa/ifa` (metadata), IFA supply invariants | full contract metadata (schema/ticker/name/precision/supply) is faithful; IFA `max = initial + inflation right`, and realizing the right raises circulating supply to the cap |
| `sdk02` | basic NIA flow | issue + off-chain token transfer via the SDK |
| `sdk09` | `issue_ifa`, `ifa_inflation`, multi-recipient | IFA issuance + on-chain mint (inflate) + batch transfer |

## Statechain-specific findings (from mirroring)

- **Blinded transfers work over statechain** (rgb09 LEG A). The receiver blind-receives onto its own
  statechain seal (a universal `blind_receive(None)` invoice — the contract arrives with the
  consignment), and the sender assigns via `create_colored_backup_tx` + `color_blinded`. This was
  previously unexercised end-to-end (all prior rgb tests used witness receive).
- **Transfer history differs on the sender side** (rgb10). The RECEIVER matches upstream exactly
  (`ReceiveWitness` of the sent amount at the split txid). The SENDER has **no** rgb-lib `Send`
  entry — that abstraction only exists in rgb-lib's high-level `send()`; our statechain transfers are
  composed from `color` + the sender's own `witness_receive` for change, so the sender records its
  change as `ReceiveWitness`. Balances and txids are fully conserved.
- **Off-chain resolver is safe against omitted ancestors** (rgb12): a consignment validates against
  its full un-broadcast ancestor set but is rejected (`valid=false`, "public witness … not known to
  the resolver") when an ancestor witness txid is withheld.

## NA — not mirrored, and why (representative)

1. **Engine-internal consignment/bundle/AluVM surgery** — `extra_known_transition`,
   `uncommitted_input_opout`, `concealed_known_transition`, `accept_bundle_missing_transitions`,
   `unordered_transitions_within_bundle`, `transition_spending_uncommitted_opout`, `extra_after_merge`,
   and the re-anchor tiers of the `validate_consignment_*` suite. These hand-craft transitions via raw
   rgb-std builders; validation is identical over a statechain coin, so mirroring adds no signal.
2. **Chain-net / deterministic-id / globals-order** — `issue_on_different_layers`,
   `deterministic_contract_id`, `contract_globals_order`: pure rgb-std hashing/ordering fixed at
   issuance, before any coin binding.
3. **Bitcoin-mempool mechanics** — `rbf_transfer`, `same_transfer_twice_*`: RBF / witness-update retry
   reuse the same coin, which our SE refuses as a double-spend (proven by `rgb04`); RGB-state validity
   is covered by `rgb01`/`rgb03`.
4. **Tapret-descriptor internals** — `tapret_commitments_on_beneficiary_output`,
   `validate_consignment_tapret_partner`, the tapret half of `tapret_opret_same_utxo`: our statechain
   deposits are opret-first (OP_RETURN); there is no user-managed tapret tweak set.
5. **Unexposed schema features** — `pfa`, `contract_linking`, `ifa_move_inflation_right`, `ifa_burn`:
   engine/schema-internal, orthogonal to statechain; would be ADAPT only if the bridge exposed them.
6. **Query-layer & Lightning** — `pagination_filters::*` (rgb-lib query cursors, statechain-invariant;
   off-chain transfers aren't broadcast so `listtransactions` is moot); LN channel/swap/HTLC tests
   from rgb-lightning-node are covered by our Lightning-latch swap tests (`sdk03`/`sdk05`/`sdk06`,
   `LN_SMOKE`) — RGB-over-statechain and RGB-over-LN are different rails.

## ADAPT — mirrorable next, and the bridge additions they need

Prioritised remaining work (each becomes an `rgbNN` when its bridge gap is filled):

- **Semantic consignment tamper-rejection** (`validate_consignment_chain_fail/genesis_fail/
  bundles_fail/commitments_fail/typesystem_fail`): `rgb13` already covers payload-integrity and
  wrong-witness rejection via byte-level tamper; the remaining field-level SEMANTIC cases (a
  well-formed consignment with a mutated `chain_net`/`schema_id`) need an in-crate rust-only tamper
  helper (load a `Transfer`, mutate the field, re-serialise) fed to `validate_offchain_chain`.
- **Multi-allocation / multi-asset per coin** (`multiple_transitions_per_vin`, `multiasset_transfer`,
  `invoice_reuse`): a multi-allocation register path + a `create_colored_multiasset_tx` builder.
- **Issuance-outpoint control** (`issue_nia_multiple_utxos`, `issue_cfa_multiple_utxos`): an
  issuance-outpoints arg to place each amount on a caller-chosen coin. (The asset-metadata getter is
  now done — `asset_metadata`, covered by `rgb14`.)
- **IFA over statechain deeper** (`ifa_zero_issuance_with_inflation`, `validate_consignment_ifa`): an
  explicit-outpoint `inflate` variant. (IFA supply getters — max/initial/circulating — are now done,
  covered by `rgb14`.)
- **Funding reorg** (`revert_genesis`, `reorg_history`): a resolver-switch harness + `get_witness_ord`
  surface.
- **UDA transfer**: the statechain deposit path is Fungible-only — `fund_statechain_utxo`
  (rgb-lib `rust_only.rs`, the `matches!(… Assignment::Fungible(amt) …)` source filter) and
  `register_statechain_utxo` (which books `Assignment::Fungible(rgb_amount)`) both need a
  `NonFungible` branch, `list_allocations` must surface non-fungible allocations, and the colored-tx
  builders' amount-based `output_map` must map to a token assignment rather than a u64 amount. A
  multi-function rgb-lib fork change with non-trivial rgb-std token/data semantics.
- **Collaborative transfer** (`collaborative_transfer`): a multi-owner `create_colored_combine_tx`
  where each input is co-signed by its own owner's blind-MuSig2.

Run the implemented tests with `RGB_E2E=1..12` on the regtest + Mercury (lockbox) + rgb-proxy stack.
