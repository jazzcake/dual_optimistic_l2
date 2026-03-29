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
        db: Arc<ShadowDb>,
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
        db: Arc<ShadowDb>,
    ) -> impl Future<Output = Result<(), CommitError>> + Send;

    /// Discard a speculative diff after conflict resolution.
    fn on_conflict_discard(
        &self,
        commit_index: u64,
        db: Arc<ShadowDb>,
    ) -> impl Future<Output = ()> + Send;
}
