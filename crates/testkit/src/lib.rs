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

    /// Compute the observed average inter-round commit interval (Δ).
    pub fn measure_delta(&self) -> Duration {
        todo!()
    }

    /// Compute the average pipeline gain per round (hard_commit - soft_commit).
    pub fn measure_pipeline_gain(&self) -> Duration {
        todo!()
    }
}
