/// Implementation of ChainGen tool.
mod chaingen;
/// Exit codes used by the CLI.
mod codes;

use crate::codes::{EXIT_CONFIG, EXIT_IO_OR_PARSE, EXIT_OK};
use clap::{Parser, Subcommand};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

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

fn run_chaingen(config_path: PathBuf) -> Result<(), i32> {
    // read the config.

    let config = {
        if !config_path.is_file() {
            eprintln!(
                "Provided path to config is not a file: {}",
                config_path.display()
            );
            return Err(EXIT_IO_OR_PARSE);
        }

        // read file
        let mut buf = String::new();

        let mut file = File::open(config_path).map_err(|e| {
            eprintln!("Cannot open config at path. Error : {e}");
            EXIT_IO_OR_PARSE
        })?;

        file.read_to_string(&mut buf).map_err(|e| {
            eprintln!("Cannot read config at path. Error : {e}");
            EXIT_IO_OR_PARSE
        })?;

        // Try to deserialize.
        match toml::from_str::<chaingen::ChainGenConfig>(&buf) {
            Ok(cfg) => cfg,

            Err(e) => {
                eprintln!("Error while deserializing ChainGenConfig. Error : {e}");
                return Err(EXIT_IO_OR_PARSE);
            }
        }
    };
    println!("Successfully read the config from file.");

    // validate the config's value.
    match config.validate() {
        Ok(()) => {
            println!("Successfully validated the config.");
        }
        Err(e) => {
            eprintln!("Invalid ChainGenConfig::seed value: {e}");
            return Err(EXIT_CONFIG);
        }
    };

    println!("Successfully read and deserialized the config.");

    Ok(())
}

fn main() -> Result<(), i32> {
    let cli = DevKitCli::parse();

    match cli.cmd {
        DevkitCommand::Chaingen { config_path } => run_chaingen(config_path),

        DevkitCommand::Loadgen { .. } => todo!("Loadgen tool is unimplemented yet."),
    }?; // <-- this will return an error from main if any.

    // and if no error caused - return EXIT_OK status code.
    Err(EXIT_OK)
}
