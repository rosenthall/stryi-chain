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

```toml
[chain]


# Number of blocks to generate
num_blocks = 100


# Seed for random number generator.
# Using the same seed will always produce the same chain.
# You can use any integer value as seed.
#
# Multiply Seeds: 
# In some cases you may want to have chain that has first N blocks always the same, but after that it may diverge.
# It may be useful for testing LCA, Reorganizations, etc.
# In this case you can use "multiple seeds" feature.
# Basically you can provide multiple seeds in array mapped by block height ranges and the generator will switch to the new seed when it reaches the specified block height.
# Example:
# seed = [ 
#   { height = 1 value = 42 },        # use seed 42 from block 0 to block 99
#   { height = 100, value = 43 },    # use seed 1234 from block 100 to block 199
# ]
# Note: the height values must be in ascending order. The minimum height must be 1 (because block 0 is genesis and always the same). Height values are inclusive, and the last range goes to up to `num_blocks`.
seed = 42


# path where to initalize the chain.
# this path will be created if not exists.
# internally it will use `stryi_storage` crate for initalization logic.
output_path = "/tmp/testchain1"

# Genesis block configuration
# Path to genesis file in the same format as used in `stryi-node`. (see `stryi-node` docs for details)
# this genesis defines initial state of the chain : balances, consts of the validation engine, etc.
genesis_path = "/path/to/genesis.json"


# Configuration for block generation.
# Defines some rules for the blocks in the chain

[blocks]

# Average time between blocks in seconds (sets block header timestamp increment)
average_block_time_secs = 10

# Address of the miner who will mine all the blocks in this chain.
# We use some fixed address instead of random one here so after the chain is generated user may use its private key to sign transactions from this address.
# It's not the same as if some balance was assigned to this address in genesis, because genesis' funds are usually distributed while generating chain history.
miner_address = "@2fdf51216b8d12feb0ecd4299446465cd8c013a5"

# Range for number of transactions per block. 
# Each block will have random number of transactions in this range.
min_transactions_per_block = 5
max_transactions_per_block = 25

# Number of unique active addresses in the chain.
# Basically, all the transactions will be created between these addresses.
active_addresses_count = 50




```


#### Implementation details
WIP








## Load Generator Tool
WIP