# Benchmark Results — Dual-Optimistic L2

**Date**: 2026-03-30
**Phase**: 5-D
**Environment**: Windows 11 Pro, tokio simulated time (`test-util`)

---

## Latency Model

The dual-optimistic design overlaps speculative EVM execution with the Mysticeti
3-round wave consensus.  Each wave takes 3Δ to reach HardCommit.

### Optimistic path

```
t = 0      round starts
t = 2Δ     SoftCommit fires → speculative execution begins
t = 2Δ+E   speculative execution finishes
t = 3Δ     HardCommit fires → no conflict → result confirmed

result visible at: max(2Δ+E, 3Δ)
```

### Baseline path

```
t = 0      round starts
t = 3Δ     HardCommit fires → execution begins (serial)
t = 3Δ+E   result visible
```

### Theoretical gain

```
gain = baseline − optimistic
     = (3Δ+E) − max(2Δ+E, 3Δ)
     = min(Δ, E)          (always ≥ 0)
```

| Condition     | gain      | intuition                                      |
|---------------|-----------|------------------------------------------------|
| E ≤ Δ         | E         | execution fully hidden under the [2Δ, 3Δ] slot |
| E > Δ         | Δ         | execution spills over; limited by Δ            |
| E = 0         | 0         | no execution work → no gain possible           |

---

## Test Results

All tests run via `cargo test -p testkit` with `tokio::time::pause()` (simulated
time — 0 ms real wall-clock for any Δ value).

### bench_baseline

| Parameter | Value |
|-----------|-------|
| Δ         | 100 ms (simulated) |
| E         | 50 ms (simulated)  |
| Expected  | 3Δ + E = 350 ms    |
| Measured  | 350 ms ✓           |

**Result**: `ok`

---

### bench_optimistic_faster_than_baseline

| Parameter  | Value              |
|------------|--------------------|
| Δ          | 100 ms (simulated) |
| E          | 50 ms (simulated)  |
| Optimistic | max(2Δ+E, 3Δ) = max(250, 300) = 300 ms |
| Baseline   | 3Δ+E = 350 ms      |
| Gain       | 50 ms = min(Δ, E) = min(100, 50) = 50 ms ✓ |

**Result**: `ok` — optimistic < baseline, gain ≈ E

---

### bench_conflict_sweep

| Conflict % | Avg optimistic | Avg baseline | Avg gain | Min expected gain |
|-----------|---------------|-------------|---------|-------------------|
| 0 %       | 300 ms        | 340 ms      | 40 ms   | 40 ms ✓           |
| 50 %      | 320 ms        | 340 ms      | 20 ms   | 20 ms ✓           |
| 100 %     | 340 ms        | 340 ms      | 0 ms    | 0 ms ✓            |

Parameters: Δ = 100 ms, E = 40 ms (simulated), 10 waves per conflict level.

**Conflict model**: when a conflict occurs, the speculative result is discarded
and execution re-runs after HardCommit (matching the baseline cost).  Even at
100 % conflicts the optimistic path never becomes *worse* than the baseline.

**Result**: `ok` — gain ≥ min_expected_gain at all conflict rates

---

## Key Observations

1. **Zero-overhead guarantee**: the optimistic path is never slower than baseline,
   regardless of conflict rate.

2. **E ≤ Δ regime** (most common for L2 EVM bundles): the entire execution cost
   is absorbed into the [2Δ, 3Δ] speculative window.  Users observe `3Δ` latency
   instead of `3Δ+E`.

3. **Backpressure integration**: `MockExecutor::new_with_delay` sends
   `BackpressureSignal::SlowDown` before each slow batch and `Resume` after,
   allowing `PipelineScheduler` to throttle without dropping batches.

4. **Out-of-order resilience**: `PendingQueue` buffers out-of-order SoftCommit
   events and dispatches them in strict `commit_index` order (R3 → R6 → R9),
   preserving causal ordering in the EVM execution pipeline.

---

## Caveats & Future Work (Phase 6)

- All measurements use **simulated time** (`tokio::time::pause`).  Real-network
  Δ measurements require a Docker multi-node setup (Phase 6).
- `ParallelExecutor` with actual REVM integration is a Phase 6 deliverable;
  current benchmarks use `MockExecutor`.
- Conflict detection (slot-level read/write sets) is modelled probabilistically
  in the sweep test.  Actual EVM conflict rates depend on workload.
