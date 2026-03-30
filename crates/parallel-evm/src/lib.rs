//! Parallel EVM executor crate.
//!
//! Responsibilities:
//! - Execute TxBatch against ShadowDb in parallel (rayon / tokio tasks)
//! - Track per-TX read/write sets for conflict detection (D5: slot granularity)
//! - Return RoundExecutionResult with state diff and conflict list
//! - Send BackpressureSignal to scheduler when queue saturates

#![allow(dead_code, unused_variables)]

use std::{future::Future, sync::Arc};
use shared::{CommitError, RoundExecutionResult, TxBatch};
use shadow_state::ShadowDb;

// ---------------------------------------------------------------------------
// ParallelExecutor trait
// ---------------------------------------------------------------------------

pub trait ParallelExecutor: Send + Sync {
    /// Execute a batch of transactions against the shadow state.
    fn execute(
        &self,
        batch: TxBatch,
        db: Arc<ShadowDb<revm_database_interface::EmptyDB>>,
    ) -> impl Future<Output = Result<RoundExecutionResult, shared::ExecutorError>> + Send;
}

// ---------------------------------------------------------------------------
// CommitWrapper trait
// ---------------------------------------------------------------------------

pub trait CommitWrapper: Send + Sync {
    /// Route a finalized execution result to canonical state commit.
    fn on_hard_commit(
        &self,
        result: RoundExecutionResult,
        db: Arc<ShadowDb<revm_database_interface::EmptyDB>>,
    ) -> impl Future<Output = Result<(), CommitError>> + Send;

    /// Discard a speculative diff after conflict resolution.
    fn on_conflict_discard(
        &self,
        commit_index: u64,
        db: Arc<ShadowDb<revm_database_interface::EmptyDB>>,
    ) -> impl Future<Output = ()> + Send;
}

// ---------------------------------------------------------------------------
// MockExecutor
// ---------------------------------------------------------------------------

/// Event recorded by [`MockExecutor`] for each [`TxBatch`] processed.
#[derive(Debug, Clone)]
pub struct ExecutorEvent {
    pub round: shared::Round,
    pub commit_index: u64,
    pub is_optimistic: bool,
    pub tx_count: usize,
}

/// Mock EVM executor for integration tests.
///
/// Immediately acknowledges each [`TxBatch`] without real EVM execution.
/// Optionally injects a per-batch artificial delay and emits
/// [`BackpressureSignal`][shared::BackpressureSignal]s so the backpressure
/// path can be exercised deterministically.
pub struct MockExecutor {
    received: Arc<std::sync::Mutex<Vec<ExecutorEvent>>>,
    /// Artificial per-batch delay in milliseconds (0 = none).
    delay_ms: u64,
}

impl MockExecutor {
    pub fn new() -> Self {
        Self {
            received: Arc::new(std::sync::Mutex::new(Vec::new())),
            delay_ms: 0,
        }
    }

    /// Create an executor that sleeps `delay_ms` per batch and wraps each
    /// sleep with `SlowDown` / `Resume` backpressure signals.
    pub fn new_with_delay(delay_ms: u64) -> Self {
        Self {
            received: Arc::new(std::sync::Mutex::new(Vec::new())),
            delay_ms,
        }
    }

    /// Returns a shared handle to the event log.  Clone this **before**
    /// calling [`run`][Self::run] so tests can inspect results after the task ends.
    pub fn events(&self) -> Arc<std::sync::Mutex<Vec<ExecutorEvent>>> {
        Arc::clone(&self.received)
    }

    /// Consume the executor and run its event loop until `batch_rx` closes.
    pub async fn run(
        self,
        mut batch_rx: tokio::sync::mpsc::Receiver<TxBatch>,
        bp_tx: tokio::sync::mpsc::Sender<shared::BackpressureSignal>,
    ) {
        while let Some(batch) = batch_rx.recv().await {
            self.received.lock().unwrap().push(ExecutorEvent {
                round: batch.round,
                commit_index: batch.commit_index,
                is_optimistic: batch.is_optimistic,
                tx_count: batch.txs.len(),
            });

            if self.delay_ms > 0 {
                let _ = bp_tx
                    .send(shared::BackpressureSignal::SlowDown)
                    .await;
                tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
                let _ = bp_tx
                    .send(shared::BackpressureSignal::Resume)
                    .await;
            }
        }
    }
}

impl Default for MockExecutor {
    fn default() -> Self {
        Self::new()
    }
}
