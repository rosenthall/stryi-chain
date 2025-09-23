use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "stryi-devkit", version, about = "Dev utilities for StryiChain")]
struct Cli {
    #[command(subcommand)]
    cmd: DevkitCommand,
}

#[derive(Subcommand)]
enum DevkitCommand {
    /// Generate a long chain for sync/IBD tests
    /// May generate configured chain with custom balances, difficulty, etc.
    Chaingen(ChaingenArgs),

    /// Push tx load via HTTP /api/tx
    Loadgen(LoadgenArgs),
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Mode {
    Offline,
    Live,
}

#[derive(Parser)]
struct ChaingenArgs {
    #[arg(long, value_enum, default_value_t = Mode::Offline)]
    mode: Mode,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, default_value_t = 1000)]
    blocks: u64,
    #[arg(long, default_value_t = 1)]
    difficulty: u8,
    #[arg(long)]
    seed: Option<u64>,
    #[arg(long, value_name = "@addr=amount", num_args = 0.., value_delimiter = ',')]
    balances: Vec<String>,
    #[arg(long, default_value_t = 1)]
    version: u16,
    #[arg(long, default_value_t = 1)]
    timestamp_step: u64,
}

#[derive(Parser)]
struct LoadgenArgs {
    #[arg(long, default_value = "http://127.0.0.1:7001")]
    node: String,
    #[arg(long, default_value_t = 5.0)]
    rate: f32,
    #[arg(long, default_value_t = 100)]
    count: u64,
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        DevkitCommand::Chaingen(args) => {
            eprintln!(
                "chaingen: mode={:?} blocks={} out={:?}",
                args.mode, args.blocks, args.out
            );
            todo!("implement chaingen (offline first)");
        }
        DevkitCommand::Loadgen(args) => {
            eprintln!(
                "loadgen: node={} rate={} count={}",
                args.node, args.rate, args.count
            );
            todo!("implement loadgen (tx -> /api/tx)");
        }
    }
}
