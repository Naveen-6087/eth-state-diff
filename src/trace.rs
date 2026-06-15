use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::primitives::{Address, B256, Bytes, U256, de_quantity_u64};

/// One account as reported by geth's `prestateTracer` in diff mode. In `pre`
/// it is the state before the transaction (only accounts that changed); in
/// `post` only the fields that changed are present.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<U256>,
    #[serde(default, deserialize_with = "de_quantity_u64", skip_serializing_if = "Option::is_none")]
    pub nonce: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Bytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_hash: Option<B256>,
    #[serde(default, deserialize_with = "de_storage", skip_serializing_if = "Option::is_none")]
    pub storage: Option<BTreeMap<B256, B256>>,
}

fn de_storage<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<BTreeMap<B256, B256>>, D::Error> {
    let raw = Option::<BTreeMap<String, String>>::deserialize(d)?;
    raw.map(|m| {
        m.into_iter()
            .map(|(k, v)| Ok((B256::from_word(&k)?, B256::from_word(&v)?)))
            .collect::<Result<_, crate::primitives::ParseError>>()
            .map_err(serde::de::Error::custom)
    })
    .transpose()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrestateDiff {
    #[serde(default)]
    pub pre: BTreeMap<Address, AccountState>,
    #[serde(default)]
    pub post: BTreeMap<Address, AccountState>,
}

/// The diff of one transaction, with its hash when the source provides it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxTrace {
    pub tx_hash: Option<B256>,
    pub diff: PrestateDiff,
}

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("JSON-RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("transaction {tx} failed to trace: {message}")]
    TxFailed { tx: String, message: String },
    #[error(
        "unrecognized trace format: expected a prestateTracer diffMode result ({{\"pre\", \"post\"}}) or a list of them"
    )]
    UnknownFormat,
    #[error("trace is not in diff mode: rerun the tracer with tracerConfig.diffMode = true")]
    NotDiffMode,
}

fn is_diff(v: &Value) -> bool {
    v.as_object().is_some_and(|o| {
        (o.contains_key("pre") || o.contains_key("post")) && o.keys().all(|k| k == "pre" || k == "post")
    })
}

fn looks_like_plain_prestate(v: &Value) -> bool {
    v.as_object().is_some_and(|o| !o.is_empty() && o.keys().all(|k| k.parse::<Address>().is_ok()))
}

fn parse_diff(v: Value) -> Result<PrestateDiff, TraceError> {
    if is_diff(&v) {
        Ok(serde_json::from_value(v)?)
    } else if looks_like_plain_prestate(&v) {
        Err(TraceError::NotDiffMode)
    } else if v.as_object().is_some_and(|o| o.is_empty()) {
        Ok(PrestateDiff::default())
    } else {
        Err(TraceError::UnknownFormat)
    }
}

fn parse_block_item(item: Value) -> Result<TxTrace, TraceError> {
    let tx_hash = item.get("txHash").and_then(Value::as_str).map(B256::from_word).transpose().ok().flatten();
    if let Some(err) = item.get("error") {
        return Err(TraceError::TxFailed {
            tx: tx_hash.map_or_else(|| "?".into(), |h| h.to_string()),
            message: err.as_str().map_or_else(|| err.to_string(), str::to_owned),
        });
    }
    match item.get("result") {
        Some(result) => Ok(TxTrace { tx_hash, diff: parse_diff(result.clone())? }),
        None => Ok(TxTrace { tx_hash: None, diff: parse_diff(item)? }),
    }
}

/// Parses any of the shapes produced by `debug_traceTransaction` and
/// `debug_traceBlockBy*` with the `prestateTracer` in diff mode: a bare
/// result, an array of per-transaction results, or a full JSON-RPC response.
pub fn parse_traces(value: Value) -> Result<Vec<TxTrace>, TraceError> {
    if let Some(obj) = value.as_object() {
        if obj.contains_key("jsonrpc") || obj.contains_key("result") && obj.contains_key("id") {
            if let Some(err) = obj.get("error") {
                return Err(TraceError::Rpc {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err.get("message").and_then(Value::as_str).unwrap_or("").to_owned(),
                });
            }
            return parse_traces(obj.get("result").cloned().unwrap_or(Value::Null));
        }
    }
    match value {
        Value::Array(items) => items.into_iter().map(parse_block_item).collect(),
        other => Ok(vec![TxTrace { tx_hash: None, diff: parse_diff(other)? }]),
    }
}

pub fn parse_traces_str(s: &str) -> Result<Vec<TxTrace>, TraceError> {
    parse_traces(serde_json::from_str(s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TX: &str = r#"{
        "post": {
            "0x70997970c51812dc3a010c7d01b50e0d17dc79c8": {"balance": "0x21e19e0c9bab2400000"},
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266": {"balance": "0x21e19df85a3ae4b4c00", "nonce": 1}
        },
        "pre": {
            "0x70997970c51812dc3a010c7d01b50e0d17dc79c8": {"balance": "0x21e19e0c9bab2400000"},
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266": {"balance": "0x21e19e0c9bab2400000"}
        }
    }"#;

    #[test]
    fn parses_bare_result() {
        let traces = parse_traces_str(TX).unwrap();
        assert_eq!(traces.len(), 1);
        let diff = &traces[0].diff;
        let sender: Address = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266".parse().unwrap();
        assert_eq!(diff.post[&sender].nonce, Some(1));
        assert_eq!(diff.pre[&sender].nonce, None);
    }

    #[test]
    fn parses_rpc_envelope_and_block_arrays() {
        let rpc = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{TX}}}"#);
        assert_eq!(parse_traces_str(&rpc).unwrap().len(), 1);

        let hash = format!("0x{}", "ab".repeat(32));
        let block = format!(r#"[{{"txHash":"{hash}","result":{TX}}},{{"txHash":"{hash}","result":{{}}}}]"#);
        let traces = parse_traces_str(&block).unwrap();
        assert_eq!(traces.len(), 2);
        assert_eq!(traces[0].tx_hash.unwrap().to_string(), hash);
        assert!(traces[1].diff.pre.is_empty());
    }

    #[test]
    fn reports_errors() {
        let err = parse_traces_str(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#,
        );
        assert!(matches!(err, Err(TraceError::Rpc { code: -32601, .. })));
        let failed = parse_traces_str(r#"[{"txHash":"0x01","error":"execution timeout"}]"#);
        assert!(matches!(failed, Err(TraceError::TxFailed { .. })));
        let plain = parse_traces_str(r#"{"0x70997970c51812dc3a010c7d01b50e0d17dc79c8":{"balance":"0x1"}}"#);
        assert!(matches!(plain, Err(TraceError::NotDiffMode)));
        assert!(matches!(parse_traces_str(r#"{"calls":[]}"#), Err(TraceError::UnknownFormat)));
        assert!(matches!(parse_traces_str("not json"), Err(TraceError::Json(_))));
    }

    #[test]
    fn accepts_quantity_variants() {
        let s = r#"{"pre":{"0x70997970c51812dc3a010c7d01b50e0d17dc79c8":{"balance":"100","nonce":"0x5","storage":{"0x1":"0x2"}}}}"#;
        let t = parse_traces_str(s).unwrap();
        let acct = t[0].diff.pre.values().next().unwrap();
        assert_eq!(acct.nonce, Some(5));
        assert_eq!(acct.balance, Some(U256::from_u128(100)));
        let slot = B256::from_word("0x1").unwrap();
        assert_eq!(acct.storage.as_ref().unwrap()[&slot], B256::from_word("0x2").unwrap());
    }
}
