# Test data

`geth/*.json` are raw `debug_traceTransaction` results (prestateTracer,
`diffMode: true`) recorded from a `geth --dev` node (v1.17.8) by
`tests/geth.rs`. `geth/*.txt` are the matching text reports (`--unit wei`),
used as golden files by `tests/fixtures.rs`.

| Fixture | Transaction |
|---------|-------------|
| `deploy` | Deploys a small contract whose constructor stores 7 in slot 1 |
| `store_call` | Calls it with 3 wei: slot 0 set to 5, slot 1 cleared |
| `transfer` | Sends 1 ETH to a fresh address |
| `selfdestruct_in_constructor` | A contract created and self-destructed in one transaction (EIP-6780), forwarding 2 wei |

To re-record them (the dev account and contract addresses change each time):

```sh
ETH_STATE_DIFF_UPDATE_FIXTURES=1 ETH_STATE_DIFF_REQUIRE_GETH=1 cargo test --test geth
```
