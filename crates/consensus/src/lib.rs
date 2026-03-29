//! Consensus crate: Mysticeti DAG wrapper.
//!
//! Responsibilities:
//! - Wrap Mysticeti consensus engine (extern/sui submodule)
//! - Detect SoftCommit (2Δ wave leader) and HardCommit (3Δ subDAG)
//! - Emit `ConsensusEvent` via broadcast channel
//! - Provide `InMemoryNetworkClient` for deterministic simulation

#![allow(dead_code, unused_variables)]

use std::{future::Future, pin::Pin, time::Duration};
use tokio::sync::broadcast;
use shared::{AuthorityIndex, ConsensusError, ConsensusEvent, EthSignedTx};

// ---------------------------------------------------------------------------
// ConsensusHandle trait (D1: SoftCommit hook lives inside this crate)
//
// Boxed futures are used so the trait is dyn compatible (required by
// SimulatedNode which stores Box<dyn ConsensusHandle>).
// ---------------------------------------------------------------------------

pub trait ConsensusHandle: Send + Sync {
    /// Subscribe to consensus events (SoftCommit / HardCommit).
    fn event_receiver(&self) -> broadcast::Receiver<ConsensusEvent>;

    /// Submit Ethereum transactions into the consensus pipeline.
    fn submit_transactions(&self, txs: Vec<EthSignedTx>) -> Result<(), ConsensusError>;

    /// Start the consensus engine (spawns internal tasks).
    fn start(&self) -> Pin<Box<dyn Future<Output = Result<(), ConsensusError>> + Send + '_>>;

    /// Gracefully shut down the consensus engine.
    fn stop(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

// ---------------------------------------------------------------------------
// LatencyModel: pluggable network delay for simulation
// ---------------------------------------------------------------------------

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
        // Phase 3: implement random delay within [min, max]
        todo!()
    }
}

// ---------------------------------------------------------------------------
// InMemoryNetworkClient: in-process ValidatorNetworkClient for simulation
// ---------------------------------------------------------------------------

/// In-process mock network client.
/// Phase 3 will implement the actual `ValidatorNetworkClient` trait from SUI.
pub struct InMemoryNetworkClient {
    // Phase 3: HashMap<AuthorityIndex, mpsc::Sender<NetworkMessage>>
    _peers: std::marker::PhantomData<AuthorityIndex>,
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
