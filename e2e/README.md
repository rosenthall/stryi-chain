# End-to-End Testing

The E2E setup exercises system behavior that is expensive or awkward to validate with unit tests alone.

It is organized as:
- one shared Compose base file
- one Compose override per scenario
- one terminal assertion service per scenario

The goal is to keep the scenario graph explicit without turning the whole E2E setup into a generic framework.

The default E2E stack does not publish node ports to the host. Scenario services talk to each other over the Compose network, which avoids host port collisions when you switch scenarios.

## Available Scenarios

### `reorg`

This is the existing multi-node convergence scenario.

It covers:
- deterministic chain generation
- bootstrap of the primary node
- IBD for a joining client
- introduction of a heavier competing branch
- convergence of all nodes on the reorged tip

This scenario also produces benchmark artifacts and a Bencher report in CI.

### `tx-lifecycle`

This is the MVP usability scenario.

It covers:
- bootstrap of a node from the shared E2E genesis
- submission of a transaction through `stryi-wallet`
- observation of the transaction in `pending` state
- confirmation by the node miner
- observation of the same transaction in `confirmed` state
- balance checks for the sender and recipient before and after confirmation
- node height advancing by exactly one block for the mined transaction

This scenario is correctness-focused and does not publish benchmark artifacts.

## Local Usage

Run scenarios from the repository root.

### Reorg

```bash
docker compose \
  -p stryi-e2e-reorg \
  -f e2e/docker/docker-compose.e2e.yml \
  -f e2e/docker/docker-compose.e2e.reorg.yml \
  up --build --remove-orphans wait-for-reorg
```

### Transaction lifecycle

```bash
docker compose \
  -p stryi-e2e-tx-lifecycle \
  -f e2e/docker/docker-compose.e2e.yml \
  -f e2e/docker/docker-compose.e2e.tx-lifecycle.yml \
  up --build --remove-orphans wait-for-tx-lifecycle
```

When a local run finishes, tear it down with the same file set:

```bash
docker compose \
  -p stryi-e2e-reorg \
  -f e2e/docker/docker-compose.e2e.yml \
  -f e2e/docker/docker-compose.e2e.reorg.yml \
  down -v --remove-orphans
```

Swap the override file if you ran `tx-lifecycle`.

Use the matching `-p` value when cleaning up a scenario. Separate project names keep scenario state isolated, including volumes, networks, and one-shot containers.

## CI Usage

The `E2E` GitHub Actions workflow is manual and accepts a `scenario` input:
- `reorg`
- `tx-lifecycle`

Result handling:
- `reorg`
  - benchmark JSON and Markdown files are uploaded as an artifact
  - Bencher publishes the benchmark report into the job summary
- `tx-lifecycle`
  - no benchmark artifacts are produced

The `Rust Tests` workflow is separate and runs `cargo nextest run --workspace`. It is intended for fast code-level feedback and can later be promoted to PR CI without changing the E2E workflow.

## Supporting Scripts

- [assert-node-ready.sh](scripts/assert-node-ready.sh)
  Waits for a node to expose the expected height and hash through `/api/nodestate`.
- [assert-reorg-complete.sh](scripts/assert-reorg-complete.sh)
  Verifies that all nodes converge to the expected reorg tip.
- [assert-tx-lifecycle.sh](scripts/assert-tx-lifecycle.sh)
  Submits a transaction with `stryi-wallet`, waits for `pending`, then waits for `confirmed`.
- [check-chaingen.sh](scripts/check-chaingen.sh)
  Performs a basic sanity check on generated chain data before nodes consume it.
- [collect-benchmark.sh](scripts/collect-benchmark.sh)
  Extracts the benchmark report for the `reorg` scenario.
