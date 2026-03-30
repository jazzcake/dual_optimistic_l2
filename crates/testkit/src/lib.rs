//! Testkit crate: deterministic simulation and benchmark harness.
//!
//! Responsibilities:
//! - `SimulatedNetwork`: in-process multi-node network with latency injection
//! - `SimulatedNode`: per-node handle for assertions
//! - `BenchmarkHarness`: wall-clock Δ measurement
//! - Uses `tokio::time::pause()` for deterministic time in unit tests

#![allow(dead_code, unused_variables)]

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::broadcast;
use consensus::{ConsensusHandle, LatencyModel, ZeroLatency};
use shared::{ConsensusEvent, Round};

// ---------------------------------------------------------------------------
// SimulatedNetwork: deterministic in-process multi-node network
// ---------------------------------------------------------------------------

pub struct SimulatedNetwork {
    nodes: Vec<SimulatedNode>,
    latency_model: Box<dyn LatencyModel>,
    /// partitions[a][b] == true means node a cannot reach node b.
    partitions: Vec<Vec<bool>>,
}

impl SimulatedNetwork {
    pub fn new(n: usize, latency: impl LatencyModel + 'static) -> Self {
        Self {
            nodes: (0..n).map(|i| SimulatedNode::new_stub(i)).collect(),
            latency_model: Box::new(latency),
            partitions: vec![vec![false; n]; n],
        }
    }

    pub fn node(&self, idx: usize) -> &SimulatedNode {
        &self.nodes[idx]
    }

    /// Inject a partition: nodes in group_a cannot reach nodes in group_b.
    pub fn partition(&mut self, group_a: &[usize], group_b: &[usize]) {
        todo!()
    }

    /// Remove all network partitions.
    pub fn heal_partitions(&mut self) {
        let n = self.nodes.len();
        self.partitions = vec![vec![false; n]; n];
    }

    /// Advance simulation until the given round is committed on all nodes.
    pub async fn run_until_commit(&mut self, target_round: Round) {
        todo!()
    }

    /// Advance simulation for a fixed number of simulated rounds.
    pub async fn run_rounds(&mut self, n: u64) {
        todo!()
    }
}

// ---------------------------------------------------------------------------
// SimulatedNode: single in-process node
// ---------------------------------------------------------------------------

pub struct SimulatedNode {
    pub index: usize,
    pub event_rx: broadcast::Receiver<ConsensusEvent>,
    _consensus: std::marker::PhantomData<Box<dyn ConsensusHandle>>,
}

impl SimulatedNode {
    fn new_stub(index: usize) -> Self {
        // Phase 3: replace with actual consensus handle
        let (tx, rx) = broadcast::channel(128);
        drop(tx);
        Self {
            index,
            event_rx: rx,
            _consensus: std::marker::PhantomData,
        }
    }

    /// Non-blocking drain of all queued consensus events.
    pub fn drain_events(&mut self) -> Vec<ConsensusEvent> {
        let mut events = Vec::new();
        loop {
            match self.event_rx.try_recv() {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }
        events
    }

    pub fn assert_soft_commit(&mut self, round: Round) {
        let events = self.drain_events();
        assert!(
            events.iter().any(|e| matches!(e, ConsensusEvent::SoftCommit { round: r, .. } if *r == round)),
            "expected SoftCommit for round {round}"
        );
    }

    pub fn assert_hard_commit(&mut self, round: Round) {
        let events = self.drain_events();
        assert!(
            events.iter().any(|e| matches!(e, ConsensusEvent::HardCommit { subdag } if subdag.leader.round == round)),
            "expected HardCommit for round {round}"
        );
    }
}

// ---------------------------------------------------------------------------
// BenchmarkHarness: real-time Δ measurement
// ---------------------------------------------------------------------------

pub struct BenchmarkHarness {
    pub network: SimulatedNetwork,
    pub timeline: Arc<Mutex<CommitTimeline>>,
}

#[derive(Default)]
pub struct CommitTimeline {
    /// round → (soft_commit_ts, hard_commit_ts, execution_done_ts)
    pub entries: BTreeMap<Round, CommitTimestamps>,
}

#[derive(Default)]
pub struct CommitTimestamps {
    pub soft_commit_at: Option<Instant>,
    pub hard_commit_at: Option<Instant>,
    pub execution_done_at: Option<Instant>,
}

impl BenchmarkHarness {
    pub fn new(n: usize) -> Self {
        Self {
            network: SimulatedNetwork::new(n, ZeroLatency),
            timeline: Arc::new(Mutex::new(CommitTimeline::default())),
        }
    }

    /// Record that a SoftCommit (2Δ) was observed for `round`.
    pub fn record_soft_commit(&self, round: Round) {
        let mut tl = self.timeline.lock().unwrap();
        tl.entries.entry(round).or_default().soft_commit_at = Some(Instant::now());
    }

    /// Record that a HardCommit (3Δ) was observed for `round`.
    pub fn record_hard_commit(&self, round: Round) {
        let mut tl = self.timeline.lock().unwrap();
        tl.entries.entry(round).or_default().hard_commit_at = Some(Instant::now());
    }

    /// Record that speculative execution completed for `round`.
    pub fn record_exec_done(&self, round: Round) {
        let mut tl = self.timeline.lock().unwrap();
        tl.entries.entry(round).or_default().execution_done_at = Some(Instant::now());
    }

    /// Average wall-clock interval between consecutive HardCommit events (Δ).
    ///
    /// Returns `Duration::ZERO` if fewer than 2 hard-commit timestamps exist.
    pub fn measure_delta(&self) -> Duration {
        let tl = self.timeline.lock().unwrap();
        let mut hard_times: Vec<Instant> = tl
            .entries
            .values()
            .filter_map(|e| e.hard_commit_at)
            .collect();
        hard_times.sort();
        if hard_times.len() < 2 {
            return Duration::ZERO;
        }
        let total: Duration = hard_times
            .windows(2)
            .map(|w| w[1].duration_since(w[0]))
            .sum();
        total / (hard_times.len() as u32 - 1)
    }

    /// Average time saved per wave: hard_commit_at − soft_commit_at.
    ///
    /// This is the "pipeline head start" — how much earlier speculative
    /// execution could start compared to waiting for HardCommit.
    /// Returns `Duration::ZERO` if no complete (soft+hard) round exists.
    pub fn measure_pipeline_gain(&self) -> Duration {
        let tl = self.timeline.lock().unwrap();
        let gains: Vec<Duration> = tl
            .entries
            .values()
            .filter_map(|e| {
                let soft = e.soft_commit_at?;
                let hard = e.hard_commit_at?;
                Some(hard.duration_since(soft))
            })
            .collect();
        if gains.is_empty() {
            return Duration::ZERO;
        }
        gains.iter().sum::<Duration>() / gains.len() as u32
    }
}

// ---------------------------------------------------------------------------
// Benchmark timing helpers (used by benchmark tests in `node`)
// ---------------------------------------------------------------------------

/// Simulate the **optimistic** execution path and return the observed latency.
///
/// Timeline:
///   t=0:       consensus round starts
///   t=2Δ:      SoftCommit fires  → speculative execution begins
///   t=2Δ+E:    speculative execution done
///   t=3Δ:      HardCommit fires  → no conflict → result confirmed
///   result available at: max(2Δ+E, 3Δ)
pub async fn measure_optimistic_latency(delta_ms: u64, exec_ms: u64) -> Duration {
    // Use tokio::time::Instant so that tokio::time::pause() affects measurements.
    let start = tokio::time::Instant::now();

    // 2Δ: wait for SoftCommit.
    tokio::time::sleep(Duration::from_millis(2 * delta_ms)).await;

    // E: speculative execution.
    tokio::time::sleep(Duration::from_millis(exec_ms)).await;
    let after_exec = start.elapsed();

    // Wait until 3Δ if exec finished early (max(2Δ+E, 3Δ)).
    let three_delta = Duration::from_millis(3 * delta_ms);
    if after_exec < three_delta {
        tokio::time::sleep(three_delta - after_exec).await;
    }

    start.elapsed()
}

/// Simulate the **baseline** (non-optimistic) execution path and return the latency.
///
/// Timeline:
///   t=0:   consensus round starts
///   t=3Δ:  HardCommit fires  → execution begins
///   t=3Δ+E: result available
pub async fn measure_baseline_latency(delta_ms: u64, exec_ms: u64) -> Duration {
    // Use tokio::time::Instant so that tokio::time::pause() affects measurements.
    let start = tokio::time::Instant::now();

    // 3Δ: wait for HardCommit.
    tokio::time::sleep(Duration::from_millis(3 * delta_ms)).await;

    // E: serial execution after commit.
    tokio::time::sleep(Duration::from_millis(exec_ms)).await;

    start.elapsed()
}

// ---------------------------------------------------------------------------
// Benchmark tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod bench_tests {
    use super::*;

    /// Verify that `measure_baseline_latency` produces ≈ 3Δ+E.
    ///
    /// Uses `tokio::time::pause()` for instant deterministic simulation.
    #[tokio::test]
    async fn bench_baseline() {
        tokio::time::pause();

        let delta_ms = 100u64;
        let exec_ms = 50u64;

        let elapsed = measure_baseline_latency(delta_ms, exec_ms).await;

        // Expected: 3Δ + E = 300 + 50 = 350 ms (simulated)
        let expected = Duration::from_millis(3 * delta_ms + exec_ms);
        // Allow ±5 ms tolerance for overhead.
        assert!(
            elapsed >= expected,
            "baseline latency {elapsed:?} < expected {expected:?}"
        );
        assert!(
            elapsed < expected + Duration::from_millis(5),
            "baseline latency {elapsed:?} much larger than expected {expected:?}"
        );
    }

    /// Verify that `measure_optimistic_latency` is strictly less than
    /// `measure_baseline_latency` when execution fits within the Δ window.
    ///
    /// With exec_ms < Δ:
    ///   optimistic  = max(2Δ+E, 3Δ) = 3Δ   (exec hidden under 3Δ - 2Δ = Δ slack)
    ///   baseline    = 3Δ + E
    ///   gain        = E = min(Δ, E) since E < Δ
    #[tokio::test]
    async fn bench_optimistic_faster_than_baseline() {
        tokio::time::pause();

        let delta_ms = 100u64;
        let exec_ms = 50u64; // exec_ms < delta_ms → gain = exec_ms

        let opt = measure_optimistic_latency(delta_ms, exec_ms).await;
        let base = measure_baseline_latency(delta_ms, exec_ms).await;

        assert!(
            opt < base,
            "optimistic {opt:?} should be < baseline {base:?}"
        );

        // Gain ≈ min(Δ, E) = exec_ms = 50 ms.
        let gain = base.saturating_sub(opt);
        let expected_gain = Duration::from_millis(exec_ms);
        assert!(
            gain >= expected_gain.saturating_sub(Duration::from_millis(2)),
            "pipeline gain {gain:?} should be ≈ {expected_gain:?}"
        );
    }

    /// Sweep a range of conflict rates (0 %, 50 %, 100 %) and verify that the
    /// optimistic path remains strictly faster than the baseline at every point.
    ///
    /// Model: conflict_pct % of waves have their speculative result discarded and
    /// re-executed serially, costing an extra E at the HardCommit point.
    /// Even with 100 % conflicts the optimistic path still wins because it overlaps
    /// 2Δ → 3Δ with speculative execution; re-execution cost is unchanged from the
    /// baseline, so the total equals baseline — the path is never *worse*.
    ///
    /// Here we assert optimistic ≤ baseline (not strict <) for 100 % conflicts,
    /// and strictly < for lower conflict rates.
    #[tokio::test]
    async fn bench_conflict_sweep() {
        tokio::time::pause();

        let delta_ms = 100u64;
        let exec_ms = 40u64;

        // (conflict_pct, expected_gain_ms_lower_bound)
        let cases: &[(u64, u64)] = &[
            (0, exec_ms),          // no conflicts: gain = min(Δ,E) = E = 40 ms
            (50, exec_ms / 2),     // half waves conflict: avg gain ≈ E/2 = 20 ms
            (100, 0),              // all conflict: optimistic ≤ baseline (gain ≥ 0)
        ];

        for &(conflict_pct, min_gain_ms) in cases {
            // Simulate N waves and aggregate latencies.
            const WAVES: u64 = 10;
            let mut total_opt = Duration::ZERO;
            let mut total_base = Duration::ZERO;

            for wave in 0..WAVES {
                // Determine whether this wave has a conflict.
                let has_conflict = (wave * 100 / WAVES) < conflict_pct;

                // Optimistic path: speculative exec at 2Δ, confirmed (or re-exec) at 3Δ.
                let opt = measure_optimistic_latency(delta_ms, exec_ms).await;
                // Baseline path: wait for HardCommit, then execute serially.
                let base = measure_baseline_latency(delta_ms, exec_ms).await;

                // On conflict the optimistic result is discarded and re-executed.
                // Model: re-execution adds exec_ms after HardCommit, matching baseline.
                let effective_opt = if has_conflict {
                    // Re-execution after conflict: 3Δ + E (same as baseline).
                    base
                } else {
                    opt
                };

                total_opt += effective_opt;
                total_base += base;
            }

            let avg_opt = total_opt / WAVES as u32;
            let avg_base = total_base / WAVES as u32;
            let gain = avg_base.saturating_sub(avg_opt);
            let min_gain = Duration::from_millis(min_gain_ms);

            assert!(
                gain >= min_gain,
                "conflict_pct={conflict_pct}%: avg gain {gain:?} < expected min {min_gain:?}"
            );
            assert!(
                avg_opt <= avg_base,
                "conflict_pct={conflict_pct}%: optimistic {avg_opt:?} > baseline {avg_base:?}"
            );
        }
    }
}
