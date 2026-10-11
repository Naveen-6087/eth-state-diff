# Changelog

## 0.1.0

First release on crates.io, as `evm-state-diff`.

- Decode geth `prestateTracer` diff-mode traces into explicit before/after
  changes for balances, nonces, code and storage, including created and
  destroyed accounts.
- Compose per-transaction diffs into a net diff for a block or a range of
  blocks.
- Filter by account, change kind, storage slot and minimum balance delta.
- Render as text, JSON, NDJSON or CSV.
- `RpcClient` (feature `rpc`): `debug_traceTransaction`,
  `debug_traceBlockByNumber/Hash` and block ranges, with retries on
  transient failures and custom headers for authenticated providers.
- `evm-state-diff` CLI (feature `cli`, default): `tx`, `block`, `range` and
  `file` commands.
- Optional features keep the library down to `serde`, `serde_json` and
  `thiserror`.
