# StryiChain

StryiChain is a Rust blockchain prototype built for experiments and learning.

It combines a CPU-oriented Proof-of-Work based on Tor's `HashX` and
`BLAKE3`, `libp2p`+`gRPC` networking, and a CLI wallet for local
testing.

The project is named after the [Stryi River](https://en.wikipedia.org/wiki/Stryi_(river)) in Ukraine.

## Contents

- [Crates structure](#crates-structure)
- [Build](#build)
- [Local Demo](#local-demo)
- [Features](#features)
- [Testing](#testing)
- [Links](#links)

## Showcase

Generate a 20-block demo chain, start a local node, inspect `nodestate`, send a transaction, and watch the chain length
grow.

![Quickstart demo: generate a chain, start the node, inspect nodestate, send a transaction, and watch the next block land](demo/stryi-quickstart.gif)

## Crates structure

- `stryi_core`: core blockchain logic and shared domain types (`Block`, `Transaction`, `AccountAddress`, and more) + tx
  mempool implementation
- `stryi_node`: the node binary implementation, gRPC and http servers, config engine, miner, main event loop,
- `stryi_storage`: storage layer for blocks, UTXOs, transactions. Powered by the [fjall](https://crates.io/crates/fjall)
  db
- `stryi_network`: p2p networking and higher-level protocol glue
- `stryi_devkit`: local development utilities, currently - just a powerful chain generator CLI tool
- `stryi_wallet`: the CLI wallet

## Build

Requires **Rust nightly**. The exact version is pinned in `rust-toolchain.toml`.

```bash
# Install deps
# Ubuntu
sudo apt-get install -y --no-install-recommends \
  clang \
  libprotobuf-dev \
  libssl-dev \
  pkg-config \
  protobuf-compiler
  
# MacOS :
brew install protobuf openssl pkg-config
```

Build the binaries used in the local demo:

```bash
cargo build --release -p stryi_devkit -p stryi_node -p stryi_wallet
```

## Local Demo

This walkthrough creates a disposable chain in `/tmp/stryi-demo`, starts a
local node, and sends a transaction between two demo accounts.

Demo assets live in [`demo/`](demo). The labeled keys used below are
recorded in [`demo/genesis-keys.txt`](demo/genesis-keys.txt).

<details>
<summary>Full step-by-step walkthrough</summary>

All commands below run from the repository root:

### 1. Generate the chain and start the node

```bash
# generate a 20-block chain into /tmp/stryi-demo/chain
rm -rf /tmp/stryi-demo && ./target/release/stryi-devkit chaingen --config-path demo/chaingen.toml

# start the node, serving HTTP on http://localhost:5556
./target/release/stryi-node --config-path demo/node.toml --genesis-config-path demo/genesis.json
```

Leave the node running. Open a second terminal in the repository root before continuing.

### 2. Explore the chain and import the demo accounts

```bash
# inspect the generated chain
./target/release/stryi-wallet nodestate
./target/release/stryi-wallet block 20

# import the demo accounts
./target/release/stryi-wallet --wallet-path /tmp/stryi-demo/wallet.json import --key 3a7dc55eecb56b8f36874f6920e56a4c4c103ea82b5a168d073d2b49394f3499 --label alice
./target/release/stryi-wallet --wallet-path /tmp/stryi-demo/wallet.json import --key 9a95bf6f928f55be8e757917cdc20a3a12d4e2fcfd5b01243d6db6db7005b019 --label bob

# verify wallet contents and balances
./target/release/stryi-wallet --wallet-path /tmp/stryi-demo/wallet.json list

# no --wallet-path needed; querying addresses directly
./target/release/stryi-wallet balance @4c8c01f08adc9162ff3d137389399634a375d6d6 @3972d0819c496cadff43cc37f99a9322745aa397
```

### 3. Send a transaction

```bash
# send from alice to bob and wait for confirmation
./target/release/stryi-wallet --wallet-path /tmp/stryi-demo/wallet.json send --from @4c8c01f08adc9162ff3d137389399634a375d6d6 --to @3972d0819c496cadff43cc37f99a9322745aa397 --amount 25000 --wait
```

### 4. Explore the API

While the node is running, you can open the Swagger UI
at [http://localhost:5556/api/swagger-ui](http://localhost:5556/api/swagger-ui)
to browse all available endpoints.

</details>

## Wallet Interactive Mode

If you want to use the wallet as a REPL instead of one-shot commands, start
it after the node from [step 1](#1-generate-the-chain-and-start-the-node) is already running.

```bash
./target/release/stryi-wallet --wallet-path /tmp/stryi-demo/wallet.json -i
```

This is the session shown in the GIF below. The wallet connects to the node
at `http://localhost:5556` by default. It does not start the node itself.

![Interactive `stryi-wallet` session against a running local node](demo/stryi-wallet-interactive-mode.gif)

## Features

### Consensus Engine

The consensus engine classifies every incoming block: extends the tip, starts a new fork, continues an existing fork, or
has an unknown parent. We need this to know exactly against what storage overlay we have to validate it.

When a competing fork appears, the node needs to validate it without corrupting the canonical chain. It does this
through overlay storages - temporary layers on top of the real state. If the fork wins (more cumulative work), the
overlay is committed and the old tip is rolled back using stored undo data. If it loses, the overlay is discarded. The
canonical state is only ever touched once the outcome is known.

### Custom Proof-of-Work

The PoW is built on
Tor's [HashX](https://tpo.pages.torproject.net/core/doc/tor/md_ext_2equix_2hashx_2README.html)
combined with BLAKE3.

**Why not just SHA-256 like Bitcoin?**

- **Bitcoin's SHA-256 is trivially parallelizable**. Mining migrated from CPUs
  to GPUs to FPGAs to ASICs within a few years, making common hardware really inefficient
- **Litecoin switched to [Scrypt](https://www.tarsnap.com/scrypt/scrypt.pdf)** (a memory-hard KDF) to try to avoid this.
  It didn't really stop ASICs - it just led to a different kind appearing, though it did raise the cost of building
  specialized hardware
- **HashX takes a different approach** - it compiles a unique short program from each
  block's seed, so the CPU executes a different instruction sequence every time.
  It's harder to bake into ASICs when the computation itself keeps changing
- **BLAKE3 ties it together** - since HashX main capability is being ASIC-resistant, and they claim to be
  preimage-resistant, but not
  collision-resistant - we combine it with the BLAKE3. HashX is not a general-purpose cryptographic hash.

tl;dr Same reason Monero switched to RandomX.

My approach should be **reasonably** ASIC-resistant for a basic blockchain while remaining much simpler than RandomX.

#### How it works (see [`block_hash.rs`](crates/stryi_core/src/block/block_hash.rs) for more details)

1. `BLAKE3(block_header)` -> seed
2. `HashX::new(seed)` -> compiles a unique short program from the seed (retries with a re-hash if the seed is "weak")
3. The header is split into 8-byte chunks; each chunk is run through the compiled HashX program
4. All chunk outputs are fed into a streaming BLAKE3 hasher -> final 32-byte hash
5. The hash must be below the current difficulty target

### Hybrid Network

The networking layer combines the libp2p stack with a separate gRPC service.

libp2p handles peer discovery, block/tx propagation, mempool synchronization,
and service advertisement.

I avoided using libp2p for bulk block transfer because:

- extra complexity at this level for no real benefit
- still worse than gRPC in speed and stability for this task
- other blockchains do the same split (Solana, Ethereum CL, and conceptually
  Avalanche, Polkadot, Near)

The libp2p layer lets nodes exchange signed service records - each node
advertises its gRPC address along with a TLS cert. After that, nodes
connect via gRPC directly.

#### Network layer architecture scheme

```
+--stryi_network---------------------------------------------------------+
|                                                                        |
|  +--libp2p (broadcast)--------------+   +--gRPC (direct, TLS)-------+  |
|  |                                  |   |                           |  |
|  |  discovery:                      |   |  GetChainInfo             |  |
|  |    rendezvous - find peers       |   |    tip, height, work      |  |
|  |    identity   - Ed25519 ids      |   |                           |  |
|  |                                  |   |  GetBlocksByHeight        |  |
|  |  propagation:                    |   |    block stream for IBD   |  |
|  |    gossipsub - blocks, txs, tips |   |                           |  |
|  |    mempool   - exchange tx pools |   |  binary-search LCA        |  |
|  |                                  |   |    find fork point        |  |
|  |  services:                       |   |                           |  |
|  |    svc records - addr + TLS cert |   |                           |  |
|  |                                  |   |                           |  |
|  +----------------+-----------------+   +---+-----------------------+  |
|                   |                         |                          |
|                   v  peers learn each       ^                          |
|                   |  other's gRPC address   |                          |
|                   |  and TLS cert from      |                          |
|                   v  signed service records ^                          |
|                   |  using the libp2p layer |                          |
|                   +---->------->------>-----+                          |
|                                                                        |
+------------------------------------------------------------------------+
```

### Performance

On average, the Node (and the StryiConsensusEngine) fully validates and applies ~403 blocks/sec on GHA free shared
runner (4 vCPU cores) when performing Initial-Block-Download/reorg on **250 blocks** with total ~3700 transactions
(~15 txs/block, 1-to-8 inputs and outputs per tx.)

A ~2.2x boost over the previous ~180 blocks/sec, an unexpected side effect of migrating the serialization layer
from `bincode` to `postcard`.

<a href="https://bencher.dev/perf/stryichain?branches=9b14b8a6-2243-4e11-8280-b59b52165d96&testbeds=627aa475-6e11-4f51-820e-7c8dc150724c%2Ce35664cc-adc7-4c6a-99e0-18220707ee04&benchmarks=ad177bc0-2a92-4a5f-8136-d9162b22f752&measures=8613913e-2bb2-40ec-9457-3cb09e68f66b&x_axis=version&lower_value=false&upper_value=false&key=true">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://stryichain-perf-badge.rzntl256.workers.dev/chart.svg?theme=dark">
    <img src="https://stryichain-perf-badge.rzntl256.workers.dev/chart.svg?theme=light" alt="StryiChain - blocks validated per second">
  </picture>
</a>

### Core Features

- UTXO transaction model with secp256k1 signatures
- Configurable fee policy: heavier transactions (more inputs, outputs, bytes) require more fees to be included in the
  block.
- HTTP API for blocks, transactions, balances, node state (querying and interacting with all of which is supported by
  `stryi_wallet -i`)
- Deterministic, fast, and customizable chain generator for testing and benchmarks
- CLI wallet with interactive REPL
- Persistent storage backed by [fjall](https://crates.io/crates/fjall)
- Multi-node E2E tests in Docker Compose (IBD, reorgs, tx lifecycle)
- Mempool with dependency tracking, and Replace-By-Fee

### Not yet implemented

- Bitcoin-style transaction scripts. Scripting support for things like coin locking and other non-trivial
  spending conditions
- A more capable wallet implementation that can create transactions with multiple outputs with a nice UX.
- Multisig (or threshold signatures) support
- An ENS-like username system, but native and baked into the core architecture
    - The idea: social-networks-inspired username format like @trinity, @neo, @007 as first-class AccountAddress values
    - Would probably need new `TransactionKind` variants to handle renting, buying, and transferring names
    - Probably needs a decentralized storage layer for name resolution (via Kademlia DHT?)
- Orphan blocks handling for the ConsensusEngine. Currently, the consensus engine just drops blocks whose
  parent is absolutely unknown.
- A minimalistic block explorer (something like etherscan) using htmx and ssr
- Smarter FeePolicy, RBF, and DifficultyCalc impls. Currently these are simple
  linear formulas, not adaptive to actual network conditions
- stryi_devkit's loadgen tool and new corresponding e2e routines with some benchmarks

## Testing

### Unit Tests

Install `cargo-nextest` if needed:

```bash
cargo install cargo-nextest --locked
```

Run the test suite:

```bash
cargo nextest run --workspace
```

### Integration Testing

See the [Local Usage section](e2e/README.md#local-usage) in the E2E README.

## Telemetry (tokio-console)

`stryi-node` can expose tokio runtime telemetry for `tokio-console`:

```bash
cargo install --locked tokio-console
cargo run -p stryi_node --bin stryi-node --features telemetry
tokio-console
```

### CI

Two manually triggered GitHub Actions workflows to test different layers:

- `Run nextest`
  Runs `cargo nextest run --workspace` for fast and parallel unit test runs.
- `E2E`
  Runs multi-container system scenarios. The available scenarios are described in [e2e/README.md](e2e/README.md).
  The `reorg` E2E scenario also publishes benchmark artifacts and a Bencher report.

## Links

- HashX
    - https://tpo.pages.torproject.net/core/doc/tor/md_ext_2equix_2hashx_2README.html
    - https://tpo.pages.torproject.net/core/doc/tor/md_ext_2equix_2devlog.html
    - https://gitlab.torproject.org/tpo/core/arti/-/issues/889
- Blockchain's general concepts
    - https://btcinformation.org/en/developer-reference
    - https://developer.bitcoin.org/devguide/block_chain.html
    - https://developer.bitcoin.org/devguide/p2p_network.html
- Networking
    - https://github.com/libp2p/specs
    - https://docs.rs/libp2p/latest/libp2p/rendezvous/
    - https://docs.rs/libp2p/latest/libp2p/gossipsub/
- Others
    - https://github.com/tokio-rs/console
    - https://protobuf.dev/overview/
