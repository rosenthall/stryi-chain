# StryiChain

StryiChain is a minimal, research-grade blockchain prototype named after the Ukrainian river **Stryi**.

## What is inside?

- **Custom CPU-only Proof-of-Work algorithm** A variant of Tor’s HashX puzzle wrapped in BLAKE3 designed to keep mining
  practical
  on ordinary CPUs and unprofitable on GPUs/FPGAs.
- **Module architecture** - project conveniently split in 5 crates : `stryi_core`, `striy_storage`, `striy_node`,
  `stryi_network` and `striy_devkit`, clean and separated logic.
- **Hybrid networking.**
    - **libp2p** handles peer discovery and lightweight gossip for txs and blocks + mempool synchronization.
    - **gRPC (tonic)** moves heavy traffic—full block transfer, Initial Block Download (IBD).
- **Pure-Rust stack.** Async with **Tokio**; persistent data via **fjall**; cryptography by **k256**;
  transactions/blocks dependencies checks through **petgraph**; compact encoding with **bincode**; errors via *
  *thiserror**; etc.

More on the architecture lives in **DESIGN.md**.
(todo: create DESIGN.md 🫠)

## Build

todo!

## Testing

Two manual GitHub Actions workflows are intended to cover different layers:

- `Rust Tests`
  Runs `cargo nextest run --workspace` for fast code-level feedback.
- `E2E`
  Runs multi-container system scenarios. The available scenarios are described in [e2e/README.md](e2e/README.md).

The `reorg` E2E scenario also publishes benchmark artifacts and a Bencher report.