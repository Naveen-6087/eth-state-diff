use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::primitives::B256;
use crate::trace::{TraceError, TxTrace, parse_traces};

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("request to {url} failed: {source}")]
    Transport { url: String, source: Box<ureq::Error> },
    #[error("JSON-RPC error {code}: {message}{hint}")]
    Rpc { code: i64, message: String, hint: &'static str },
    #[error("malformed JSON-RPC response: {0}")]
    Malformed(String),
    #[error(transparent)]
    Trace(#[from] TraceError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockId {
    Tag(&'static str),
    Number(u64),
    Hash(B256),
}

impl FromStr for BlockId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        const TAGS: [&str; 5] = ["latest", "earliest", "pending", "safe", "finalized"];
        if let Some(tag) = TAGS.iter().find(|t| **t == s) {
            return Ok(BlockId::Tag(tag));
        }
        if let Some(hex) = s.strip_prefix("0x") {
            if hex.len() == 64 {
                return s.parse().map(BlockId::Hash).map_err(|e| format!("{e}"));
            }
            return u64::from_str_radix(hex, 16)
                .map(BlockId::Number)
                .map_err(|_| format!("invalid block `{s}`"));
        }
        s.parse().map(BlockId::Number).map_err(|_| {
            format!(
                "invalid block `{s}`: use a number, a 0x-prefixed number or hash, or one of {}",
                TAGS.join(", ")
            )
        })
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockId::Tag(t) => f.write_str(t),
            BlockId::Number(n) => write!(f, "{n}"),
            BlockId::Hash(h) => write!(f, "{h}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TracerOptions {
    pub disable_code: bool,
    pub disable_storage: bool,
}

impl TracerOptions {
    pub fn to_json(self) -> Value {
        json!({
            "tracer": "prestateTracer",
            "tracerConfig": {
                "diffMode": true,
                "disableCode": self.disable_code,
                "disableStorage": self.disable_storage,
            },
        })
    }
}

/// A minimal blocking JSON-RPC client for the `debug` tracing endpoints.
pub struct RpcClient {
    url: String,
    agent: ureq::Agent,
    next_id: AtomicU64,
}

impl RpcClient {
    pub fn new(url: &str, timeout: Duration) -> Self {
        let agent =
            ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build();
        Self { url: url.to_owned(), agent: agent.into(), next_id: AtomicU64::new(1) }
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let transport = |e: ureq::Error| RpcError::Transport { url: self.url.clone(), source: Box::new(e) };
        let mut resp = self.agent.post(&self.url).send_json(&body).map_err(transport)?;
        let status = resp.status();
        let text = resp.body_mut().read_to_string().map_err(transport)?;
        let value: Value = serde_json::from_str(&text).map_err(|_| {
            let snippet: String = text.chars().take(200).collect();
            RpcError::Malformed(format!("HTTP {status}: {snippet}"))
        })?;
        if let Some(err) = value.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = err.get("message").and_then(Value::as_str).unwrap_or("").to_owned();
            let hint = match code {
                -32601 => {
                    " (the node does not expose the debug namespace; enable it, e.g. geth --http.api eth,debug)"
                }
                -32602 => {
                    " (the node rejected the tracer options; it may not support prestateTracer diffMode)"
                }
                _ => "",
            };
            return Err(RpcError::Rpc { code, message, hint });
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| RpcError::Malformed(format!("HTTP {status}: no result field")))
    }

    pub fn trace_transaction(&self, hash: &B256, opts: TracerOptions) -> Result<TxTrace, RpcError> {
        let result = self.call("debug_traceTransaction", json!([hash.to_string(), opts.to_json()]))?;
        let mut traces = parse_traces(result)?;
        let mut trace = traces.pop().ok_or(TraceError::UnknownFormat)?;
        trace.tx_hash = Some(*hash);
        Ok(trace)
    }

    pub fn trace_block(&self, block: &BlockId, opts: TracerOptions) -> Result<Vec<TxTrace>, RpcError> {
        let (method, id) = match block {
            BlockId::Hash(h) => ("debug_traceBlockByHash", json!(h.to_string())),
            BlockId::Number(n) => ("debug_traceBlockByNumber", json!(format!("0x{n:x}"))),
            BlockId::Tag(t) => ("debug_traceBlockByNumber", json!(t)),
        };
        let result = self.call(method, json!([id, opts.to_json()]))?;
        Ok(parse_traces(result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_ids() {
        assert_eq!("latest".parse(), Ok(BlockId::Tag("latest")));
        assert_eq!("12345".parse(), Ok(BlockId::Number(12345)));
        assert_eq!("0x10".parse(), Ok(BlockId::Number(16)));
        let hash = format!("0x{}", "ab".repeat(32));
        assert!(matches!(hash.parse::<BlockId>(), Ok(BlockId::Hash(_))));
        assert!("newest".parse::<BlockId>().is_err());
        assert!("0xzz".parse::<BlockId>().is_err());
    }

    #[test]
    fn tracer_config() {
        let v = TracerOptions { disable_code: true, disable_storage: false }.to_json();
        assert_eq!(v["tracer"], "prestateTracer");
        assert_eq!(v["tracerConfig"]["diffMode"], true);
        assert_eq!(v["tracerConfig"]["disableCode"], true);
    }

    #[test]
    fn unreachable_node_is_a_transport_error() {
        let client = RpcClient::new("http://127.0.0.1:1", Duration::from_secs(2));
        assert!(matches!(client.call("eth_chainId", json!([])), Err(RpcError::Transport { .. })));
    }
}
