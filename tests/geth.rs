#![cfg(feature = "rpc")]

mod common;

use common::{Geth, fixtures_dir, update_fixtures};
use evm_state_diff::{
    AccountStatus, Address, B256, BlockId, Format, Report, StateDiff, TracerOptions, U256, Unit,
    diffs_from_traces, parse_traces, render,
};
use serde_json::{Value, json};

// Constructor (16 bytes) stores 7 in slot 1, then copies and returns the
// 13-byte runtime that follows it, which stores calldata[0..32] in slot 0
// and calldata[32..64] in slot 1.
const STORE_DEPLOY: &str = "0x6007600155600d8060106000396000f360003560005560203560015500";
const STORE_RUNTIME: &str = "0x60003560005560203560015500";
// Constructor that immediately self-destructs to 0x…bb (EIP-6780: deleted
// because it was created in the same transaction).
const BOMB_INIT: &str = "0x7300000000000000000000000000000000000000bbff";

fn word(n: u64) -> String {
    format!("{n:064x}")
}

fn hex_u256(v: &Value) -> U256 {
    U256::from_hex(v.as_str().unwrap()).unwrap()
}

/// Checks every change in `diff` against the node's state at `before` and `after`.
fn assert_matches_chain(geth: &Geth, diff: &StateDiff, before: u64, after: u64) {
    assert!(!diff.is_empty());
    for (addr, acct) in &diff.accounts {
        let a = addr.to_string();
        let at = |b: u64| format!("0x{b:x}");
        if let Some(c) = acct.balance {
            assert_eq!(
                hex_u256(&geth.rpc("eth_getBalance", json!([a, at(before)]))),
                c.from,
                "{a} balance before"
            );
            assert_eq!(
                hex_u256(&geth.rpc("eth_getBalance", json!([a, at(after)]))),
                c.to,
                "{a} balance after"
            );
        }
        if let Some(c) = acct.nonce {
            let n = |b| hex_u256(&geth.rpc("eth_getTransactionCount", json!([a, at(b)]))).to_string();
            assert_eq!(n(before), c.from.to_string(), "{a} nonce before");
            assert_eq!(n(after), c.to.to_string(), "{a} nonce after");
        }
        if let Some(c) = &acct.code {
            assert_eq!(
                geth.rpc("eth_getCode", json!([a, at(before)])),
                json!(c.from.to_string()),
                "{a} code before"
            );
            assert_eq!(
                geth.rpc("eth_getCode", json!([a, at(after)])),
                json!(c.to.to_string()),
                "{a} code after"
            );
        }
        for (slot, c) in &acct.storage {
            let s = |b| geth.rpc("eth_getStorageAt", json!([a, slot.to_string(), at(b)]));
            assert_eq!(s(before), json!(c.from.to_string()), "{a} slot {slot} before");
            assert_eq!(s(after), json!(c.to.to_string()), "{a} slot {slot} after");
        }
    }
}

fn block_number(receipt: &Value) -> u64 {
    u64::from_str_radix(receipt["blockNumber"].as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

fn traced(geth: &Geth, receipt: &Value, name: &str) -> (StateDiff, u64) {
    let hash: B256 = receipt["transactionHash"].as_str().unwrap().parse().unwrap();
    let raw =
        geth.rpc("debug_traceTransaction", json!([hash.to_string(), TracerOptions::default().to_json()]));
    let via_client = geth.client.trace_transaction(&hash, TracerOptions::default()).unwrap();
    let parsed = parse_traces(raw.clone()).unwrap();
    assert_eq!(parsed[0].diff, via_client.diff);

    if update_fixtures() {
        let dir = fixtures_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.json")), serde_json::to_string_pretty(&raw).unwrap() + "\n")
            .unwrap();
        let diff = StateDiff::from_prestate(&parsed[0].diff);
        let report = Report::Net { label: name.into(), transactions: 1, diff };
        let mut text = Vec::new();
        render(&report, Format::Text, Unit::Wei, &mut text).unwrap();
        std::fs::write(dir.join(format!("{name}.txt")), text).unwrap();
    }
    (StateDiff::from_prestate(&via_client.diff), block_number(receipt))
}

#[test]
fn diffs_match_chain_state() {
    let Some(geth) = Geth::start() else { return };
    let dev = geth.dev_account();
    let dev_addr: Address = dev.parse().unwrap();

    let deploy = geth.send(json!({"from": dev, "data": STORE_DEPLOY, "gas": "0x100000"}));
    let store: Address = deploy["contractAddress"].as_str().unwrap().parse().unwrap();
    let (d, b) = traced(&geth, &deploy, "deploy");
    let c = &d.accounts[&store];
    assert_eq!(c.status, AccountStatus::Created);
    assert_eq!(c.code.as_ref().unwrap().to.to_string(), STORE_RUNTIME);
    assert_eq!(c.nonce.unwrap().to, 1);
    assert_eq!(c.storage.len(), 1);
    assert_eq!(d.accounts[&dev_addr].nonce.unwrap().to, d.accounts[&dev_addr].nonce.unwrap().from + 1);
    assert_matches_chain(&geth, &d, b - 1, b);
    let first_block = b;

    let call_data = format!("0x{}{}", word(5), word(0));
    let call = geth.send(
        json!({"from": dev, "to": store.to_string(), "data": call_data, "value": "0x3", "gas": "0x100000"}),
    );
    let (d, b) = traced(&geth, &call, "store_call");
    let c = &d.accounts[&store];
    assert_eq!(c.status, AccountStatus::Modified);
    assert_eq!(c.balance.unwrap().to, U256::from_u128(3));
    let slot = |n| B256::from_word(&format!("0x{n:x}")).unwrap();
    assert_eq!(c.storage[&slot(0)].to, slot(5), "slot 0 set");
    assert_eq!(c.storage[&slot(1)].from, slot(7), "slot 1 was 7");
    assert!(c.storage[&slot(1)].to.is_zero(), "slot 1 cleared");
    assert_matches_chain(&geth, &d, b - 1, b);

    let fresh = "0x00000000000000000000000000000000000000cc";
    let transfer = geth.send(json!({"from": dev, "to": fresh, "value": "0xde0b6b3a7640000"}));
    let (d, b) = traced(&geth, &transfer, "transfer");
    let r = &d.accounts[&fresh.parse::<Address>().unwrap()];
    assert_eq!(r.status, AccountStatus::Created);
    assert_eq!(r.balance.unwrap().to.to_string(), "1000000000000000000");
    assert_matches_chain(&geth, &d, b - 1, b);

    let bomb = geth.send(json!({"from": dev, "data": BOMB_INIT, "value": "0x2", "gas": "0x100000"}));
    let (d, b) = traced(&geth, &bomb, "selfdestruct_in_constructor");
    let bomb_addr: Address = bomb["contractAddress"].as_str().unwrap().parse().unwrap();
    assert!(!d.accounts.contains_key(&bomb_addr), "created and destroyed in one tx leaves no trace");
    let beneficiary = &d.accounts[&"0x00000000000000000000000000000000000000bb".parse::<Address>().unwrap()];
    assert_eq!(beneficiary.balance.unwrap().to, U256::from_u128(2));
    assert_matches_chain(&geth, &d, b - 1, b);

    // Block-level tracing of the same blocks, and the net effect of all four.
    let mut per_tx = Vec::new();
    for n in first_block..=b {
        let traces = geth.client.trace_block(&BlockId::Number(n), TracerOptions::default()).unwrap();
        assert_eq!(traces.len(), 1);
        assert!(traces[0].tx_hash.is_some());
        per_tx.extend(diffs_from_traces(&traces));
    }
    let net = StateDiff::compose(per_tx.iter().map(|t| &t.diff));
    assert_matches_chain(&geth, &net, first_block - 1, b);
    let store_net = &net.accounts[&store];
    assert_eq!(store_net.status, AccountStatus::Created);
    assert!(!store_net.storage.contains_key(&slot(1)), "slot 1 went 0 -> 7 -> 0: no net change");
}

#[test]
fn disable_code_reports_code_hash_only() {
    let Some(geth) = Geth::start() else { return };
    let dev = geth.dev_account();
    let deploy = geth.send(json!({"from": dev, "data": STORE_DEPLOY, "gas": "0x100000"}));
    let hash: B256 = deploy["transactionHash"].as_str().unwrap().parse().unwrap();
    let opts = TracerOptions { disable_code: true, disable_storage: false };
    let trace = geth.client.trace_transaction(&hash, opts).unwrap();
    let d = StateDiff::from_prestate(&trace.diff);
    let store: Address = deploy["contractAddress"].as_str().unwrap().parse().unwrap();
    let c = &d.accounts[&store];
    assert!(c.code.is_none());
    assert!(c.code_hash.is_some(), "geth still reports the code hash");
}

#[test]
fn missing_transaction_is_an_rpc_error() {
    let Some(geth) = Geth::start() else { return };
    let err = geth.client.trace_transaction(&B256([0x11; 32]), TracerOptions::default()).unwrap_err();
    assert!(matches!(err, evm_state_diff::RpcError::Rpc { .. }), "{err}");
}
