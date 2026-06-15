# eth-state-diff

A Rust CLI and library that shows exactly what an Ethereum transaction or
block changed in state: balances, nonces, code and storage slots. It reads
execution traces from geth's built-in `prestateTracer` in diff mode, turns
them into explicit before → after values, nets out a whole block, filters
the result and prints it as text, JSON, NDJSON or CSV.

```console
$ eth-state-diff file testdata/geth/store_call.json     # or: eth-state-diff tx <hash> --rpc <url>
== testdata/geth/store_call.json (1 transaction)
0x3a220f351252089d385b29beca14e27f204c296a  modified
  balance  0 ETH -> 0.000000000000000003 ETH  (+0.000000000000000003 ETH)
  storage  0x0: 0x0 -> 0x5
  storage  0x1: 0x7 -> 0x0
0x71562b71999873db5b286df957af199ec94617f7  modified
  balance  … -> …  (-0.000101317594351473 ETH)
  nonce    1 -> 2
summary: 2 accounts (0 created, 0 destroyed); 2 balance, 1 nonce, 0 code, 2 storage changes
```

The example output on this page is real. It comes from traces recorded
against a `geth --dev` node, which are also the test fixtures (the dev
account's balance, which is close to 2²⁵⁶ wei, is abbreviated with `…`).

## Why

Receipts and logs tell you what a contract *announced*. The state diff tells
you what actually *happened*: which slots were written, which accounts were
created or destroyed, and every wei that moved, including fees. Clients
expose this through `debug_traceTransaction` with
`{"tracer": "prestateTracer", "tracerConfig": {"diffMode": true}}`, but the
raw output is a compact encoding that is easy to misread:

- `post` contains **only the fields that changed**. A missing balance means
  "unchanged", not zero.
- An account that appears **only in `post`** was empty before (created).
- An account that appears **only in `pre`** was deleted (self-destructed).
- A storage slot in `pre` but **missing from `post`** was cleared to zero.
- A slot only in `post` was zero before.

`eth-state-diff` decodes those rules into explicit `from`/`to` pairs (taken
from geth's `processDiffState`). It also composes the per-transaction diffs
of a block into one net diff, so intermediate writes that cancel out
disappear.

## Install

```sh
cargo install --git https://github.com/Naveen-6087/x402
```

You need an RPC endpoint with the `debug` namespace (for example
`geth --http.api eth,debug`, or a provider that offers tracing), or a saved
trace file.

## Usage

```
eth-state-diff [OPTIONS] <COMMAND>

Commands:
  tx     Diff a single transaction via debug_traceTransaction
  block  Diff a block via debug_traceBlockByNumber/Hash (number, 0x-hash or tag)
  file   Diff a saved trace (a tracer result, a block trace array or a JSON-RPC response); `-` reads stdin
```

| Option | Meaning |
|--------|---------|
| `--rpc <URL>` (or `ETH_RPC_URL`) | Endpoint for `tx` and `block` |
| `--per-tx` | For `block`/`file`: one diff per transaction instead of the net diff |
| `-f, --format text\|json\|ndjson\|csv` | Output format (default `text`) |
| `--unit ether\|wei` | Balance units in text output |
| `--address <A>` / `--exclude <A>` | Keep only / hide accounts (repeatable) |
| `--kind balance\|nonce\|code\|storage` | Keep only these change kinds (repeatable) |
| `--slot <S>` | Keep only these storage slots (implies storage) |
| `--min-balance-delta <WEI>` | Hide balance changes smaller than this |
| `--no-code` | Ask the tracer to omit contract code (smaller responses) |
| `--timeout <SECS>` | RPC timeout (default 60) |
| `-o, --output <FILE>` | Write to a file instead of stdout |

Exit status: `0` success, `1` RPC, trace or I/O error, `2` invalid arguments.

### Examples

```sh
# Net effect of the latest block, as JSON
eth-state-diff block latest --rpc $ETH_RPC_URL -f json

# Every transaction in block 21000000 separately, storage writes only, as CSV
eth-state-diff block 21000000 --per-tx --kind storage -f csv --rpc $ETH_RPC_URL

# One contract's slot, from a trace you saved earlier
eth-state-diff file trace.json --address 0x3a22…296a --slot 0x1

# Pipe a raw JSON-RPC response straight in
curl -s $ETH_RPC_URL -H 'content-type: application/json' -d '{…debug_traceTransaction…}' | eth-state-diff file -
```

CSV (`--kind storage -f csv` on the call above):

```
tx,address,status,kind,slot,from,to,delta
,0x3a220f351252089d385b29beca14e27f204c296a,modified,storage,0x00…00,0x00…00,0x00…05,
,0x3a220f351252089d385b29beca14e27f204c296a,modified,storage,0x00…01,0x00…07,0x00…00,
```

JSON (`-f json --address 0x…cc` on a plain transfer to a fresh address):

```json
{
  "accounts": {
    "0x00000000000000000000000000000000000000cc": {
      "balance": { "delta": "+1000000000000000000", "from": "0", "to": "1000000000000000000" },
      "status": "created"
    }
  },
  "source": "testdata/geth/transfer.json",
  "summary": { "accounts": 1, "created": 1, "balance_changes": 1, "total_credited_wei": "1000000000000000000", "…": "…" },
  "transactions": 1
}
```

All numbers in JSON, NDJSON and CSV are exact decimal strings in wei. Slots
and storage values are full 32-byte words. Text output shortens words to
minimal hex.

## Library

```rust
use eth_state_diff::{Filter, Kind, StateDiff, parse_traces_str};

let traces = parse_traces_str(&json)?;                     // any supported shape
let per_tx: Vec<StateDiff> = traces.iter().map(|t| StateDiff::from_prestate(&t.diff)).collect();
let net = StateDiff::compose(&per_tx);                     // block-level net diff
let storage = Filter { kinds: [Kind::Storage].into(), ..Filter::default() }.apply(&net);
for (address, account) in &storage.accounts {
    for (slot, change) in &account.storage {
        println!("{address} {slot}: {} -> {}", change.from, change.to);
    }
}
```

## Architecture

```
 RPC (ureq) ──► debug_trace* ──┐
 file / stdin ─────────────────┴─► trace::parse_traces ─► diff::StateDiff::from_prestate
                                                              │
                                     diff::StateDiff::compose (block net diff)
                                                              │
                                                       filter::Filter
                                                              │
                                     render: text | json | ndjson | csv
```

| Module | Responsibility |
|--------|----------------|
| `primitives` | `Address`, `B256`, `Bytes`, a 256-bit `U256` with ether formatting, signed `Delta` |
| `trace` | Serde models of the tracer output; accepts bare results, block arrays and JSON-RPC envelopes; rejects non-diff-mode traces with a clear error |
| `diff` | Decoding geth's diff encoding into `Change { from, to }` per field; composition; summaries |
| `filter` | Address, kind, slot and balance-threshold filters |
| `render` | The four output formats |
| `rpc` | Minimal JSON-RPC client with helpful errors (for example a hint when the `debug` namespace is disabled) |

### Design decisions

- **Explicit before/after values.** Every change carries both ends, so
  consumers never need to know the tracer's encoding.
- **Composition instead of re-tracing.** A block diff is the in-order fold of
  its transaction diffs: the first `from` and the last `to` per field, with
  no-op changes dropped. Accounts created and destroyed within the block
  vanish.
- **Lenient input, strict output.** Quantities are accepted as hex strings,
  decimal strings or JSON numbers, since clients differ. Output always uses
  one canonical form per format.
- **No big-number dependency.** The 256-bit arithmetic needed (add, sub,
  decimal and ether formatting) is about 150 lines with tests.
- **Small dependency surface**: `serde`, `serde_json`, `clap`, `thiserror`,
  and `ureq` with only rustls and JSON enabled.

## Testing

```sh
cargo test                                         # unit, property, fixture and CLI tests
ETH_STATE_DIFF_REQUIRE_GETH=1 GETH=/path/to/geth cargo test   # also require the live-node tests
```

| Suite | Checks |
|-------|--------|
| `tests/geth.rs` (needs `geth`) | Starts `geth --dev` and runs four real transactions: deploying a contract, calling it (sets one slot, clears another, receives value), transferring to a fresh address, and creating plus self-destructing a contract in one transaction (EIP-6780). For **every** reported change, `from`/`to` must equal `eth_getBalance`, `eth_getTransactionCount`, `eth_getCode` and `eth_getStorageAt` at the blocks before and after. Also checks `debug_traceBlockByNumber`, the composed net diff across all four blocks against chain state, `disableCode`, and RPC errors |
| `tests/properties.rs` (proptest, 512 cases each) | Random worlds of accounts, each transition encoded exactly as geth's tracer would. Applying the decoded diff reproduces the next state, with every `from` checked along the way. Composing a chain of diffs equals the end-to-end diff |
| `tests/fixtures.rs` | Recorded geth traces still decode and render byte-for-byte as their golden `.txt` files |
| `tests/cli.rs` | Every subcommand, format and filter; stdin and `-o`; exit codes; `tx`/`block` against a live geth when available |
| Unit tests (25) | Parsing edge cases (RPC errors, failed transactions, non-diff traces, quantity variants), 256-bit arithmetic, decoding rules, composition, filters and renderers |

Without geth on `PATH` (or `GETH`), the live-node tests are skipped. CI
builds geth and requires them.

## Limitations

- Block diffs cover **transactions only**. Withdrawals, block rewards and
  system calls made outside transactions (EIP-4788 beacon roots, EIP-2935
  block hashes) are not traced by `debug_traceBlock*`, so they don't appear.
- The input must come from a `prestateTracer` in **diff mode** with geth's
  encoding. Other clients that follow it (Reth, Nethermind, Erigon) should
  work, but only geth is tested here.
- Addresses are printed in lowercase, without EIP-55 checksums.
- Pre-Cancun traces can contain fully destroyed accounts. They are decoded
  (`status: destroyed`, every field going to zero) and unit tested, but a
  post-Cancun dev chain cannot produce them, so the live tests don't cover
  them.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo +1.85 check --all-targets   # MSRV
cargo audit
```

`Cargo.lock` pins `time` 0.3.55 (it fixes RUSTSEC-2026-0009) even though
the MSRV-aware resolver would choose an older version. `time` is only
reachable through `ureq`'s cookie feature, which is disabled, so it is never
compiled.

## License

MIT, see [LICENSE](LICENSE).
