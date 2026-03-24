# StryiChain

StryiChain is a blockchain prototype written in Rust for experiments and learning.
It is named after the Stryi River in Ukraine.

## What is inside?

- **Modular architecture.**
  The project is split into five crates with clearly separated responsibilities: `stryi_core`,
  `stryi_storage`, `stryi_node`, `stryi_network`, `stryi_devkit`, and `stryi_wallet`.
- **Pure Rust stack.**
  StryiChain uses `tokio` for async runtime, `fjall` for storage, `k256` for cryptographic signatures,
  `petgraph` for transaction and block dependency checks, `bincode` for compact encoding, and `thiserror` for error
  handling.
- **Custom CPU-friendly Proof-of-Work.**
  StryiChain uses a custom PoW algorithm based on Tor's `HashX` puzzle and `BLAKE3`, designed to keep mining practical
  on ordinary CPUs and unattractive for GPUs and FPGAs.
- **Hybrid networking.**
    - `libp2p` handles peer discovery, lightweight gossip for transactions and blocks, and mempool synchronization.
    - `gRPC` via `tonic` handles block transfer and Initial Block Download (IBD), where a node may need to stream
      thousands of blocks.

## Build

```bash
cargo build --workspace
```

## Demo

The demo walks you through generating a local chain, starting a node,
and sending a transaction between two accounts.

Demo assets live in [`demo/`](demo). The labeled keys used below are
recorded in [`demo/genesis-keys.txt`](demo/genesis-keys.txt).

### 1. Generate the chain and start the node

```bash
# generate a 20-block chain into /tmp/stryi-demo/chain
rm -rf /tmp/stryi-demo && cargo run -p stryi_devkit -- chaingen --config-path demo/chaingen.toml

# start the node, serving HTTP on http://localhost:5556
cargo run -p stryi_node -- --config-path demo/node.toml
```

Keep the node running and open a new terminal for the steps below.

### 2. Explore the chain

```bash
# inspect the generated chain
cargo run -p stryi_wallet -- nodestate
cargo run -p stryi_wallet -- block --id 20

# import the demo accounts
cargo run -p stryi_wallet -- --wallet-path /tmp/stryi-demo/wallet.json import --key 3a7dc55eecb56b8f36874f6920e56a4c4c103ea82b5a168d073d2b49394f3499 --label alice
cargo run -p stryi_wallet -- --wallet-path /tmp/stryi-demo/wallet.json import --key 9a95bf6f928f55be8e757917cdc20a3a12d4e2fcfd5b01243d6db6db7005b019 --label bob

# verify wallet contents and balances
cargo run -p stryi_wallet -- --wallet-path /tmp/stryi-demo/wallet.json list
cargo run -p stryi_wallet -- balance --address @4c8c01f08adc9162ff3d137389399634a375d6d6
cargo run -p stryi_wallet -- balance --address @3972d0819c496cadff43cc37f99a9322745aa397
```

### 3. Send a transaction

```bash
# send from alice to bob
cargo run -p stryi_wallet -- --wallet-path /tmp/stryi-demo/wallet.json send --from @4c8c01f08adc9162ff3d137389399634a375d6d6 --to @3972d0819c496cadff43cc37f99a9322745aa397 --amount 2500

# inspect the transaction (replace with the hash printed by the send command)
cargo run -p stryi_wallet -- tx --id <tx-hash>

# check balances after the transaction was processed
cargo run -p stryi_wallet -- balance --address @4c8c01f08adc9162ff3d137389399634a375d6d6
cargo run -p stryi_wallet -- balance --address @3972d0819c496cadff43cc37f99a9322745aa397
```

## Testing

### Local Testing

Run unit tests (requires `cargo nextest` installed, run `cargo binstall cargo-nextest --secure` for that)

- `cargo nextest run --workspace`

### Integration Testing

See the [Local Usage section](e2e/README.md#local-usage) in the E2E README.

### CI

Two manual GitHub Actions workflows are intended to cover different layers:

- `Rust Tests`
  Runs `cargo nextest run --workspace` for fast and parallel unit test runs.
- `E2E`
  Runs multi-container system scenarios. The available scenarios are described in [e2e/README.md](e2e/README.md).
  The `reorg` E2E scenario also publishes benchmark artifacts and a Bencher report.
