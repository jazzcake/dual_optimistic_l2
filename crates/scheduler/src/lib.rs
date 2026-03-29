//! Scheduler crate: consensus event → transaction batch dispatch.
//!
//! Responsibilities:
//! - Receive `ConsensusEvent` from consensus broadcast channel
//! - Emit `TxBatch` to executor mpsc channel
//! - Apply backpressure logic (SlowDown / Resume signals)
//! - Guarantee round ordering (SoftCommit before HardCommit for same round)

#![allow(dead_code, unused_variables)]

use tokio::sync::{broadcast, mpsc};
use shared::{BackpressureSignal, ConsensusEvent, TxBatch};

// ---------------------------------------------------------------------------
// SchedulerHandle: channel endpoints + event loop
// ---------------------------------------------------------------------------

pub struct SchedulerHandle {
    consensus_rx: broadcast::Receiver<ConsensusEvent>,
    executor_tx: mpsc::Sender<TxBatch>,
    backpressure_rx: mpsc::Receiver<BackpressureSignal>,
}

impl SchedulerHandle {
    pub fn new(
        consensus_rx: broadcast::Receiver<ConsensusEvent>,
        executor_tx: mpsc::Sender<TxBatch>,
        backpressure_rx: mpsc::Receiver<BackpressureSignal>,
    ) -> Self {
        Self { consensus_rx, executor_tx, backpressure_rx }
    }

    /// Run the scheduler event loop. Meant to be spawned as a task.
    pub async fn run(self) {
        todo!()
    }
}
