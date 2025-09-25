#![allow(incomplete_features)]
#![feature(generic_const_exprs)]

/// Implementation of ChainGen tool.
mod chaingen;
/// Exit codes used by the CLI.
mod codes;
/// Helpers for resolving paths.
mod path_resolve;

use crate::chaingen::ChainGenerator;
use crate::codes::{EXIT_CONFIG, EXIT_IO_OR_PARSE, EXIT_OK, EXIT_UNKNOWN};
use crate::path_resolve::{config_base_dir, resolve_relative};
use clap::{Parser, Subcommand};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Command-line interface definition.
#[derive(Parser, Debug)]
#[command(
    name = "stryi-devkit",
    version,
    about = "StryiChain DevKit",
    author = "github.com/rosenthall"
)]
struct DevKitCli {
    #[command(subcommand)]
    cmd: DevkitCommand,
}

#[derive(Subcommand, Debug)]
enum DevkitCommand {
    /// generate a long chain for sync/IBD tests
    Chaingen {
        // Path to a TOML config for ChainGen.
        #[arg(long = "config-path", value_name = "FILE", required = true)]
        config_path: PathBuf,
    },

    /// generate http load for node's api.
    Loadgen {
        // Kept for interface compatibility.
        #[arg(long = "config-path", value_name = "FILE", required = true)]
        config_path: PathBuf,
    },
}

fn run_loadgen(_config_path: PathBuf) -> i32 {
    panic!("loadgen is not implemented yet");
}

async fn run_chaingen(config_path: PathBuf) -> Result<(), i32> {
    // read the config
    let mut config = {
        if !config_path.is_file() {
            eprintln!(
                "Provided path to config is not a file: {}",
                config_path.display()
            );
            return Err(EXIT_IO_OR_PARSE);
        }

        let mut buf = String::new();
        let mut file = File::open(&config_path).map_err(|e| {
            eprintln!("Cannot open config at path. Error : {e}");
            EXIT_IO_OR_PARSE
        })?;
        file.read_to_string(&mut buf).map_err(|e| {
            eprintln!("Cannot read config at path. Error : {e}");
            EXIT_IO_OR_PARSE
        })?;

        match toml::from_str::<chaingen::ChainGenConfig>(&buf) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("Error while deserializing ChainGenConfig. Error : {e}");
                return Err(EXIT_IO_OR_PARSE);
            }
        }
    };

    // resolve paths relative to the TOML file location
    let base = config_base_dir(&config_path);
    config.chain.genesis_path = resolve_relative(&base, &config.chain.genesis_path);
    config.chain.output_path = resolve_relative(&base, &config.chain.output_path);

    println!("Successfully read the config from file.");

    // validate
    match config.validate() {
        Ok(()) => println!("Successfully validated the config."),
        Err(e) => {
            eprintln!("Invalid ChainGenConfig value: {e}");
            return Err(EXIT_CONFIG);
        }
    };
    println!("Successfully read and deserialized the config.");

    // initialize and run
    let chaingen = ChainGenerator::initialize(config).await.map_err(|e| {
        eprintln!("Failed to initialize ChainGenerator. Error: {e}");
        EXIT_IO_OR_PARSE
    })?;

    chaingen.start().await.map_err(|e| {
        eprintln!("Failed during ChainGenerator run. Error: {e}");
        EXIT_UNKNOWN
    })
}

#[tokio::main]
async fn main() -> Result<(), i32> {
    // Set default log level to info if RUST_LOG is not set
    // and initialize tracing subscriber with environment filter and formatting layer.
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_target(true).with_level(true))
        .init();

    let cli = DevKitCli::parse();

    let code = match cli.cmd {
        DevkitCommand::Chaingen { config_path } => match run_chaingen(config_path).await {
            Ok(()) => EXIT_OK,
            Err(code) => code,
        },
        DevkitCommand::Loadgen { config_path } => todo!(),
    };

    // and if no error caused - return EXIT_OK status code.
    std::process::exit(code);
}
