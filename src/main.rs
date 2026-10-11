use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use evm_state_diff::{
    Address, B256, BlockId, Filter, Format, Kind, Report, RpcClient, StateDiff, TracerOptions, TxDiff, U256,
    Unit, diffs_from_traces, parse_traces_str, render,
};

/// Ethereum execution state diffs from prestateTracer traces.
#[derive(Parser, Debug)]
#[command(name = "evm-state-diff", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    #[command(flatten)]
    output: OutputArgs,
    #[command(flatten)]
    filter: FilterArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Diff a single transaction via debug_traceTransaction.
    Tx {
        /// Transaction hash.
        hash: B256,
        #[command(flatten)]
        rpc: RpcArgs,
    },
    /// Diff a block via debug_traceBlockByNumber/Hash (number, 0x-hash or tag).
    Block {
        /// Block number, 0x-prefixed number or hash, or a tag (latest, safe, finalized, ...).
        block: BlockId,
        #[command(flatten)]
        rpc: RpcArgs,
        /// Print each transaction separately instead of the net block diff.
        #[arg(long)]
        per_tx: bool,
    },
    /// Net diff over a range of blocks (inclusive), via one debug_traceBlockByNumber per block.
    Range {
        /// First block number.
        from: u64,
        /// Last block number (inclusive).
        to: u64,
        #[command(flatten)]
        rpc: RpcArgs,
        /// Print each transaction separately instead of the net diff.
        #[arg(long)]
        per_tx: bool,
    },
    /// Diff a saved trace (a tracer result, a block trace array or a JSON-RPC response); `-` reads stdin.
    File {
        /// Path to the JSON trace, or `-` for stdin.
        path: PathBuf,
        /// Print each transaction separately instead of the net diff.
        #[arg(long)]
        per_tx: bool,
    },
}

#[derive(Args, Debug)]
struct RpcArgs {
    /// JSON-RPC endpoint exposing the debug namespace.
    #[arg(long, env = "ETH_RPC_URL")]
    rpc: String,
    /// Request timeout in seconds.
    #[arg(long, default_value_t = 60)]
    timeout: u64,
    /// Ask the tracer to omit contract code (smaller responses).
    #[arg(long)]
    no_code: bool,
    /// Retries after connection errors and HTTP 429/502/503/504.
    #[arg(long, default_value_t = 2)]
    retries: u32,
    /// Extra HTTP header for every request, as `Name: value` (repeatable).
    #[arg(long = "header", short = 'H', value_parser = parse_header)]
    headers: Vec<(String, String)>,
}

fn parse_header(s: &str) -> Result<(String, String), String> {
    let (name, value) =
        s.split_once(':').ok_or_else(|| format!("header `{s}` must look like `Name: value`"))?;
    let name = name.trim();
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(format!("invalid header name `{name}`"));
    }
    Ok((name.to_owned(), value.trim().to_owned()))
}

#[derive(Args, Debug)]
struct OutputArgs {
    /// Output format.
    #[arg(long, short, global = true, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Units for balances in text output.
    #[arg(long, global = true, value_enum, default_value_t = Unit::Ether)]
    unit: Unit,
    /// Write to a file instead of stdout.
    #[arg(long, short, global = true)]
    output: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct FilterArgs {
    /// Only show these accounts (repeatable).
    #[arg(long = "address", global = true)]
    addresses: Vec<Address>,
    /// Hide these accounts (repeatable).
    #[arg(long, global = true)]
    exclude: Vec<Address>,
    /// Only show these change kinds: balance, nonce, code, storage (repeatable).
    #[arg(long = "kind", global = true)]
    kinds: Vec<Kind>,
    /// Only show these storage slots (repeatable; implies storage changes only).
    #[arg(long = "slot", global = true, value_parser = parse_slot)]
    slots: Vec<B256>,
    /// Hide balance changes smaller than this many wei.
    #[arg(long, global = true)]
    min_balance_delta: Option<U256>,
}

fn parse_slot(s: &str) -> Result<B256, String> {
    B256::from_word(s).map_err(|e| e.to_string())
}

impl FilterArgs {
    fn build(&self) -> Filter {
        Filter {
            include: self.addresses.iter().copied().collect(),
            exclude: self.exclude.iter().copied().collect(),
            kinds: self.kinds.iter().copied().collect(),
            slots: self.slots.iter().copied().collect(),
            min_balance_delta: self.min_balance_delta,
        }
    }
}

fn client(args: &RpcArgs) -> (RpcClient, TracerOptions) {
    let opts = TracerOptions { disable_code: args.no_code, disable_storage: false };
    let mut builder = RpcClient::builder(&args.rpc)
        .timeout(Duration::from_secs(args.timeout))
        .retries(args.retries, Duration::from_millis(500));
    for (name, value) in &args.headers {
        builder = builder.header(name, value);
    }
    (builder.build(), opts)
}

fn build_report(diffs: Vec<TxDiff>, label: String, per_tx: bool, filter: &Filter) -> Report {
    if per_tx {
        Report::PerTx(
            diffs.into_iter().map(|t| TxDiff { tx_hash: t.tx_hash, diff: filter.apply(&t.diff) }).collect(),
        )
    } else {
        let net = StateDiff::compose(diffs.iter().map(|t| &t.diff));
        Report::Net { label, transactions: diffs.len(), diff: filter.apply(&net) }
    }
}

const MAX_RANGE: u64 = 10_000;

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let filter = cli.filter.build();
    let report = match &cli.command {
        Command::Tx { hash, rpc } => {
            let (client, opts) = client(rpc);
            let trace = client.trace_transaction(hash, opts)?;
            build_report(diffs_from_traces(&[trace]), format!("tx {hash}"), false, &filter)
        }
        Command::Block { block, rpc, per_tx } => {
            let (client, opts) = client(rpc);
            let traces = client.trace_block(block, opts)?;
            build_report(diffs_from_traces(&traces), format!("block {block}"), *per_tx, &filter)
        }
        Command::Range { from, to, rpc, per_tx } => {
            if from > to {
                return Err(format!("empty range: {from} > {to}").into());
            }
            if to - from >= MAX_RANGE {
                return Err(
                    format!("range of {} blocks exceeds the limit of {MAX_RANGE}", to - from + 1).into()
                );
            }
            let (client, opts) = client(rpc);
            let traces = client.trace_range(*from, *to, opts)?;
            build_report(diffs_from_traces(&traces), format!("blocks {from}..={to}"), *per_tx, &filter)
        }
        Command::File { path, per_tx } => {
            let mut text = String::new();
            if path.as_os_str() == "-" {
                io::stdin().read_to_string(&mut text)?;
            } else {
                File::open(path)
                    .map_err(|e| format!("{}: {e}", path.display()))?
                    .read_to_string(&mut text)?;
            }
            let traces = parse_traces_str(&text)?;
            build_report(diffs_from_traces(&traces), path.display().to_string(), *per_tx, &filter)
        }
    };

    let mut out: Box<dyn Write> = match &cli.output.output {
        Some(path) => {
            Box::new(BufWriter::new(File::create(path).map_err(|e| format!("{}: {e}", path.display()))?))
        }
        None => Box::new(BufWriter::new(io::stdout().lock())),
    };
    render(&report, cli.output.format, cli.output.unit, &mut out)?;
    out.flush()?;
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if e.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) {
                return ExitCode::SUCCESS;
            }
            eprintln!("evm-state-diff: {e}");
            ExitCode::FAILURE
        }
    }
}
