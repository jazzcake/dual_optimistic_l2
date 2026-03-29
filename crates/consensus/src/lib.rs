//! Consensus crate: Mysticeti DAG wrapper.
//!
//! Responsibilities:
//! - Implement Mysticeti consensus engine (extracted from extern/sui, SUI-dep-free)
//! - Detect SoftCommit (2Δ wave leader) and HardCommit (3Δ subDAG)
//! - Emit `ConsensusEvent` via broadcast channel
//! - Provide deterministic in-process simulation for testing

// ---------------------------------------------------------------------------
// Phase 3-A: ported SUI modules
// ---------------------------------------------------------------------------

pub mod committee;
pub mod context;
pub mod types;
pub mod commit;

pub(crate) mod stake_aggregator;
pub(crate) mod threshold_clock;
pub mod dag_state;
pub(crate) mod block_manager;
pub(crate) mod base_committer;
pub(crate) mod universal_committer;
pub mod linearizer;

// ---------------------------------------------------------------------------
// Phase 3-B: new modules
// ---------------------------------------------------------------------------

pub(crate) mod soft_commit;
pub mod node;

/// Deterministic simulation infrastructure (test-only).
#[cfg(test)]
pub mod sim;

// ---------------------------------------------------------------------------
// Public re-exports (crate boundary API)
// ---------------------------------------------------------------------------

pub use committee::{Authority, AuthorityIndex, Committee, Epoch, Stake, make_test_committee};
pub use context::Context;
pub use types::{
    Block, BlockDigest, BlockRef, BlockTimestampMs, Round, Slot, TestBlock, Transaction,
    VerifiedBlock, DIGEST_LENGTH, GENESIS_ROUND, genesis_blocks,
};
pub use commit::{CommitDigest, CommitIndex, CommitRef, CommittedSubDag};
pub use dag_state::DagState;
pub use linearizer::Linearizer;
pub use node::ConsensusNode;

// ---------------------------------------------------------------------------
// Pluggable latency model (used by simulation and production network clients)
// ---------------------------------------------------------------------------

use std::time::Duration;

/// Pluggable network-delay model for simulation.
pub trait LatencyModel: Send + Sync {
    fn delay(&self) -> Duration;
}

pub struct ZeroLatency;
impl LatencyModel for ZeroLatency {
    fn delay(&self) -> Duration {
        Duration::ZERO
    }
}

pub struct UniformLatency {
    pub min: Duration,
    pub max: Duration,
}
impl LatencyModel for UniformLatency {
    fn delay(&self) -> Duration {
        // Returns the midpoint of [min, max].
        // A proper random implementation requires an injected RNG (Phase 5).
        self.min + self.max.saturating_sub(self.min) / 2
    }
}

// ---------------------------------------------------------------------------
// ConsensusHandle trait (Phase 3-B stub — full impl in Phase 4)
// ---------------------------------------------------------------------------

use std::{future::Future, pin::Pin};
use tokio::sync::broadcast;
use shared::{ConsensusError, ConsensusEvent, EthSignedTx};

/// Handle to the running consensus engine.
pub trait ConsensusHandle: Send + Sync {
    fn event_receiver(&self) -> broadcast::Receiver<ConsensusEvent>;
    fn submit_transactions(&self, txs: Vec<EthSignedTx>) -> Result<(), ConsensusError>;
    fn start(&self) -> Pin<Box<dyn Future<Output = Result<(), ConsensusError>> + Send + '_>>;
    fn stop(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// In-process mock network client for simulation.
pub struct InMemoryNetworkClient {
    _peers: std::marker::PhantomData<shared::AuthorityIndex>,
    _latency: Box<dyn LatencyModel>,
}

impl InMemoryNetworkClient {
    pub fn new(latency: impl LatencyModel + 'static) -> Self {
        Self {
            _peers: std::marker::PhantomData,
            _latency: Box::new(latency),
        }
    }
}
