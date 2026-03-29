//! Node crate: top-level wiring of all components.
//!
//! Responsibilities:
//! - Instantiate all crate components
//! - Wire async channels per §4 of docs/interfaces.md
//! - Expose node binary entrypoint (bin/node will call into this)
//! - Read configuration from environment variables / config file

#![allow(dead_code, unused_variables)]

use tokio::sync::{broadcast, mpsc};
use shared::{BackpressureSignal, ConsensusEvent, RoundExecutionResult, TxBatch};

// ---------------------------------------------------------------------------
// Channel capacity constants (§4 채널 배선)
// ---------------------------------------------------------------------------

const CONSENSUS_BROADCAST_CAP: usize = 128;
const SCHEDULER_EXECUTOR_CAP: usize = 32;
const EXECUTOR_SHADOW_CAP: usize = 32;
const BACKPRESSURE_CAP: usize = 8;

// ---------------------------------------------------------------------------
// NodeChannels: all channel endpoints bundled for wiring
// ---------------------------------------------------------------------------

pub struct NodeChannels {
    // consensus → scheduler
    pub consensus_tx: broadcast::Sender<ConsensusEvent>,

    // scheduler → executor
    pub executor_tx: mpsc::Sender<TxBatch>,
    pub executor_rx: mpsc::Receiver<TxBatch>,

    // executor → shadow_state
    pub shadow_tx: mpsc::Sender<RoundExecutionResult>,
    pub shadow_rx: mpsc::Receiver<RoundExecutionResult>,

    // executor → scheduler (backpressure, reverse direction)
    pub backpressure_tx: mpsc::Sender<BackpressureSignal>,
    pub backpressure_rx: mpsc::Receiver<BackpressureSignal>,
}

impl NodeChannels {
    pub fn new() -> Self {
        let (consensus_tx, _) = broadcast::channel(CONSENSUS_BROADCAST_CAP);
        let (executor_tx, executor_rx) = mpsc::channel(SCHEDULER_EXECUTOR_CAP);
        let (shadow_tx, shadow_rx) = mpsc::channel(EXECUTOR_SHADOW_CAP);
        let (backpressure_tx, backpressure_rx) = mpsc::channel(BACKPRESSURE_CAP);

        Self {
            consensus_tx,
            executor_tx,
            executor_rx,
            shadow_tx,
            shadow_rx,
            backpressure_tx,
            backpressure_rx,
        }
    }
}

impl Default for NodeChannels {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// NodeConfig: externalized via env vars / config file (Docker-ready)
// ---------------------------------------------------------------------------

pub struct NodeConfig {
    pub node_index: usize,
    pub committee_size: usize,
    pub consensus_port: u16,
    pub rpc_port: u16,
    pub health_port: u16,
    pub peers: Vec<String>,
}

impl NodeConfig {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self, String> {
        todo!()
    }
}
