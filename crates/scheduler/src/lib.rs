//! Scheduler crate: consensus event → transaction batch dispatch.
//!
//! Responsibilities:
//! - Receive `ConsensusEvent` from the consensus broadcast channel.
//! - Buffer out-of-order SoftCommits and emit `TxBatch` to the executor in
//!   ascending round order.
//! - On HardCommit, confirm speculative results or supply a fresh batch.
//! - Apply backpressure (SlowDown / Resume) based on pending-queue depth.

mod backpressure;
mod pending_queue;
mod pipeline;

pub use backpressure::BackpressureController;
pub use pending_queue::{HardCommitDecision, PendingQueue};
pub use pipeline::{CommitDecision, PipelineScheduler};
