#![cfg(feature = "cli")]

mod common;

use std::io::Write;
use std::process::{Command, Output, Stdio};

use common::{Geth, fixtures_dir};
use serde_json::{Value, json};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_evm-state-diff"))
}

fn run(args: &[&str]) -> Output {
    bin().args(args).output().unwrap()
}

fn fixture(name: &str) -> String {
    fixtures_dir().join(format!("{name}.json")).to_string_lossy().into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn file_text_and_json() {
    let out = run(&["file", &fixture("store_call"), "--unit", "wei"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = stdout(&out);
    assert!(text.contains("  storage  0x1: 0x7 -> 0x0"));
    assert!(text.contains("  balance  0 wei -> 3 wei  (+3 wei)"));

    let out = run(&["file", &fixture("store_call"), "--format", "json"]);
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["transactions"], 1);
    assert_eq!(v["summary"]["storage_changes"], 2);
}

#[test]
fn filters_and_row_formats() {
    let out = run(&["file", &fixture("store_call"), "-f", "ndjson", "--kind", "storage"]);
    let rows: Vec<Value> = stdout(&out).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r["kind"] == "storage"));

    let out = run(&["file", &fixture("store_call"), "-f", "csv", "--slot", "0x1"]);
    let csv = stdout(&out);
    assert_eq!(csv.lines().count(), 2, "{csv}");
    assert!(
        csv.lines()
            .nth(1)
            .unwrap()
            .contains(",storage,0x0000000000000000000000000000000000000000000000000000000000000001,")
    );

    let out =
        run(&["file", &fixture("transfer"), "-f", "json", "--min-balance-delta", "1000000000000000000"]);
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let accounts = v["accounts"].as_object().unwrap();
    assert!(accounts.contains_key("0x00000000000000000000000000000000000000cc"));

    let out = run(&["file", &fixture("transfer"), "--exclude", "0x00000000000000000000000000000000000000cc"]);
    assert!(!stdout(&out).contains("0x00000000000000000000000000000000000000cc"));
}

#[test]
fn reads_stdin_and_writes_files() {
    let mut child =
        bin().args(["file", "-", "-f", "json"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(std::fs::read(fixture("deploy")).unwrap().as_slice()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap()["summary"]["created"], 1);

    let target = std::env::temp_dir().join(format!("evm-state-diff-cli-{}.csv", std::process::id()));
    let out = run(&["file", &fixture("deploy"), "-f", "csv", "-o", &target.to_string_lossy()]);
    assert!(out.status.success());
    assert!(
        std::fs::read_to_string(&target).unwrap().starts_with("tx,address,status,kind,slot,from,to,delta\n")
    );
    std::fs::remove_file(target).unwrap();
}

#[test]
fn errors_and_exit_codes() {
    let out = run(&["file", "does-not-exist.json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does-not-exist.json"));

    let not_diff = std::env::temp_dir().join(format!("evm-state-diff-plain-{}.json", std::process::id()));
    std::fs::write(&not_diff, r#"{"0x00000000000000000000000000000000000000aa":{"balance":"0x1"}}"#).unwrap();
    let out = run(&["file", &not_diff.to_string_lossy()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("diffMode"));
    std::fs::remove_file(not_diff).unwrap();

    assert_eq!(run(&["file", &fixture("deploy"), "--address", "0x12"]).status.code(), Some(2));
    assert_eq!(run(&["file", &fixture("deploy"), "--kind", "gas"]).status.code(), Some(2));
    assert_eq!(run(&["block", "newest", "--rpc", "http://127.0.0.1:1"]).status.code(), Some(2));
    assert_eq!(run(&["tx"]).status.code(), Some(2));

    let unreachable = format!("0x{}", "ab".repeat(32));
    let out = run(&["tx", &unreachable, "--rpc", "http://127.0.0.1:1", "--timeout", "2"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn rpc_commands_against_geth() {
    let Some(geth) = Geth::start() else { return };
    let dev = geth.dev_account();
    let receipt =
        geth.send(json!({"from": dev, "to": "0x00000000000000000000000000000000000000dd", "value": "0x5"}));
    let hash = receipt["transactionHash"].as_str().unwrap();
    let block = receipt["blockNumber"].as_str().unwrap();

    let out = run(&["tx", hash, "--rpc", &geth.url, "-f", "json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let tx: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(tx["accounts"]["0x00000000000000000000000000000000000000dd"]["balance"]["to"], "5");

    let out = run(&["block", block, "--rpc", &geth.url, "-f", "json"]);
    let net: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(net["accounts"], tx["accounts"]);

    let out = run(&["block", "latest", "--rpc", &geth.url, "--per-tx", "-f", "json"]);
    let per_tx: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(per_tx["transactions"][0]["tx_hash"], hash);
}
