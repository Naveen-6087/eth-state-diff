use std::io::{self, Write};

use serde_json::{Map, Value, json};

use crate::diff::{AccountDiff, AccountStatus, StateDiff, Summary, TxDiff};
use crate::primitives::{B256, Delta, U256, encode_hex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Format {
    #[default]
    Text,
    Json,
    Ndjson,
    Csv,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum Unit {
    #[default]
    Ether,
    Wei,
}

/// What to print: one net diff, or one diff per transaction.
#[derive(Clone, Debug)]
pub enum Report {
    Net { label: String, transactions: usize, diff: StateDiff },
    PerTx(Vec<TxDiff>),
}

pub fn render(report: &Report, format: Format, unit: Unit, out: &mut impl Write) -> io::Result<()> {
    match format {
        Format::Text => text(report, unit, out),
        Format::Json => {
            serde_json::to_writer_pretty(&mut *out, &json_report(report))?;
            writeln!(out)
        }
        Format::Ndjson => {
            for row in rows(report) {
                serde_json::to_writer(&mut *out, &row.to_json())?;
                writeln!(out)?;
            }
            Ok(())
        }
        Format::Csv => {
            writeln!(out, "tx,address,status,kind,slot,from,to,delta")?;
            for row in rows(report) {
                writeln!(
                    out,
                    "{},{},{},{},{},{},{},{}",
                    row.tx.unwrap_or_default(),
                    row.address,
                    row.status,
                    row.kind,
                    row.slot.unwrap_or_default(),
                    row.from,
                    row.to,
                    row.delta.unwrap_or_default()
                )?;
            }
            Ok(())
        }
    }
}

fn status_name(s: AccountStatus) -> &'static str {
    match s {
        AccountStatus::Created => "created",
        AccountStatus::Destroyed => "destroyed",
        AccountStatus::Modified => "modified",
    }
}

/// A 32-byte word as a minimal hex quantity, for readable storage output.
fn compact(word: &B256) -> String {
    U256::from_be_slice(&word.0).expect("32 bytes").to_hex_quantity()
}

fn amount(v: U256, unit: Unit) -> String {
    match unit {
        Unit::Ether => format!("{} ETH", v.format_ether()),
        Unit::Wei => format!("{v} wei"),
    }
}

fn signed(d: Delta, unit: Unit) -> String {
    match unit {
        Unit::Ether => format!("{} ETH", d.format_ether()),
        Unit::Wei => format!("{d} wei"),
    }
}

fn text(report: &Report, unit: Unit, out: &mut impl Write) -> io::Result<()> {
    match report {
        Report::Net { label, transactions, diff } => {
            let plural = if *transactions == 1 { "" } else { "s" };
            writeln!(out, "== {label} ({transactions} transaction{plural})")?;
            text_diff(diff, unit, out)
        }
        Report::PerTx(txs) => {
            for (i, tx) in txs.iter().enumerate() {
                if i > 0 {
                    writeln!(out)?;
                }
                match tx.tx_hash {
                    Some(h) => writeln!(out, "== tx {h}")?,
                    None => writeln!(out, "== tx #{i}")?,
                }
                text_diff(&tx.diff, unit, out)?;
            }
            Ok(())
        }
    }
}

fn text_diff(diff: &StateDiff, unit: Unit, out: &mut impl Write) -> io::Result<()> {
    if diff.is_empty() {
        return writeln!(out, "no state changes");
    }
    for (addr, acct) in &diff.accounts {
        writeln!(out, "{addr}  {}", status_name(acct.status))?;
        if let (Some(c), Some(d)) = (acct.balance, acct.balance_delta()) {
            writeln!(
                out,
                "  balance  {} -> {}  ({})",
                amount(c.from, unit),
                amount(c.to, unit),
                signed(d, unit)
            )?;
        }
        if let Some(c) = acct.nonce {
            writeln!(out, "  nonce    {} -> {}", c.from, c.to)?;
        }
        if let Some(c) = &acct.code {
            writeln!(out, "  code     {} bytes -> {} bytes", c.from.len(), c.to.len())?;
        } else if let Some(c) = acct.code_hash {
            writeln!(out, "  codehash {} -> {}", c.from, c.to)?;
        }
        for (slot, c) in &acct.storage {
            writeln!(out, "  storage  {}: {} -> {}", compact(slot), compact(&c.from), compact(&c.to))?;
        }
    }
    let s = diff.summary();
    writeln!(
        out,
        "summary: {} accounts ({} created, {} destroyed); {} balance, {} nonce, {} code, {} storage changes",
        s.accounts,
        s.created,
        s.destroyed,
        s.balance_changes,
        s.nonce_changes,
        s.code_changes,
        s.storage_changes
    )
}

fn summary_json(s: &Summary) -> Value {
    json!({
        "accounts": s.accounts,
        "created": s.created,
        "destroyed": s.destroyed,
        "balance_changes": s.balance_changes,
        "nonce_changes": s.nonce_changes,
        "code_changes": s.code_changes,
        "storage_changes": s.storage_changes,
        "total_credited_wei": s.total_credited.to_string(),
        "total_debited_wei": s.total_debited.to_string(),
    })
}

fn account_json(acct: &AccountDiff) -> Value {
    let mut o = Map::new();
    o.insert("status".into(), json!(status_name(acct.status)));
    if let (Some(c), Some(d)) = (acct.balance, acct.balance_delta()) {
        o.insert(
            "balance".into(),
            json!({"from": c.from.to_string(), "to": c.to.to_string(), "delta": d.to_string()}),
        );
    }
    if let Some(c) = acct.nonce {
        o.insert("nonce".into(), json!({"from": c.from, "to": c.to}));
    }
    if let Some(c) = &acct.code {
        o.insert(
            "code".into(),
            json!({"from": c.from.to_string(), "to": c.to.to_string(), "from_len": c.from.len(), "to_len": c.to.len()}),
        );
    }
    if let Some(c) = acct.code_hash {
        o.insert("code_hash".into(), json!({"from": c.from.to_string(), "to": c.to.to_string()}));
    }
    if !acct.storage.is_empty() {
        let slots: Map<String, Value> = acct
            .storage
            .iter()
            .map(|(slot, c)| (slot.to_string(), json!({"from": c.from.to_string(), "to": c.to.to_string()})))
            .collect();
        o.insert("storage".into(), Value::Object(slots));
    }
    Value::Object(o)
}

fn diff_json(diff: &StateDiff) -> Value {
    let accounts: Map<String, Value> =
        diff.accounts.iter().map(|(a, acct)| (a.to_string(), account_json(acct))).collect();
    json!({"summary": summary_json(&diff.summary()), "accounts": accounts})
}

fn json_report(report: &Report) -> Value {
    match report {
        Report::Net { label, transactions, diff } => {
            let mut v = diff_json(diff);
            v["source"] = json!(label);
            v["transactions"] = json!(transactions);
            v
        }
        Report::PerTx(txs) => json!({
            "transactions": txs.iter().map(|t| {
                let mut v = diff_json(&t.diff);
                v["tx_hash"] = t.tx_hash.map_or(Value::Null, |h| json!(h.to_string()));
                v
            }).collect::<Vec<_>>()
        }),
    }
}

/// One flattened change, for NDJSON and CSV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub tx: Option<String>,
    pub address: String,
    pub status: &'static str,
    pub kind: &'static str,
    pub slot: Option<String>,
    pub from: String,
    pub to: String,
    pub delta: Option<String>,
}

impl Row {
    fn to_json(&self) -> Value {
        let mut v = json!({
            "tx": self.tx,
            "address": self.address,
            "status": self.status,
            "kind": self.kind,
            "from": self.from,
            "to": self.to,
        });
        if let Some(slot) = &self.slot {
            v["slot"] = json!(slot);
        }
        if let Some(delta) = &self.delta {
            v["delta"] = json!(delta);
        }
        v
    }
}

fn diff_rows(tx: Option<String>, diff: &StateDiff, out: &mut Vec<Row>) {
    for (addr, acct) in &diff.accounts {
        let row = |kind, slot, from: String, to: String, delta| Row {
            tx: tx.clone(),
            address: addr.to_string(),
            status: status_name(acct.status),
            kind,
            slot,
            from,
            to,
            delta,
        };
        if let (Some(c), Some(d)) = (acct.balance, acct.balance_delta()) {
            out.push(row("balance", None, c.from.to_string(), c.to.to_string(), Some(d.to_string())));
        }
        if let Some(c) = acct.nonce {
            out.push(row("nonce", None, c.from.to_string(), c.to.to_string(), None));
        }
        if let Some(c) = &acct.code {
            out.push(row("code", None, encode_hex(&c.from.0), encode_hex(&c.to.0), None));
        }
        if let Some(c) = acct.code_hash {
            out.push(row("code_hash", None, c.from.to_string(), c.to.to_string(), None));
        }
        for (slot, c) in &acct.storage {
            out.push(row("storage", Some(slot.to_string()), c.from.to_string(), c.to.to_string(), None));
        }
        if !acct.has_changes() {
            out.push(row("account", None, String::new(), String::new(), None));
        }
    }
}

pub fn rows(report: &Report) -> Vec<Row> {
    let mut out = Vec::new();
    match report {
        Report::Net { diff, .. } => diff_rows(None, diff, &mut out),
        Report::PerTx(txs) => {
            for t in txs {
                diff_rows(t.tx_hash.map(|h| h.to_string()), &t.diff, &mut out);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::parse_traces_str;

    fn report() -> Report {
        let json = r#"{"pre":{
            "0x0101010101010101010101010101010101010101":{"balance":"0xde0b6b3a7640000","nonce":1,"storage":{"0x1":"0x7"}}},
          "post":{
            "0x0101010101010101010101010101010101010101":{"balance":"0x6f05b59d3b20000","nonce":2,"storage":{"0x2":"0x1"}},
            "0x0202020202020202020202020202020202020202":{"balance":"0x6f05b59d3b20000"}}}"#;
        let diff = StateDiff::from_prestate(&parse_traces_str(json).unwrap()[0].diff);
        Report::Net { label: "example".into(), transactions: 1, diff }
    }

    fn render_str(format: Format, unit: Unit) -> String {
        let mut buf = Vec::new();
        render(&report(), format, unit, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn text_output() {
        let s = render_str(Format::Text, Unit::Ether);
        assert!(s.starts_with("== example (1 transaction)\n"));
        assert!(s.contains("  balance  1 ETH -> 0.5 ETH  (-0.5 ETH)\n"));
        assert!(s.contains("  nonce    1 -> 2\n"));
        assert!(s.contains("  storage  0x1: 0x7 -> 0x0\n"));
        assert!(s.contains("  storage  0x2: 0x0 -> 0x1\n"));
        assert!(s.contains("0x0202020202020202020202020202020202020202  created\n"));
        assert!(s.ends_with(
            "summary: 2 accounts (1 created, 0 destroyed); 2 balance, 1 nonce, 0 code, 2 storage changes\n"
        ));
        assert!(render_str(Format::Text, Unit::Wei).contains("(-500000000000000000 wei)"));
    }

    #[test]
    fn json_output() {
        let v: Value = serde_json::from_str(&render_str(Format::Json, Unit::Ether)).unwrap();
        assert_eq!(v["transactions"], 1);
        assert_eq!(v["summary"]["accounts"], 2);
        assert_eq!(v["summary"]["total_credited_wei"], "500000000000000000");
        let a = &v["accounts"]["0x0101010101010101010101010101010101010101"];
        assert_eq!(a["balance"]["delta"], "-500000000000000000");
        assert_eq!(a["nonce"]["to"], 2);
        assert_eq!(
            a["storage"]["0x0000000000000000000000000000000000000000000000000000000000000001"]["to"],
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn ndjson_and_csv_rows() {
        let nd = render_str(Format::Ndjson, Unit::Ether);
        let lines: Vec<Value> = nd.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().all(|l| l["tx"].is_null()));
        let csv = render_str(Format::Csv, Unit::Ether);
        let mut it = csv.lines();
        assert_eq!(it.next(), Some("tx,address,status,kind,slot,from,to,delta"));
        assert_eq!(it.count(), 5);
        assert!(csv.contains(",balance,,1000000000000000000,500000000000000000,-500000000000000000"));
    }

    #[test]
    fn empty_diff() {
        let r = Report::PerTx(vec![TxDiff { tx_hash: None, diff: StateDiff::default() }]);
        let mut buf = Vec::new();
        render(&r, Format::Text, Unit::Ether, &mut buf).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "== tx #0\nno state changes\n");
    }
}
