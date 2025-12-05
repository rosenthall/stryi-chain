## `stryi-devkit` Design Overview


This subcrate provides CLI tools and utilities for development and testing of StryiChain.
It includes:
- chain generation tool
  - http load generator for node testing

Both of tools are provided by single binary `stryi-devkit`.
So you can run them as:

```bash
stryi-devkit chaingen --help
stryi-devkit loadgen --help
```
##### Note: CLI arguments
    None of the binaries meant to be run with any arguments except for `--config-path <path>` to specify custom config file location. 
    Why? - because these tools are meant to be used in automated testing, CI/CD pipelines, and benchmarks.
    So you can create a config file with all the parameters you need and run the tool with that config file.
    I think it's more convenient than passing a lot of arguments via CLI, and the `overload case` (when you want to use config, but overload some values via CLI/ENV) is not that common as in `stryi-node` binary.


##### Note: Why TOML?
    Config files are in TOML format.
    Why TOML? - Initially I wanted to use YAML (which widely used in many CI/CD systems and configurations), but the Rust ecosystem for YAML is not that great as for TOML.



## Chain Generation Tool


### Overview

The chain generation tool is a command-line application that allows users to generate a deterministic blockchain history for testing purposes.
It helps test first-time node startup and Initial Block Download (IBD) performance, different consensus rules, and other scenarios such as : 
  - "Will the ConsensusEngine of node still work in chain of 100k blocks?"
  - "How fast can node sync 50k blocks from scratch?"
  - Experiment with different block intervals, difficulty adjustment algorithms, and other consensus parameters.
  - Benchmark "How fast can node validate 10k blocks with 100 txs each?", "How much memory ChainIndex will use?"
  - See how fast node can find LCA (Last Common Ancestor) from other peer when local chain diverged from the peer's chain at block 3k and the peer has 5k blocks.
  - etc.
  So basically helps to see if blockchain is really working as expected.
  This tool is meant to be used in automated testing, CI/CD pipelines, and benchmarks.





### How to use
The chain generation tool can be run from the command line with provided configuration file.

```bash 
stryi-devkit chaingen --config-path <path_to_config_file>
```

#### Configuration
The configuration file is in TOML format.
Examples : 


#### Configuration

Configuration is read from a TOML file provided via `--config-path`. The file contains two main areas: `[chain]` for chain-wide settings and `[blocks]` for block-generation rules. Values are validated at startup and the tool emits clear errors for unknown or invalid keys.

Top-level `[chain]` keys (comments kept immediately above fields)

```toml
[chain]

# Number of blocks to generate
num_blocks = 100

# Seed for random number generator.
# Using the same seed will always produce the same chain.
# You can use any integer value as seed.
#
# Multiple seeds:
# Provide multiple seeds mapped by block height ranges to switch RNG at given heights.
# Example:
# seed = [
#   { height = 1, value = 42 },     # use seed 42 from block 0 to block 99
#   { height = 100, value = 43 },   # use seed 43 from block 100 to block 199
# ]
# Note: heights must be ascending; minimum height is 1 (block 0 is genesis).
seed = 42

# Path where to initialize the chain. This path will be created if it does not exist.
# The tool also creates `CHAINGEN_PRIVATE_KEYS.txt` with generated account keys.
output_path = "/tmp/testchain1"

# Path to genesis file in the same format as used in `stryi-node`.
# This genesis defines initial state of the chain: balances, consensus constants, etc.
genesis_path = "/path/to/genesis.json"

# How generated blocks are persisted/applied.
# Allowed values: "consensus_engine", "direct_insert"
# Default: "consensus_engine"
# - "consensus_engine" - Build and feed blocks into StryiConsensusEngine; validates and applies blocks as a real node would. Recommended for most tests and benchmarks.
# - "direct_insert" - Write blocks directly to storage without consensus validation. Faster, useful for low-level tests, but may create chains that real nodes reject. Use only when you know what you are doing.
persistence_mode = "consensus_engine"


[blocks]

# Private key (hex) of an account that has pre-defined balance in genesis.
# The available balance will be evenly distributed among generated active addresses.
funding_key = "8fea080a21992e9262bbd64698d4c81d995c94dd9d262f3a572d2a8f1b65575a"

# Average time between blocks in seconds (sets block header timestamp increment)
average_block_time_secs = 10

# Address of the miner who will mine all the blocks in this chain.
# Using a fixed address allows reuse of its private key after generation.
miner_address = "@2fdf51216b8d12feb0ecd4299446465cd8c013a5"

# Range for number of transactions per block.
# Each block will have a random number of transactions in this range.
min_transactions_per_block = 5
max_transactions_per_block = 25

# Number of unique active addresses in the chain.
# All transactions will be created between these addresses.
active_addresses_count = 50

# Whether to insert BlockUndo records for each block.
# BlockUndo records are used to roll back the chain to previous state.
# Usually not needed for testing; disable to save disk space and speed up generation.
# NOTE: if persistence_mode is "consensus_engine", this setting is ignored and BlockUndo records are always created.
undo = false

```

#### Implementation details

Implementation has two modes, mode depends on whether it relies on stryi_core's ConsensusEngine, or if it just naively interacts with Storage layer.
Initially there was only direct-insert mode, but it failed a lot when ConsensusEngine of real node tried to validate the generated chain,
because of some hard-to-debug issues of generated blocks not passing consensus validation.
Thats is why there are two ways.


**WIP**





## Load Generator Tool
**WIP**