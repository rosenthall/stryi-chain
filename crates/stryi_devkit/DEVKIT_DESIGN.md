## `stryi-devkit` Design Overview

This subcrate provides CLI tools and utilities for development and testing of StryiChain.
It includes:

- chain generation tool
- http load generator for node testing

Both tools live in the same binary: `stryi-devkit`.
So you run them as:

```bash
stryi-devkit chaingen --help
stryi-devkit loadgen --help
```

##### Note: CLI arguments

    These tools are not meant to have a long list of CLI flags.
    The expected interface is basically just `--config-path <path>`.
    They are mostly for CI, benchmarks, and repeatable local runs, so a config file is simpler than pushing a lot of knobs through the command line.

##### Note: Why TOML?

    Config files are in TOML format.
    I considered YAML because it is common in CI configs, but TOML ended up being a better fit here.
    The Rust tooling around it is simpler and generally nicer to work with.

## Chain Generation Tool

### Overview

The chain generation tool is a command-line application that generates deterministic blockchain history for testing.
It helps test first-time node startup and Initial Block Download (IBD) performance, different consensus rules, and other
scenarios such as:

- "Will the ConsensusEngine of node still work in chain of 100k blocks?"
- "How fast can node sync 50k blocks from scratch?"
- Experiment with different block intervals, difficulty adjustment algorithms, and other consensus parameters.
- Benchmark "How fast can node validate 10k blocks with 100 txs each?", "How much memory will ChainIndex use?"
- See how fast node can find LCA (Last Common Ancestor) from other peer when local chain diverged from the peer's chain
  at block 3k and the peer has 5k blocks.
- etc.

### How to use

Run it with a config file:

```bash 
stryi-devkit chaingen --config-path <path_to_config_file>
```

#### Configuration

Configuration is read from a TOML file provided via `--config-path`. The file contains two main areas: `[chain]` for
chain-wide settings and `[blocks]` for block-generation rules. Values are validated at startup and the tool emits clear
errors for unknown or invalid keys.

Top-level `[chain]` keys (comments kept immediately above fields)

```toml
[chain]

# Number of blocks to generate after the current tip.
# This number already includes the Distribution Block.
# On a fresh chain, 100 means:
# - 1 distributor block
# - 99 regular generated blocks
# On a non-empty chain, the same rule applies after the existing tip.
# Minimum value: 2
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
# The tool also writes a generated accounts backup file:
# `CHAINGEN_ACCOUNTS_<start_height>_<end_height>.txt`
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
# Minimum value is 2 because the first transaction slot is always coinbase.
min_transactions_per_block = 5
max_transactions_per_block = 25

# Number of unique active addresses in the chain.
# All transactions will be created between these addresses.
active_addresses_count = 50

# Whether to insert BlockUndo records for each block.
# BlockUndo records are used to roll back the chain to previous state.
# Usually not needed for testing; disable to save disk space and speed up generation.
# NOTE:
# - `need_undo = true` requires `persistence_mode = "consensus_engine"`
# - in consensus-engine mode, undo data is already produced by the engine
need_undo = true

```

#### Implementation details

Chaingen has two persistence modes.

- `consensus_engine`
  This is the normal mode. Generated blocks go through `StryiConsensusEngine`, so the chain is checked the same way a
  real node would check it.
- `direct_insert`
  This skips consensus validation and writes blocks straight to storage. It is useful for lower-level experiments and
  some storage benchmarks, but it can also produce chains a real node would reject.
  DO NOT USE IT FOR OTHER REASONS THAN EXPERIMENTING

Chaingen is resume-friendly: it opens existing storage, reads the current tip, and keeps going from there.
That also means rerunning the same config against the same `output_path` is different from starting fresh.
The funding account may already be drained by an older run, so for clean benchmark runs it is usually better to use a
fresh directory.

##### Account generation

After config validation, chaingen deterministically generates the active addresses used for synthetic transactions.
Payment transactions are created between those addresses.
When the run finishes, their private keys are written to a file in the output directory.
The file name includes the start and end heights of the generated segment, so it is easy to match the backup to a
specific run.

##### Distribution Block

The Distribution Block is a special block inserted into the chain at the beginning of the generation process.
The purpose of this block is to evenly distribute funds among all the generated active addresses.
The block contains:

- the normal coinbase transaction for that height
- one payment transaction that drains the configured funding account into the generated active addresses

## Load Generator Tool

**WIP**