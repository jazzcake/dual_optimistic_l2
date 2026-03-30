//! Node crate: top-level wiring of all components.
//!
//! Responsibilities:
//! - Instantiate all crate components
//! - Wire async channels per §4 of docs/interfaces.md
//! - Expose node binary entrypoint (bin/node will call into this)
//! - Read configuration from environment variables / config file

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
use shared::{BackpressureSignal, ConsensusEvent, RoundExecutionResult, TxBatch};
use consensus::ConsensusNode;
use scheduler::{CommitDecision, PipelineScheduler};
use parallel_evm::{ExecutorEvent, MockExecutor};

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
    ///
    /// Environment variables (all optional, with defaults):
    /// - `NODE_INDEX` — zero-based node index (default: 0)
    /// - `COMMITTEE_SIZE` — total number of validators (default: 4)
    /// - `CONSENSUS_PORT` — port for consensus P2P (default: 9000)
    /// - `RPC_PORT` — port for JSON-RPC endpoint (default: 8545)
    /// - `HEALTH_PORT` — port for health / metrics endpoint (default: 9001)
    /// - `PEERS` — comma-separated peer addresses (default: empty)
    pub fn from_env() -> Result<Self, String> {
        let parse_usize = |key: &str, default: &str| -> Result<usize, String> {
            std::env::var(key)
                .unwrap_or_else(|_| default.to_owned())
                .parse::<usize>()
                .map_err(|e| format!("invalid {key}: {e}"))
        };
        let parse_u16 = |key: &str, default: &str| -> Result<u16, String> {
            std::env::var(key)
                .unwrap_or_else(|_| default.to_owned())
                .parse::<u16>()
                .map_err(|e| format!("invalid {key}: {e}"))
        };

        let node_index = parse_usize("NODE_INDEX", "0")?;
        let committee_size = parse_usize("COMMITTEE_SIZE", "4")?;
        let consensus_port = parse_u16("CONSENSUS_PORT", "9000")?;
        let rpc_port = parse_u16("RPC_PORT", "8545")?;
        let health_port = parse_u16("HEALTH_PORT", "9001")?;
        let peers = std::env::var("PEERS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned())
            .collect();

        Ok(Self {
            node_index,
            committee_size,
            consensus_port,
            rpc_port,
            health_port,
            peers,
        })
    }
}

// ---------------------------------------------------------------------------
// MockCommitWrapper: logs CommitDecisions for test inspection
// ---------------------------------------------------------------------------

/// Commit decision event as logged by [`MockCommitWrapper`].
#[derive(Debug, Clone)]
pub enum CommitEvent {
    /// Speculative execution was correct; commit the cached state diff.
    Commit { commit_index: u64, round: shared::Round },
    /// Speculative result must be discarded.
    Discard { commit_index: u64 },
    /// No speculative run existed; fresh batch was supplied for execution.
    FreshBatch { round: shared::Round, commit_index: u64, tx_count: usize },
}

/// Mock commit wrapper for integration tests.
///
/// Consumes [`CommitDecision`] messages from [`PipelineScheduler`] and
/// records them in a shared log that tests can inspect.
pub struct MockCommitWrapper {
    events: Arc<Mutex<Vec<CommitEvent>>>,
}

impl MockCommitWrapper {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns a shared handle to the event log.  Clone this **before**
    /// calling [`run`][Self::run].
    pub fn events(&self) -> Arc<Mutex<Vec<CommitEvent>>> {
        Arc::clone(&self.events)
    }

    /// Consume the wrapper and run its event loop until `commit_rx` closes.
    pub async fn run(self, mut commit_rx: mpsc::Receiver<CommitDecision>) {
        while let Some(decision) = commit_rx.recv().await {
            let event = match decision {
                CommitDecision::Commit { commit_index, round } => {
                    CommitEvent::Commit { commit_index, round }
                }
                CommitDecision::Discard { commit_index } => {
                    CommitEvent::Discard { commit_index }
                }
                CommitDecision::FreshBatch(batch) => CommitEvent::FreshBatch {
                    round: batch.round,
                    commit_index: batch.commit_index,
                    tx_count: batch.txs.len(),
                },
            };
            self.events.lock().unwrap().push(event);
        }
    }
}

impl Default for MockCommitWrapper {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// NodeHandle: test-facing handle returned by build_test_node()
// ---------------------------------------------------------------------------

/// Test handle giving access to all pipeline components after `build_test_node`.
pub struct NodeHandle {
    /// The consensus node — drive the pipeline via `accept_blocks()`.
    pub consensus: ConsensusNode,
    /// Shared log of TxBatches seen by the MockExecutor.
    pub executor_events: Arc<Mutex<Vec<ExecutorEvent>>>,
    /// Shared log of CommitDecisions seen by the MockCommitWrapper.
    pub commit_events: Arc<Mutex<Vec<CommitEvent>>>,
}

/// Wire a complete single-node test pipeline:
/// ```text
/// ConsensusNode ──broadcast──▶ PipelineScheduler ──TxBatch──▶ MockExecutor
///                                    ▲ CommitDecision          │
///                                    │                         │ BackpressureSignal
///                              MockCommitWrapper               │
///                                                             ▼
///                              (backpressure_rx) ◀─────────────┘
/// ```
///
/// Returns the [`NodeHandle`] for block injection and assertions, plus a
/// `Vec` of spawned [`tokio::task::JoinHandle`]s (scheduler, executor,
/// commit wrapper).  The caller is responsible for awaiting or aborting them.
pub fn build_test_node(
    context: std::sync::Arc<consensus::Context>,
) -> (NodeHandle, Vec<tokio::task::JoinHandle<()>>) {
    let (consensus, _first_rx) = ConsensusNode::new(context);
    let scheduler_rx = consensus.subscribe();

    let (executor_tx, executor_rx) = mpsc::channel::<TxBatch>(SCHEDULER_EXECUTOR_CAP);
    let (commit_tx, commit_rx) = mpsc::channel::<CommitDecision>(32);
    let (backpressure_tx, backpressure_rx) = mpsc::channel::<BackpressureSignal>(BACKPRESSURE_CAP);

    let (scheduler, _outbound_bp_rx) = PipelineScheduler::new(
        scheduler_rx,
        executor_tx,
        backpressure_rx,
        commit_tx,
    );

    let mock_executor = MockExecutor::new();
    let executor_events = mock_executor.events();

    let mock_commit = MockCommitWrapper::new();
    let commit_events = mock_commit.events();

    let handles = vec![
        tokio::spawn(scheduler.run()),
        tokio::spawn(mock_executor.run(executor_rx, backpressure_tx)),
        tokio::spawn(mock_commit.run(commit_rx)),
    ];

    let handle = NodeHandle {
        consensus,
        executor_events,
        commit_events,
    };
    (handle, handles)
}

/// Same as [`build_test_node`] but injects an artificial per-batch executor
/// delay (milliseconds) to exercise the backpressure path.
pub fn build_test_node_with_delay(
    context: std::sync::Arc<consensus::Context>,
    delay_ms: u64,
) -> (NodeHandle, Vec<tokio::task::JoinHandle<()>>) {
    let (consensus, _first_rx) = ConsensusNode::new(context);
    let scheduler_rx = consensus.subscribe();

    let (executor_tx, executor_rx) = mpsc::channel::<TxBatch>(SCHEDULER_EXECUTOR_CAP);
    let (commit_tx, commit_rx) = mpsc::channel::<CommitDecision>(32);
    let (backpressure_tx, backpressure_rx) = mpsc::channel::<BackpressureSignal>(BACKPRESSURE_CAP);

    let (scheduler, _outbound_bp_rx) = PipelineScheduler::new(
        scheduler_rx,
        executor_tx,
        backpressure_rx,
        commit_tx,
    );

    let mock_executor = MockExecutor::new_with_delay(delay_ms);
    let executor_events = mock_executor.events();

    let mock_commit = MockCommitWrapper::new();
    let commit_events = mock_commit.events();

    let handles = vec![
        tokio::spawn(scheduler.run()),
        tokio::spawn(mock_executor.run(executor_rx, backpressure_tx)),
        tokio::spawn(mock_commit.run(commit_rx)),
    ];

    let handle = NodeHandle {
        consensus,
        executor_events,
        commit_events,
    };
    (handle, handles)
}

// ---------------------------------------------------------------------------
// E2E integration tests (5-C)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use consensus::{
        make_test_committee, AuthorityIndex, Context, TestBlock, Transaction,
        BlockRef, VerifiedBlock, genesis_blocks,
    };
    use shared::ConsensusEvent;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn genesis_refs(n: usize) -> Vec<BlockRef> {
        let committee = make_test_committee(0, n);
        genesis_blocks(&committee)
            .into_iter()
            .map(|b| b.reference())
            .collect()
    }

    fn build_round(round: u32, authors: &[u32], prev: &[BlockRef]) -> Vec<VerifiedBlock> {
        authors
            .iter()
            .map(|&a| {
                TestBlock::new(round, a)
                    .set_ancestors(prev.to_vec())
                    .build()
            })
            .collect()
    }

    fn build_round_tx(round: u32, authors: &[u32], prev: &[BlockRef]) -> Vec<VerifiedBlock> {
        // Each block carries one tx with payload [round, author].
        authors
            .iter()
            .map(|&a| {
                TestBlock::new(round, a)
                    .set_ancestors(prev.to_vec())
                    .set_transactions(vec![Transaction(vec![round as u8, a as u8])])
                    .build()
            })
            .collect()
    }

    fn refs_of(blocks: &[VerifiedBlock]) -> Vec<BlockRef> {
        blocks.iter().map(|b| b.reference()).collect()
    }

    /// Yield the tokio scheduler enough times for spawned tasks to drain all
    /// pending channel messages.
    async fn yield_all() {
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
    }

    // -----------------------------------------------------------------------
    // test_e2e_single_round
    // -----------------------------------------------------------------------

    /// 4-node, 1 wave.  Verify:
    ///  - Executor receives an optimistic TxBatch (SoftCommit at R4).
    ///  - Commit wrapper receives a Commit decision (HardCommit at R5).
    #[tokio::test]
    async fn test_e2e_single_round() {
        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let g = genesis_refs(n);

        let r1 = build_round(1, &[0, 1, 2, 3], &g);   let r1r = refs_of(&r1); handle.consensus.accept_blocks(r1);
        let r2 = build_round(2, &[0, 1, 2, 3], &r1r); let r2r = refs_of(&r2); handle.consensus.accept_blocks(r2);
        let r3 = build_round(3, &[0, 1, 2, 3], &r2r); let r3r = refs_of(&r3); handle.consensus.accept_blocks(r3);

        // R4 voting round: 3 blocks → 2f+1 votes → SoftCommit R3.
        let r4a = build_round(4, &[0, 1, 2], &r3r);
        let r4ar = refs_of(&r4a);
        handle.consensus.accept_blocks(r4a);

        // 4th R4 block completes the round.
        let r4b = build_round(4, &[3], &r3r);
        let mut r4r = r4ar.clone();
        r4r.extend(refs_of(&r4b));
        handle.consensus.accept_blocks(r4b);

        // R5 decision round → HardCommit R3.
        let r5 = build_round(5, &[0, 1, 2, 3], &r4r);
        handle.consensus.accept_blocks(r5);

        yield_all().await;

        let ex = handle.executor_events.lock().unwrap().clone();
        assert!(
            ex.iter().any(|e| e.is_optimistic),
            "executor must receive at least one optimistic TxBatch from SoftCommit"
        );

        let cm = handle.commit_events.lock().unwrap().clone();
        assert!(
            cm.iter().any(|e| matches!(e, CommitEvent::Commit { .. })),
            "commit wrapper must receive a Commit decision after HardCommit"
        );
    }

    // -----------------------------------------------------------------------
    // test_e2e_multi_round
    // -----------------------------------------------------------------------

    /// 3 consecutive waves (leaders at R3, R6, R9).  Verify the executor
    /// receives 3 optimistic batches with ascending commit_index (1, 2, 3)
    /// and the commit wrapper receives 3 Commit decisions.
    #[tokio::test]
    async fn test_e2e_multi_round() {
        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let g = genesis_refs(n);
        let all_nodes: &[u32] = &[0, 1, 2, 3];

        // R1 to R11 — 3 full waves.
        let r1  = build_round(1,  all_nodes, &g);    let r1r  = refs_of(&r1);  handle.consensus.accept_blocks(r1);
        let r2  = build_round(2,  all_nodes, &r1r);  let r2r  = refs_of(&r2);  handle.consensus.accept_blocks(r2);
        let r3  = build_round(3,  all_nodes, &r2r);  let r3r  = refs_of(&r3);  handle.consensus.accept_blocks(r3);
        let r4  = build_round(4,  all_nodes, &r3r);  let r4r  = refs_of(&r4);  handle.consensus.accept_blocks(r4);
        let r5  = build_round(5,  all_nodes, &r4r);  let r5r  = refs_of(&r5);  handle.consensus.accept_blocks(r5);
        let r6  = build_round(6,  all_nodes, &r5r);  let r6r  = refs_of(&r6);  handle.consensus.accept_blocks(r6);
        let r7  = build_round(7,  all_nodes, &r6r);  let r7r  = refs_of(&r7);  handle.consensus.accept_blocks(r7);
        let r8  = build_round(8,  all_nodes, &r7r);  let r8r  = refs_of(&r8);  handle.consensus.accept_blocks(r8);
        let r9  = build_round(9,  all_nodes, &r8r);  let r9r  = refs_of(&r9);  handle.consensus.accept_blocks(r9);
        let r10 = build_round(10, all_nodes, &r9r);  let r10r = refs_of(&r10); handle.consensus.accept_blocks(r10);
        let r11 = build_round(11, all_nodes, &r10r);                            handle.consensus.accept_blocks(r11);

        yield_all().await;

        let ex = handle.executor_events.lock().unwrap().clone();
        let mut ci: Vec<u64> = ex
            .iter()
            .filter(|e| e.is_optimistic)
            .map(|e| e.commit_index)
            .collect();
        assert_eq!(ci.len(), 3, "expected 3 optimistic TxBatches; got {:?}", ex);
        ci.sort();
        assert_eq!(ci, vec![1, 2, 3], "commit_indexes must be 1, 2, 3");

        let cm = handle.commit_events.lock().unwrap().clone();
        let n_commits = cm
            .iter()
            .filter(|e| matches!(e, CommitEvent::Commit { .. }))
            .count();
        assert_eq!(n_commits, 3, "expected 3 Commit decisions for 3 waves");
    }

    // -----------------------------------------------------------------------
    // test_e2e_soft_hard_tx_match
    // -----------------------------------------------------------------------

    /// Blocks carry tx payloads.  Verify that `SoftCommit.txs` equals the
    /// aggregated txs in the HardCommit subdag (byte-sorted comparison).
    #[tokio::test]
    async fn test_e2e_soft_hard_tx_match() {
        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        // Subscribe BEFORE feeding blocks so we capture every event.
        let mut event_rx = handle.consensus.subscribe();

        let g = genesis_refs(n);
        let all_nodes: &[u32] = &[0, 1, 2, 3];

        let r1 = build_round_tx(1, all_nodes, &g);    let r1r = refs_of(&r1); handle.consensus.accept_blocks(r1);
        let r2 = build_round_tx(2, all_nodes, &r1r);  let r2r = refs_of(&r2); handle.consensus.accept_blocks(r2);
        let r3 = build_round_tx(3, all_nodes, &r2r);  let r3r = refs_of(&r3); handle.consensus.accept_blocks(r3);

        let r4a = build_round_tx(4, &[0, 1, 2], &r3r);
        let r4ar = refs_of(&r4a);
        handle.consensus.accept_blocks(r4a);

        let r4b = build_round_tx(4, &[3], &r3r);
        let mut r4r = r4ar.clone();
        r4r.extend(refs_of(&r4b));
        handle.consensus.accept_blocks(r4b);

        let r5 = build_round_tx(5, all_nodes, &r4r);
        handle.consensus.accept_blocks(r5);

        yield_all().await;

        let mut events = Vec::new();
        while let Ok(e) = event_rx.try_recv() {
            events.push(e);
        }

        let soft_txs = events
            .iter()
            .find_map(|e| {
                if let ConsensusEvent::SoftCommit { txs, .. } = e {
                    Some(txs.clone())
                } else {
                    None
                }
            })
            .expect("SoftCommit must be present");
        assert!(!soft_txs.is_empty(), "SoftCommit.txs must not be empty");

        let hard_txs: Vec<Vec<u8>> = events
            .iter()
            .find_map(|e| {
                if let ConsensusEvent::HardCommit { subdag } = e {
                    Some(
                        subdag
                            .blocks
                            .iter()
                            .flat_map(|b| b.txs.iter().map(|t| t.0.clone()))
                            .collect(),
                    )
                } else {
                    None
                }
            })
            .expect("HardCommit must be present");
        assert!(!hard_txs.is_empty(), "HardCommit subdag must carry txs");

        let mut soft_sorted: Vec<Vec<u8>> = soft_txs.iter().map(|t| t.0.clone()).collect();
        let mut hard_sorted = hard_txs;
        soft_sorted.sort();
        hard_sorted.sort();
        assert_eq!(soft_sorted, hard_sorted, "SoftCommit.txs must equal HardCommit subdag txs");
    }

    // -----------------------------------------------------------------------
    // test_e2e_out_of_order
    // -----------------------------------------------------------------------

    /// SoftCommit R6 arrives at the scheduler before R3.  The PendingQueue must
    /// buffer R6 and dispatch to the executor in order: R3 first, then R6.
    ///
    /// Sequence:
    ///   1. R1-R3 full.
    ///   2. 2 R4 blocks (insufficient votes, no SoftCommit R3 yet).
    ///   3. R5-R7 with {0,1,2}: R7 → 3 votes for R6-node2 → SoftCommit R6.
    ///   4. 3rd R4 block (node 2) → SoftCommit R3.
    #[tokio::test]
    async fn test_e2e_out_of_order() {
        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let g = genesis_refs(n);

        let r1 = build_round(1, &[0, 1, 2, 3], &g);   let r1r = refs_of(&r1); handle.consensus.accept_blocks(r1);
        let r2 = build_round(2, &[0, 1, 2, 3], &r1r); let r2r = refs_of(&r2); handle.consensus.accept_blocks(r2);
        let r3 = build_round(3, &[0, 1, 2, 3], &r2r); let r3r = refs_of(&r3); handle.consensus.accept_blocks(r3);

        // 2 R4 blocks — insufficient for SoftCommit R3.
        let r4p = build_round(4, &[0, 1], &r3r);
        let r4pr = refs_of(&r4p);
        handle.consensus.accept_blocks(r4p);

        // R5-R7 with {0,1,2}; R6-node2 is wave-2 leader (6 % 4 = 2).
        let r5 = build_round(5, &[0, 1, 2], &r4pr);   let r5r = refs_of(&r5); handle.consensus.accept_blocks(r5);
        let r6 = build_round(6, &[0, 1, 2], &r5r);    let r6r = refs_of(&r6); handle.consensus.accept_blocks(r6);
        // 3 R7 blocks each referencing R6-node2 → SoftCommit R6 fires here.
        let r7 = build_round(7, &[0, 1, 2], &r6r);
        handle.consensus.accept_blocks(r7);

        // 3rd R4 block (node 2) → SoftCommit R3 fires.
        let r4_node2 = build_round(4, &[2], &r3r);
        handle.consensus.accept_blocks(r4_node2);

        yield_all().await;

        let ex = handle.executor_events.lock().unwrap().clone();
        let rounds: Vec<u64> = ex.iter().map(|e| e.round).collect();
        assert!(
            rounds.len() >= 2,
            "executor must receive batches for R3 and R6; got: {:?}",
            rounds
        );
        assert_eq!(rounds[0], 3, "R3 must be dispatched first");
        assert_eq!(rounds[1], 6, "R6 must be dispatched second");
    }

    // -----------------------------------------------------------------------
    // test_e2e_backpressure
    // -----------------------------------------------------------------------

    /// 10 ms per-batch executor delay exercises the SlowDown/Resume path.
    /// Both wave TxBatches must eventually be processed.
    #[tokio::test]
    async fn test_e2e_backpressure() {
        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node_with_delay(context, 10);

        let g = genesis_refs(n);
        let all_nodes: &[u32] = &[0, 1, 2, 3];

        let r1 = build_round(1, all_nodes, &g);   let r1r = refs_of(&r1); handle.consensus.accept_blocks(r1);
        let r2 = build_round(2, all_nodes, &r1r); let r2r = refs_of(&r2); handle.consensus.accept_blocks(r2);
        let r3 = build_round(3, all_nodes, &r2r); let r3r = refs_of(&r3); handle.consensus.accept_blocks(r3);
        let r4 = build_round(4, all_nodes, &r3r); let r4r = refs_of(&r4); handle.consensus.accept_blocks(r4);
        let r5 = build_round(5, all_nodes, &r4r); let r5r = refs_of(&r5); handle.consensus.accept_blocks(r5);
        let r6 = build_round(6, all_nodes, &r5r); let r6r = refs_of(&r6); handle.consensus.accept_blocks(r6);
        let r7 = build_round(7, all_nodes, &r6r); let r7r = refs_of(&r7); handle.consensus.accept_blocks(r7);
        let r8 = build_round(8, all_nodes, &r7r);                          handle.consensus.accept_blocks(r8);

        // Wait for both 10 ms delays plus processing overhead.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let ex = handle.executor_events.lock().unwrap().clone();
        let optimistic: Vec<u64> = ex
            .iter()
            .filter(|e| e.is_optimistic)
            .map(|e| e.round)
            .collect();
        assert!(
            optimistic.len() >= 2,
            "both optimistic batches must complete despite delay; got: {:?}",
            optimistic
        );
    }

    // -----------------------------------------------------------------------
    // Large-committee helpers
    // -----------------------------------------------------------------------

    /// Wave length constant — must match `DEFAULT_WAVE_LENGTH` in consensus crate.
    const WAVE_LEN: u32 = 3;

    /// Minimal seeded LCG (Knuth's MMIX parameters) for deterministic test randomness.
    ///
    /// State advances with `s' = s * A + C (mod 2^64)`.
    struct Lcg(u64);

    impl Lcg {
        fn new(seed: u64) -> Self { Self(seed) }

        fn next_u64(&mut self) -> u64 {
            self.0 = self.0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }

        fn next_usize(&mut self, n: usize) -> usize {
            debug_assert!(n > 0);
            (self.next_u64() % n as u64) as usize
        }

        /// Fisher-Yates in-place shuffle.
        fn shuffle<T>(&mut self, slice: &mut [T]) {
            for i in (1..slice.len()).rev() {
                let j = self.next_usize(i + 1);
                slice.swap(i, j);
            }
        }

        /// Sample `k` items from `items` without replacement (random order).
        fn choose_k<T: Copy>(&mut self, items: &[T], k: usize) -> Vec<T> {
            let k = k.min(items.len());
            let mut idx: Vec<usize> = (0..items.len()).collect();
            self.shuffle(&mut idx);
            idx[..k].iter().map(|&i| items[i]).collect()
        }
    }

    /// Round-robin leader authority index at a leader round.
    fn leader_author_for_round(leader_round: u32, committee_size: usize) -> usize {
        (leader_round % committee_size as u32) as usize
    }

    /// Build round `r` blocks where each block randomly selects `refs_per_block`
    /// ancestors from `prev` using the given RNG.
    ///
    /// **Voting-round guarantee**: when the previous round is a leader round
    /// (i.e., `r - 1` is divisible by `WAVE_LEN`), the wave leader's block from
    /// `prev` is **always** inserted into every block's ancestor list.  This
    /// ensures that all `authors.len()` blocks cast a vote for the leader,
    /// guaranteeing a SoftCommit.
    ///
    /// For all other rounds ancestors are chosen purely at random.
    fn build_round_random_refs(
        r: u32,
        authors: &[u32],
        prev: &[BlockRef],
        committee_size: usize,
        refs_per_block: usize,
        rng: &mut Lcg,
    ) -> Vec<VerifiedBlock> {
        debug_assert!(!prev.is_empty(), "prev must not be empty");
        debug_assert!(refs_per_block > 0, "refs_per_block must be positive");

        let prev_round = r.saturating_sub(1);
        // Is this a voting round? (prev round == leader round)
        let forced_leader: Option<BlockRef> =
            if prev_round > 0 && prev_round % WAVE_LEN == 0 {
                let la = leader_author_for_round(prev_round, committee_size);
                prev.iter().find(|b| b.author.value() == la).copied()
            } else {
                None
            };

        authors.iter().map(|&a| {
            let mut chosen = rng.choose_k(prev, refs_per_block);

            // Guarantee the leader is in the ancestor list on voting rounds.
            if let Some(lr) = forced_leader {
                if !chosen.iter().any(|b| *b == lr) {
                    match chosen.last_mut() {
                        Some(slot) => *slot = lr,
                        None => chosen.push(lr),
                    }
                }
            }

            TestBlock::new(r, a).set_ancestors(chosen).build()
        }).collect()
    }

    // -----------------------------------------------------------------------
    // test_8node_20waves_full_connectivity
    // -----------------------------------------------------------------------

    /// 8-node committee, 20 consecutive waves with full block connectivity.
    ///
    /// Runs rounds 1 – 62 (= 20 × WAVE_LEN + 2 decision-round overshoot).
    /// Every round all 8 nodes produce blocks that reference every block from
    /// the previous round.
    ///
    /// Assertions:
    /// - Executor receives exactly 20 optimistic TxBatches.
    /// - Commit indexes form the sequence 1 .. 20 (ascending, no gaps).
    /// - Commit wrapper records exactly 20 `Commit` decisions.
    #[tokio::test]
    async fn test_8node_20waves_full_connectivity() {
        let n = 8;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let all_nodes: Vec<u32> = (0..n as u32).collect();
        // 20 waves: leader at R3, R6, …, R60.  Decision round for wave 20 = R62.
        let total_rounds: u32 = WAVE_LEN * 20 + 2;

        let mut prev = genesis_refs(n);
        for r in 1..=total_rounds {
            let blocks = build_round(r, &all_nodes, &prev);
            prev = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }

        for _ in 0..400 {
            tokio::task::yield_now().await;
        }

        let ex = handle.executor_events.lock().unwrap().clone();
        let mut ci: Vec<u64> = ex
            .iter()
            .filter(|e| e.is_optimistic)
            .map(|e| e.commit_index)
            .collect();
        ci.sort();
        assert_eq!(ci.len(), 20, "expected 20 optimistic TxBatches; got ci={ci:?}");
        assert_eq!(
            ci,
            (1u64..=20).collect::<Vec<_>>(),
            "commit_indexes must be exactly 1..=20"
        );

        let cm = handle.commit_events.lock().unwrap().clone();
        let n_commits = cm
            .iter()
            .filter(|e| matches!(e, CommitEvent::Commit { .. }))
            .count();
        assert_eq!(n_commits, 20, "expected 20 Commit decisions; got cm={cm:?}");
    }

    // -----------------------------------------------------------------------
    // test_8node_random_ancestors_10waves
    // -----------------------------------------------------------------------

    /// 8-node committee, 10 waves, seeded random ancestor selection.
    ///
    /// Each block picks exactly `quorum` (5) random ancestors from the previous
    /// round.  On voting rounds the wave leader's block is **always** forced into
    /// the ancestor list, guaranteeing all 8 blocks cast a vote for the leader.
    /// On decision rounds any 5-block random selection from the voting round
    /// constitutes a certificate (all voting-round blocks are votes).
    ///
    /// Seed: 42.  Rounds 1 – 32 (wave 10 leader R30, decision R32).
    ///
    /// Assertions:
    /// - Exactly 10 optimistic TxBatches with commit_indexes 1 .. 10.
    /// - Exactly 10 Commit decisions.
    #[tokio::test]
    async fn test_8node_random_ancestors_10waves() {
        let n = 8usize;
        // Byzantine quorum: n - floor((n-1)/3) = 8 - 2 = 6.
        // (Committee computes quorum_threshold = total_stake - fault_tolerance.)
        let quorum = n - (n - 1) / 3;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let all_nodes: Vec<u32> = (0..n as u32).collect();
        let mut rng = Lcg::new(42);

        // Rounds 1..32: wave 10 leader at R30, decision at R32.
        let mut prev = genesis_refs(n);
        for r in 1u32..=32 {
            let blocks =
                build_round_random_refs(r, &all_nodes, &prev, n, quorum, &mut rng);
            prev = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }

        for _ in 0..400 {
            tokio::task::yield_now().await;
        }

        let ex = handle.executor_events.lock().unwrap().clone();
        let mut ci: Vec<u64> = ex
            .iter()
            .filter(|e| e.is_optimistic)
            .map(|e| e.commit_index)
            .collect();
        ci.sort();
        assert_eq!(
            ci.len(), 10,
            "expected 10 optimistic TxBatches (random-ref); got ci={ci:?}"
        );
        assert_eq!(
            ci,
            (1u64..=10).collect::<Vec<_>>(),
            "commit_indexes must be exactly 1..=10"
        );

        let cm = handle.commit_events.lock().unwrap().clone();
        let n_commits = cm
            .iter()
            .filter(|e| matches!(e, CommitEvent::Commit { .. }))
            .count();
        assert_eq!(
            n_commits, 10,
            "expected 10 Commit decisions (random-ref); got cm={cm:?}"
        );
    }

    // -----------------------------------------------------------------------
    // test_8node_sparse_participation_random
    // -----------------------------------------------------------------------

    /// 8-node committee, 15 waves, randomly varying per-round participation.
    ///
    /// Each round a seeded LCG selects 5–8 nodes to participate (at least
    /// `quorum` = 5 to maintain liveness).  Wave leaders are **always** forced
    /// to participate at their leader round so that a leader block exists for
    /// voting-round blocks to reference.
    ///
    /// Since all participating voting-round blocks use full connectivity within
    /// the participating set (which always includes the leader's block from the
    /// previous round), every wave obtains a SoftCommit and a HardCommit.
    ///
    /// Seed: 99.  Rounds 1 – 47 (wave 15 leader R45, decision R47).
    ///
    /// Assertions:
    /// - Exactly 15 optimistic TxBatches with monotonically increasing
    ///   commit_indexes.
    /// - Exactly 15 Commit decisions.
    #[tokio::test]
    async fn test_8node_sparse_participation_random() {
        let n = 8usize;
        // Byzantine quorum: n - floor((n-1)/3) = 8 - 2 = 6.
        let quorum = n - (n - 1) / 3;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let mut rng = Lcg::new(99);

        // Rounds 1..47: wave 15 leader at R45, decision at R47.
        let mut prev = genesis_refs(n);
        for r in 1u32..=47 {
            // Random participation count in [quorum, n].
            let n_part = quorum + rng.next_usize(n - quorum + 1);

            // Shuffle all node indices and take the first n_part.
            let mut all: Vec<u32> = (0..n as u32).collect();
            rng.shuffle(&mut all);
            let mut participants: Vec<u32> = all[..n_part].to_vec();

            // Leader round: the wave leader *must* produce a block so that
            // voting-round blocks in the next round can reference it.
            if r % WAVE_LEN == 0 {
                let la = (r % n as u32) as u32;
                if !participants.contains(&la) {
                    *participants.last_mut().unwrap() = la;
                    participants.dedup();
                }
            }

            participants.sort();

            // Full connectivity within the participating set.
            // `prev` contains only the blocks from the previous round that
            // actually participated — all of them are in the DAG.
            let blocks = build_round(r, &participants, &prev);
            prev = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }

        for _ in 0..500 {
            tokio::task::yield_now().await;
        }

        let ex = handle.executor_events.lock().unwrap().clone();
        let ci: Vec<u64> = ex
            .iter()
            .filter(|e| e.is_optimistic)
            .map(|e| e.commit_index)
            .collect();

        assert_eq!(
            ci.len(), 15,
            "expected 15 optimistic TxBatches (sparse); got ci={ci:?}"
        );

        // Commit indexes must be strictly ascending.
        for w in ci.windows(2) {
            assert!(
                w[0] < w[1],
                "commit_indexes must be strictly increasing: {ci:?}"
            );
        }

        let cm = handle.commit_events.lock().unwrap().clone();
        let n_commits = cm
            .iter()
            .filter(|e| matches!(e, CommitEvent::Commit { .. }))
            .count();
        assert_eq!(
            n_commits, 15,
            "expected 15 Commit decisions (sparse); got cm={cm:?}"
        );
    }

    // -----------------------------------------------------------------------
    // test_e2e_byzantine_f1
    // -----------------------------------------------------------------------

    /// 4-node committee, f=1.  Node 0 is byzantine (no blocks after genesis).
    /// 3 honest nodes (1, 2, 3) must drive the pipeline to a commit decision.
    #[tokio::test]
    async fn test_e2e_byzantine_f1() {
        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(1), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let g = genesis_refs(n);
        let honest: &[u32] = &[1, 2, 3];

        let r1 = build_round(1, honest, &g);   let r1r = refs_of(&r1); handle.consensus.accept_blocks(r1);
        let r2 = build_round(2, honest, &r1r); let r2r = refs_of(&r2); handle.consensus.accept_blocks(r2);
        let r3 = build_round(3, honest, &r2r); let r3r = refs_of(&r3); handle.consensus.accept_blocks(r3);
        let r4 = build_round(4, honest, &r3r); let r4r = refs_of(&r4); handle.consensus.accept_blocks(r4);
        let r5 = build_round(5, honest, &r4r);                          handle.consensus.accept_blocks(r5);

        yield_all().await;

        let cm = handle.commit_events.lock().unwrap().clone();
        assert!(
            cm.iter().any(|e| matches!(
                e,
                CommitEvent::Commit { .. } | CommitEvent::FreshBatch { .. }
            )),
            "commit wrapper must receive a commit-path decision; got: {:?}",
            cm
        );
    }

    // -----------------------------------------------------------------------
    // count_commit_decisions — shared helper for partition/rejoin/equivocation
    // -----------------------------------------------------------------------

    fn count_commit_decisions(handle: &NodeHandle) -> usize {
        handle
            .commit_events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| {
                matches!(e, CommitEvent::Commit { .. } | CommitEvent::FreshBatch { .. })
            })
            .count()
    }

    // -----------------------------------------------------------------------
    // test_network_partition_and_heal
    // -----------------------------------------------------------------------

    /// 8-node committee.  Simulates a symmetric network partition: nodes 0-3
    /// (group A) can only see their own blocks, and nodes 4-7 (group B) can
    /// only see their own blocks for rounds 10-18 (3 waves).
    ///
    /// Neither half reaches quorum=6 independently:
    ///  - `enough_leader_support` = 0 for all partition-era waves (the
    ///    4-block voting sub-group never reaches the 6-block certificate
    ///    threshold).
    ///  - `enough_leader_blame` = 4 < 6, so waves are Undecided, not Skipped.
    ///
    /// After the partition heals at round 19, full connectivity is restored.
    /// The first directly committed wave (R21) also indirectly decides the
    /// three undecided partition-era waves (as Skip — neither sub-group had
    /// enough certificates), so commits resume with no permanent gaps.
    ///
    /// Phases:
    ///  1 (R1-R11):  full connectivity → 3 commits (wave R9's decision round is R11)
    ///  2 (R12-R20): partition         → 0 new commits
    ///  3 (R21-R29): heal              → ≥ 1 new commit
    ///
    /// Note: `try_decide` only considers leaders up to `highest_accepted_round - 2`,
    /// so phase 1 must include the decision round (R11) for wave 3 (leader R9).
    #[tokio::test]
    async fn test_network_partition_and_heal() {
        let n = 8usize;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let all_nodes: Vec<u32> = (0..n as u32).collect();
        let group_a: Vec<u32> = (0..4u32).collect();
        let group_b: Vec<u32> = (4..8u32).collect();

        // Phase 1: full connectivity, rounds 1-11.
        // R11 is the decision round for wave 3 (leader R9), completing 3 commits.
        let mut prev = genesis_refs(n);
        for r in 1u32..=11 {
            let blocks = build_round(r, &all_nodes, &prev);
            prev = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }
        for _ in 0..400 { tokio::task::yield_now().await; }
        assert_eq!(
            count_commit_decisions(&handle), 3,
            "expected 3 commits after full-connectivity phase"
        );

        // Phase 2: partition, rounds 12-20.
        // Covers waves R12/R15/R18 and their decision rounds R14/R17/R20 — all partition-era.
        // Group A only references group-A prev, group B only references group-B prev.
        // Each sub-group has 4 nodes < quorum=6, so no wave can commit or skip.
        let mut prev_a: Vec<BlockRef> = prev.iter()
            .filter(|r| r.author.value() < 4).cloned().collect();
        let mut prev_b: Vec<BlockRef> = prev.iter()
            .filter(|r| r.author.value() >= 4).cloned().collect();

        for r in 12u32..=20 {
            let blocks_a = build_round(r, &group_a, &prev_a);
            let blocks_b = build_round(r, &group_b, &prev_b);
            prev_a = refs_of(&blocks_a);
            prev_b = refs_of(&blocks_b);
            handle.consensus.accept_blocks(blocks_a);
            handle.consensus.accept_blocks(blocks_b);
        }
        for _ in 0..400 { tokio::task::yield_now().await; }
        assert_eq!(
            count_commit_decisions(&handle), 3,
            "no new commits during partition (each sub-group has only 4 < quorum=6 nodes)"
        );

        // Phase 3: partition heals, rounds 21-29.
        // Combine both groups' prev refs to restore full cross-group connectivity.
        // R21/R24/R27 commit directly; R12/R15/R18 are indirectly decided Skip
        // (partition-era certificate counts never reach quorum=6).
        let mut prev_all: Vec<BlockRef> = prev_a.into_iter().chain(prev_b).collect();
        for r in 21u32..=29 {
            let blocks = build_round(r, &all_nodes, &prev_all);
            prev_all = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }
        for _ in 0..500 { tokio::task::yield_now().await; }
        let total = count_commit_decisions(&handle);
        assert!(
            total > 3,
            "commits must resume after partition heals; total={total}"
        );
    }

    // -----------------------------------------------------------------------
    // test_node_offline_and_rejoin
    // -----------------------------------------------------------------------

    /// 8-node committee.  Node 7 goes offline from rounds 10-27 (6 waves)
    /// and rejoins at round 28.
    ///
    /// The remaining 7 nodes always satisfy quorum=6, so the pipeline
    /// continues with only one interruption: wave at R15 (leader = 15%8 = 7)
    /// is *skipped* because the leader block is absent and all 7 voting
    /// blocks blame the leader.  The other 5 waves in the offline period
    /// produce normal commits.
    ///
    /// After rejoin, node 7's first block legally references the 7-block
    /// `prev` from round 27 — the DAG does not require consecutive rounds
    /// per node, so causal ancestry is valid and commits continue normally.
    ///
    /// Phases:
    ///  1 (R1-R11):  all 8 nodes  → 3 commits (includes R9's decision round R11)
    ///  2 (R12-R29): node 7 off   → 5 commits + 1 skip (R15: leader=7 absent)
    ///  3 (R30-R38): node 7 back  → 3 more commits
    #[tokio::test]
    async fn test_node_offline_and_rejoin() {
        let n = 8usize;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let all_nodes: Vec<u32> = (0..n as u32).collect();
        let without_7: Vec<u32> = (0..7u32).collect();

        // Phase 1: all 8 online, rounds 1-11, 3 commits.
        // R11 is the decision round for wave 3 (leader R9); needed for the
        // committer range `highest_accepted_round - 2` to cover R9.
        let mut prev = genesis_refs(n);
        for r in 1u32..=11 {
            let blocks = build_round(r, &all_nodes, &prev);
            prev = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }
        for _ in 0..400 { tokio::task::yield_now().await; }
        assert_eq!(count_commit_decisions(&handle), 3);

        // Phase 2: node 7 offline, rounds 12-29.
        // `prev` carries only 7 BlockRefs per round; node 7 produces no blocks.
        // Wave at R15 (leader = 15%8=7, absent): all 7 voting-round blocks have no
        // ancestor with author=7 → enough_leader_blame = 7 ≥ 6 → Skip (no event).
        // Waves R12, R18, R21, R24, R27: leaders present, 7 nodes ≥ quorum=6 → Commit.
        for r in 12u32..=29 {
            let blocks = build_round(r, &without_7, &prev);
            prev = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }
        for _ in 0..500 { tokio::task::yield_now().await; }
        let after_offline = count_commit_decisions(&handle);
        // 3 pre-offline + 5 offline commits (R12, R18, R21, R24, R27).
        assert!(
            after_offline >= 8,
            "pipeline must continue with 7/8 nodes; got after_offline={after_offline}"
        );

        // Phase 3: node 7 rejoins at round 30.
        // Its first block references the 7-block prev from round 29 — valid,
        // because the BlockManager only requires declared ancestors to exist.
        for r in 30u32..=38 {
            let blocks = build_round(r, &all_nodes, &prev);
            prev = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }
        for _ in 0..500 { tokio::task::yield_now().await; }
        let after_rejoin = count_commit_decisions(&handle);
        assert!(
            after_rejoin > after_offline,
            "commits must increase after node 7 rejoins; {after_rejoin} > {after_offline}"
        );

        // The PendingQueue dispatches SoftCommit batches in consecutive wave order
        // (next_dispatch_round advances by WAVE_LENGTH each time).  When R15 is
        // *skipped* (no SoftCommit — leader absent), the queue's pointer stays at
        // R15 and all subsequent SoftCommit insertions for R18, R21, ... are
        // buffered but never dispatched optimistically.  Those waves fall back to
        // FreshExecution (CommitDecision::FreshBatch), which is still counted by
        // count_commit_decisions above.
        //
        // Optimistic batches that DO dispatch: R3, R6, R9 (phase 1) + R12 (last
        // wave before the skip blocks the queue) = 4.
        let ex = handle.executor_events.lock().unwrap().clone();
        let n_opt = ex.iter().filter(|e| e.is_optimistic).count();
        assert!(
            n_opt >= 3,
            "at least 3 optimistic batches (phase-1 waves R3/R6/R9); got {n_opt}"
        );
    }

    // -----------------------------------------------------------------------
    // test_equivocating_voter
    // -----------------------------------------------------------------------

    /// 4-node committee.  Node 1 equivocates at the voting round (R4):
    /// it produces two distinct blocks V1a and V1b with the same (round=4,
    /// author=1) but different transaction payloads → different digests.
    /// Both blocks reference the wave leader (node 3 at R3) so both are
    /// technically votes for the leader.
    ///
    /// The `StakeAggregator` tracks votes per `AuthorityIndex` and ignores
    /// the second submission from the same authority.  Node 1 therefore
    /// contributes exactly one stake unit.  With unique voters {0, 1, 2} the
    /// quorum=3 threshold is reached and SoftCommit fires; node 3's R4 vote
    /// arrives after the quorum is already met.
    ///
    /// In the decision round R5, blocks reference all five R4 refs (including
    /// both V1a and V1b).  `is_certificate` also uses per-authority staking,
    /// so V1b's stake is merged with V1a → 3 unique voter authors ≥ quorum=3
    /// → certificate.  HardCommit fires and a single Commit decision is
    /// recorded — equivocation is absorbed without panic or duplication.
    #[tokio::test]
    async fn test_equivocating_voter() {
        let n = 4usize;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let all_nodes: Vec<u32> = (0..n as u32).collect();

        // Rounds 1-3: full connectivity.  Wave leader at R3 = node 3%4 = 3.
        let mut prev = genesis_refs(n);
        for r in 1u32..=3 {
            let blocks = build_round(r, &all_nodes, &prev);
            prev = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }

        // R4 (voting): node 1 equivocates.
        // V1a and V1b share the same ancestors (including the R3 leader)
        // but differ in their transaction payload → different block digests.
        let r4_node0 = TestBlock::new(4, 0).set_ancestors(prev.clone()).build();
        let v1a = TestBlock::new(4, 1)
            .set_ancestors(prev.clone())
            .set_transactions(vec![Transaction(vec![1, 0])])
            .build();
        let v1b = TestBlock::new(4, 1)
            .set_ancestors(prev.clone())
            .set_transactions(vec![Transaction(vec![1, 1])])  // different tx → different digest
            .build();
        let r4_node2 = TestBlock::new(4, 2).set_ancestors(prev.clone()).build();
        let r4_node3 = TestBlock::new(4, 3).set_ancestors(prev.clone()).build();

        // Feed both equivocating blocks.  The duplicate vote from author=1 is silently ignored.
        handle.consensus.accept_blocks(vec![
            r4_node0.clone(), v1a.clone(), v1b.clone(), r4_node2.clone(), r4_node3.clone(),
        ]);

        // R5 (decision): all 4 nodes reference all five R4 refs (node0, V1a, V1b, node2, node3).
        // `is_certificate` stacks per-author: authors {0,1,2} yield 3 = quorum=3 → certificate.
        let mut r5_prev = refs_of(&[r4_node0, r4_node2, r4_node3]);
        r5_prev.push(v1a.reference());
        r5_prev.push(v1b.reference());
        let r5 = build_round(5, &all_nodes, &r5_prev);
        handle.consensus.accept_blocks(r5);

        for _ in 0..300 { tokio::task::yield_now().await; }

        // Exactly one commit — equivocating voter does not produce duplicate commits.
        let cm = handle.commit_events.lock().unwrap().clone();
        let n_commits = count_commit_decisions(&handle);
        assert_eq!(
            n_commits, 1,
            "exactly one commit despite voter equivocation; events: {cm:?}"
        );

        // SoftCommit fires once when the 3rd unique voter (node 2) is processed.
        let ex = handle.executor_events.lock().unwrap().clone();
        let n_opt = ex.iter().filter(|e| e.is_optimistic).count();
        assert_eq!(n_opt, 1,
            "exactly one optimistic batch; got {n_opt}");
    }

    // -----------------------------------------------------------------------
    // test_equivocating_leader
    // -----------------------------------------------------------------------

    /// 8-node committee.  The wave-1 leader (node 3 at R3) equivocates:
    /// it produces two distinct blocks L1 and L2 with the same (round=3,
    /// author=3) but different transaction payloads → different digests.
    ///
    /// Vote split:
    ///  - Nodes 0-5 (6 voters) reference L1 at their R4 voting blocks.
    ///  - Nodes 6-7 (2 voters) reference L2.
    ///
    /// L1 accumulates 6 votes = quorum=6 → SoftCommit fires for L1.
    /// L2 accumulates 2 votes < quorum=6  → no SoftCommit for L2.
    ///
    /// In the decision round R5, all 8 blocks reference all R4 blocks.
    /// For each R5 block, the 6 L1-vote ancestors {0,1,2,3,4,5} yield 6
    /// unique author stakes ≥ quorum=6 → certificate for L1.  `try_direct_decide`
    /// finds [L1] has enough support and [L2] does not → `Commit(L1)` returned
    /// (the `len > 1` panic guard is never triggered).
    ///
    /// The system commits exactly once for L1 and silently discards L2 — no
    /// panic, no double commit.
    #[tokio::test]
    async fn test_equivocating_leader() {
        let n = 8usize;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut handle, _tasks) = build_test_node(context);

        let all_nodes: Vec<u32> = (0..n as u32).collect();

        // R1-R2: full connectivity.
        let mut prev = genesis_refs(n);
        for r in 1u32..=2 {
            let blocks = build_round(r, &all_nodes, &prev);
            prev = refs_of(&blocks);
            handle.consensus.accept_blocks(blocks);
        }

        // R3 (leader round): node 3 equivocates — L1 and L2, same slot, different tx.
        let r3_others: Vec<VerifiedBlock> = (0..n as u32).filter(|&a| a != 3).map(|a| {
            TestBlock::new(3, a).set_ancestors(prev.clone()).build()
        }).collect();
        let l1 = TestBlock::new(3, 3)
            .set_ancestors(prev.clone())
            .set_transactions(vec![Transaction(vec![0])])
            .build();
        let l2 = TestBlock::new(3, 3)
            .set_ancestors(prev.clone())
            .set_transactions(vec![Transaction(vec![1])])   // different tx → different digest
            .build();

        let r3_other_refs = refs_of(&r3_others);
        handle.consensus.accept_blocks(r3_others);
        // Both L1 and L2 are injected; the DAG stores them under separate BlockRefs.
        handle.consensus.accept_blocks(vec![l1.clone(), l2.clone()]);

        // R4 (voting): split votes.
        // Nodes 0-5 reference L1 (6 voters ≥ quorum=6 → SoftCommit for L1).
        // Nodes 6-7 reference L2 (2 voters < quorum=6 → no SoftCommit for L2).
        let r4_l1_voters: Vec<VerifiedBlock> = (0..6u32).map(|a| {
            let mut ancs = r3_other_refs.clone();
            ancs.push(l1.reference());
            TestBlock::new(4, a).set_ancestors(ancs).build()
        }).collect();
        let r4_l2_voters: Vec<VerifiedBlock> = (6..8u32).map(|a| {
            let mut ancs = r3_other_refs.clone();
            ancs.push(l2.reference());
            TestBlock::new(4, a).set_ancestors(ancs).build()
        }).collect();

        let mut r4r = refs_of(&r4_l1_voters);
        r4r.extend(refs_of(&r4_l2_voters));

        handle.consensus.accept_blocks(r4_l1_voters);
        handle.consensus.accept_blocks(r4_l2_voters);

        // R5 (decision): all 8 nodes reference all 8 R4 blocks.
        // Each R5 block has ancestors {0,1,2,3,4,5} as L1-votes → 6 unique authors
        // ≥ quorum=6 → certificate for L1.
        let r5 = build_round(5, &all_nodes, &r4r);
        handle.consensus.accept_blocks(r5);

        for _ in 0..400 { tokio::task::yield_now().await; }

        // Exactly one commit for L1; L2 is discarded without panic.
        let cm = handle.commit_events.lock().unwrap().clone();
        let n_commits = count_commit_decisions(&handle);
        assert_eq!(
            n_commits, 1,
            "exactly one commit for L1; L2 must be silently discarded; events: {cm:?}"
        );

        // SoftCommit must have fired for L1 (6 votes = quorum).
        let ex = handle.executor_events.lock().unwrap().clone();
        let n_opt = ex.iter().filter(|e| e.is_optimistic).count();
        assert_eq!(n_opt, 1,
            "exactly one optimistic batch for L1; got {n_opt}");
    }
}
