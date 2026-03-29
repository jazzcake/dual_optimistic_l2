# Dual Optimistic L2

A high-throughput L2 execution pipeline combining Mysticeti DAG consensus with parallel EVM execution.

**Core idea**: Start executing transactions optimistically at the 2Δ soft-commit point, before global consensus finalizes at 3Δ. This guarantees a latency reduction of `min(Δ, E)` over the baseline serial model.

- Architecture & theory: [`docs/architecture.md`](docs/architecture.md)
- Task roadmap: [`docs/tasks/TASKS.md`](docs/tasks/TASKS.md)

---

## Prerequisites

- Rust 1.75+ (`rustup.rs`)
- Git with submodule support

## Setup

```bash
git clone https://github.com/<you>/dual_optimistic_l
cd dual_optimistic_l
git submodule update --init --recursive
cargo build
```

## Running

> Work in progress — see [`docs/tasks/TASKS.md`](docs/tasks/TASKS.md) for current status.

## Testing

```bash
# All tests
cargo test

# Single crate
cargo test -p shadow-state
cargo test -p scheduler
cargo test -p consensus
```

## Project Structure

```
crates/
├── consensus/      # Mysticeti DAG consensus (extracted from SUI)
├── scheduler/      # Optimistic pipeline scheduler with backpressure
├── shadow-state/   # Multi-version shadow memory (REVM Database impl)
├── parallel-evm/   # Parallel EVM executor
└── node/           # Top-level node binary

extern/
├── sui/            # Forked MystenLabs/sui (submodule)
└── revm/           # Forked bluealloy/revm (submodule)
```
