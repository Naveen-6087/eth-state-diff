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
    headers: Vec<(String, String)>,
    retries: u32,
    backoff: Duration,
    next_id: AtomicU64,
}

/// Configures an [`RpcClient`].
#[derive(Clone, Debug)]
pub struct RpcClientBuilder {
    url: String,
    timeout: Duration,
    headers: Vec<(String, String)>,
    retries: u32,
    backoff: Duration,
}

impl RpcClientBuilder {
    /// Timeout for each request, including retries individually.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Adds a header to every request, e.g. an API key for a hosted node.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Retries after connection failures and HTTP 429/502/503/504, with
    /// exponential backoff starting at `backoff`. JSON-RPC errors are never
    /// retried.
    pub fn retries(mut self, retries: u32, backoff: Duration) -> Self {
        self.retries = retries;
        self.backoff = backoff;
        self
    }

    pub fn build(self) -> RpcClient {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .build();
        RpcClient {
            url: self.url,
            agent: agent.into(),
            headers: self.headers,
            retries: self.retries,
            backoff: self.backoff,
            next_id: AtomicU64::new(1),
        }
    }
}

const RETRY_STATUS: [u16; 4] = [429, 502, 503, 504];

impl RpcClient {
    /// A client without retries or extra headers.
    pub fn new(url: &str, timeout: Duration) -> Self {
        Self::builder(url).timeout(timeout).build()
    }

    pub fn builder(url: &str) -> RpcClientBuilder {
        RpcClientBuilder {
            url: url.to_owned(),
            timeout: Duration::from_secs(60),
            headers: Vec::new(),
            retries: 0,
            backoff: Duration::from_millis(250),
        }
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut attempt = 0;
        let (status, text) = loop {
            match self.send(&body) {
                Ok((status, _)) if RETRY_STATUS.contains(&status) && attempt < self.retries => {}
                Err(_) if attempt < self.retries => {}
                other => break other?,
            }
            std::thread::sleep(self.backoff.saturating_mul(1 << attempt.min(16)));
            attempt += 1;
        };
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

    fn send(&self, body: &Value) -> Result<(u16, String), RpcError> {
        let transport = |e: ureq::Error| RpcError::Transport { url: self.url.clone(), source: Box::new(e) };
        let mut req = self.agent.post(&self.url);
        for (name, value) in &self.headers {
            req = req.header(name, value);
        }
        let mut resp = req.send_json(body).map_err(transport)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(transport)?;
        Ok((status, text))
    }

    pub fn trace_transaction(&self, hash: &B256, opts: TracerOptions) -> Result<TxTrace, RpcError> {
        let result = self.call("debug_traceTransaction", json!([hash.to_string(), opts.to_json()]))?;
        let mut traces = parse_traces(result)?;
        let mut trace = traces.pop().ok_or(TraceError::UnknownFormat)?;
        trace.tx_hash = Some(*hash);
        Ok(trace)
    }

    /// Traces every block in `from..=to`, in order, as one list of
    /// transactions. Compose them for the net change over the range.
    pub fn trace_range(&self, from: u64, to: u64, opts: TracerOptions) -> Result<Vec<TxTrace>, RpcError> {
        let mut traces = Vec::new();
        for n in from..=to {
            traces.extend(self.trace_block(&BlockId::Number(n), opts)?);
        }
        Ok(traces)
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

    /// Serves the given raw HTTP responses in order, one per connection,
    /// and returns the request heads it received.
    fn serve(responses: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for response in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let (mut head, mut len) = (String::new(), 0);
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                reader.read_exact(&mut vec![0; len]).unwrap();
                reader.get_mut().write_all(response.as_bytes()).unwrap();
                heads.push(head);
            }
            heads
        });
        (url, handle)
    }

    fn http(status: &str, body: &str) -> &'static str {
        format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
            .leak()
    }

    #[test]
    fn retries_transient_http_errors() {
        let ok = http("200 OK", r#"{"jsonrpc":"2.0","id":1,"result":"0x1"}"#);
        let (url, server) =
            serve(vec![http("503 Service Unavailable", ""), http("429 Too Many Requests", ""), ok]);
        let client = RpcClient::builder(&url)
            .timeout(Duration::from_secs(5))
            .retries(2, Duration::from_millis(1))
            .header("x-api-key", "secret")
            .build();
        assert_eq!(client.call("eth_chainId", json!([])).unwrap(), json!("0x1"));
        let heads = server.join().unwrap();
        assert_eq!(heads.len(), 3);
        assert!(heads.iter().all(|h| h.to_ascii_lowercase().contains("x-api-key: secret")));
    }

    #[test]
    fn gives_up_after_the_retry_budget() {
        let (url, server) =
            serve(vec![http("503 Service Unavailable", "busy"), http("503 Service Unavailable", "busy")]);
        let client = RpcClient::builder(&url).retries(1, Duration::from_millis(1)).build();
        assert!(
            matches!(client.call("eth_chainId", json!([])), Err(RpcError::Malformed(m)) if m.contains("503"))
        );
        server.join().unwrap();
    }

    #[test]
    fn json_rpc_errors_are_not_retried() {
        let err = http("200 OK", r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no debug"}}"#);
        let (url, server) = serve(vec![err]);
        let client = RpcClient::builder(&url).retries(3, Duration::from_millis(1)).build();
        assert!(matches!(
            client.call("debug_traceTransaction", json!([])),
            Err(RpcError::Rpc { code: -32601, .. })
        ));
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn unreachable_node_is_a_transport_error() {
        let client = RpcClient::new("http://127.0.0.1:1", Duration::from_secs(2));
        assert!(matches!(client.call("eth_chainId", json!([])), Err(RpcError::Transport { .. })));
    }
}
