#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
#![feature(exact_size_is_empty)]

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
    // Set the default log level to info if RUST_LOG is not set
    let env_layer = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("fjall=error".parse().unwrap())
        .add_directive("lsm_tree=error".parse().unwrap());

    if use_json_logs() {
        tracing_subscriber::registry()
            .with(env_layer)
            .with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_level(true)
                    .flatten_event(true),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_layer)
            .with(fmt::layer().with_target(true).with_level(true))
            .init();
    }

    let cli = DevKitCli::parse();

    // process command
    let cmd_result = match cli.cmd {
        DevkitCommand::Chaingen { config_path } => run_chaingen(config_path).await,
        DevkitCommand::Loadgen { .. } => {
            eprintln!("Loadgen is not yet implemented.");
            Err(EXIT_UNKNOWN)
        }
    };

    // Map the result to an exit code and exit with it.
    std::process::exit(result_into_code(cmd_result));
}

/// Maps `Result<(), i32>` into code.
/// `Ok(_)` to EXIT_OK
/// And `Err(c)` to c.
fn result_into_code(res: Result<(), i32>) -> i32 {
    match res {
        Ok(_) => EXIT_OK,
        Err(code) => code,
    }
}

fn use_json_logs() -> bool {
    matches!(
        std::env::var("STRYI_LOG_FORMAT").ok().as_deref(),
        Some("json") | Some("JSON")
    )
}
