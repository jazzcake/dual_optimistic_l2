//! Shared domain types crossing crate boundaries.
//!
//! Primitive types (Address, U256, B256, AccountInfo, StorageKey, StorageValue)
//! are re-exported from revm-primitives / revm-state.
//! Domain types specific to this project are defined here.

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// Re-export revm primitive types
// ---------------------------------------------------------------------------

pub use revm_primitives::{Address, B256, StorageKey, StorageValue, U256};
pub use revm_state::AccountInfo;

// ---------------------------------------------------------------------------
// Stable project-level aliases
// ---------------------------------------------------------------------------

/// Round number in the Mysticeti DAG.
pub type Round = u64;
pub type AuthorityIndex = u64;

/// 32-byte transaction hash (alias for B256).
pub type TxHash = B256;

/// 32-byte block digest (alias for B256).
pub type BlockDigest = B256;

/// Raw EIP-2718 encoded Ethereum transaction.
#[derive(Debug, Clone)]
pub struct EthSignedTx(pub Vec<u8>);

// ---------------------------------------------------------------------------
// DAG references
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BlockRef {
    pub round: Round,
    pub author: AuthorityIndex,
    pub digest: BlockDigest,
}

// ---------------------------------------------------------------------------
// Consensus events (consensus → scheduler)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ConsensusEvent {
    /// Optimistic pre-commit at 2Δ: wave leader detected.
    /// Transactions may be executed speculatively.
    SoftCommit {
        round: Round,
        leader: BlockRef,
        txs: Vec<EthSignedTx>,
    },
    /// Final commit at 3Δ: subDAG committed.
    /// Conflicting speculative results must be discarded.
    HardCommit { subdag: OurCommittedSubDag },
}

#[derive(Debug, Clone)]
pub struct OurCommittedSubDag {
    pub leader: BlockRef,
    pub blocks: Vec<OurVerifiedBlock>,
    pub timestamp_ms: u64,
    pub commit_index: u64,
}

#[derive(Debug, Clone)]
pub struct OurVerifiedBlock {
    pub block_ref: BlockRef,
    pub txs: Vec<EthSignedTx>,
}

// ---------------------------------------------------------------------------
// Scheduler → Executor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TxBatch {
    pub round: Round,
    pub commit_index: u64,
    pub txs: Vec<EthSignedTx>,
    /// `true` = SoftCommit-based (speculative), `false` = HardCommit-based (final).
    pub is_optimistic: bool,
}

// ---------------------------------------------------------------------------
// Executor → Shadow state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StateDiff {
    pub round: Round,
    pub commit_index: u64,
    pub is_optimistic: bool,
    pub changes: std::collections::HashMap<Address, AccountDiff>,
}

/// Per-account change set produced by a single transaction.
#[derive(Debug, Clone, Default)]
pub struct AccountDiff {
    pub balance: Option<U256>,
    pub nonce: Option<u64>,
    pub code: Option<Vec<u8>>,
    /// Slot-level write set (storage slot granularity).
    pub storage: std::collections::HashMap<StorageKey, StorageValue>,
}

// ---------------------------------------------------------------------------
// Execution results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RoundExecutionResult {
    pub round: Round,
    pub commit_index: u64,
    pub is_optimistic: bool,
    pub results: Vec<TxExecutionResult>,
    pub state_diff: StateDiff,
    /// Indices into `results` of transactions with R/W conflicts.
    pub conflict_txs: Vec<usize>,
}

#[derive(Debug, Clone)]
pub enum TxExecutionResult {
    Success { tx_hash: TxHash, gas_used: u64 },
    Revert { tx_hash: TxHash, gas_used: u64, reason: Vec<u8> },
    Invalid { tx_hash: TxHash, error: String },
}

// ---------------------------------------------------------------------------
// Backpressure (executor → scheduler, reverse channel)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackpressureSignal {
    /// Executor queue is filling up; pause SoftCommit dispatch.
    SlowDown,
    /// Executor has capacity; resume normal dispatch.
    Resume,
}

// ---------------------------------------------------------------------------
// R/W conflict tracking types (slot granularity)
// ---------------------------------------------------------------------------

pub type ReadSet = std::collections::HashSet<(Address, StorageKey)>;
pub type WriteSet = std::collections::HashMap<(Address, StorageKey), StorageValue>;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ConsensusError(pub String);

#[derive(Debug)]
pub struct ExecutorError(pub String);

#[derive(Debug)]
pub struct CommitError(pub String);

#[derive(Debug)]
pub struct DbError(pub String);
