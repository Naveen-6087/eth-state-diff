use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use evm_state_diff::RpcClient;
use serde_json::{Value, json};

/// A throwaway `geth --dev` node, killed on drop.
pub struct Geth {
    child: Child,
    datadir: PathBuf,
    pub url: String,
    pub client: RpcClient,
}

fn geth_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GETH") {
        return Some(PathBuf::from(path));
    }
    let name = if cfg!(windows) { "geth.exe" } else { "geth" };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Geth {
    /// Starts geth, or returns `None` (panicking if `ETH_STATE_DIFF_REQUIRE_GETH=1`)
    /// when no binary is available.
    pub fn start() -> Option<Geth> {
        let Some(bin) = geth_binary() else {
            if std::env::var("ETH_STATE_DIFF_REQUIRE_GETH").as_deref() == Ok("1") {
                panic!("geth not found: set GETH or put geth on PATH");
            }
            eprintln!("skipping: geth not found (set GETH or put geth on PATH)");
            return None;
        };
        let port = free_port();
        let datadir = std::env::temp_dir().join(format!("evm-state-diff-geth-{}-{port}", std::process::id()));
        let child = Command::new(bin)
            .args(["--dev", "--dev.period", "0", "--http", "--http.addr", "127.0.0.1"])
            .args(["--http.port", &port.to_string(), "--http.api", "eth,debug,net,web3"])
            .args(["--ipcdisable", "--nodiscover", "--verbosity", "1", "--datadir"])
            .arg(&datadir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start geth");
        let url = format!("http://127.0.0.1:{port}");
        let geth = Geth { child, datadir, client: RpcClient::new(&url, Duration::from_secs(30)), url };
        let deadline = Instant::now() + Duration::from_secs(60);
        while geth.client.call("eth_chainId", json!([])).is_err() {
            assert!(Instant::now() < deadline, "geth did not start");
            std::thread::sleep(Duration::from_millis(200));
        }
        Some(geth)
    }

    pub fn rpc(&self, method: &str, params: Value) -> Value {
        self.client.call(method, params).unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    pub fn dev_account(&self) -> String {
        self.rpc("eth_accounts", json!([]))[0].as_str().unwrap().to_owned()
    }

    /// Sends a transaction from the unlocked dev account and waits for its receipt.
    pub fn send(&self, tx: Value) -> Value {
        let hash = self.rpc("eth_sendTransaction", json!([tx]));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let receipt = match self.client.call("eth_getTransactionReceipt", json!([hash])) {
                Ok(r) => r,
                // geth answers this way while its transaction indexer starts up.
                Err(e) if e.to_string().contains("indexing is in progress") => Value::Null,
                Err(e) => panic!("eth_getTransactionReceipt: {e}"),
            };
            if !receipt.is_null() {
                assert_eq!(receipt["status"], "0x1", "transaction reverted: {receipt}");
                return receipt;
            }
            assert!(Instant::now() < deadline, "transaction {hash} was not mined");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Geth {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.datadir);
    }
}
