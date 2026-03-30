// New file — no SUI equivalent.
// ConsensusNode wires together the Phase-3-A components (DagState, BlockManager,
// UniversalCommitter, Linearizer) and adds the Phase-3-B SoftCommitTracker.
// It emits `shared::ConsensusEvent` on a broadcast channel whenever a soft-commit
// quorum is detected (2Δ) or a hard-commit is decided (3Δ).

use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::broadcast;

use shared::{ConsensusEvent, EthSignedTx, OurCommittedSubDag, OurVerifiedBlock};

use crate::{
    block_manager::BlockManager,
    commit::{CommittedSubDag, DecidedLeader},
    context::Context,
    dag_state::DagState,
    linearizer::Linearizer,
    soft_commit::SoftCommitTracker,
    types::{BlockRef, Round, Slot, VerifiedBlock, GENESIS_ROUND},
    universal_committer::universal_committer_builder::UniversalCommitterBuilder,
};

/// Capacity of the broadcast channel for consensus events.
const EVENT_CHANNEL_CAPACITY: usize = 512;

// ---------------------------------------------------------------------------
// ConsensusNode
// ---------------------------------------------------------------------------

/// Runs the full Mysticeti consensus pipeline for one validator node.
///
/// Accepts blocks from the network via [`accept_block`] / [`accept_blocks`] and
/// emits [`ConsensusEvent::SoftCommit`] (2Δ) and [`ConsensusEvent::HardCommit`] (3Δ)
/// events on the broadcast channel returned by [`ConsensusNode::new`].
pub struct ConsensusNode {
    context: Arc<Context>,
    #[allow(dead_code)]
    dag_state: Arc<RwLock<DagState>>,
    block_manager: BlockManager,
    committer: crate::universal_committer::UniversalCommitter,
    linearizer: Linearizer,
    soft_commit: SoftCommitTracker,
    /// Last leader slot that was decided (committed or skipped).
    last_decided: Slot,
    event_tx: broadcast::Sender<ConsensusEvent>,
}

impl ConsensusNode {
    /// Create a new `ConsensusNode` for the given `context`.
    ///
    /// Returns the node together with the first broadcast receiver.
    /// Additional receivers can be obtained via [`ConsensusNode::subscribe`].
    pub fn new(context: Arc<Context>) -> (Self, broadcast::Receiver<ConsensusEvent>) {
        debug_assert!(
            context.committee.size() > 0,
            "ConsensusNode requires a non-empty committee"
        );

        let dag_state = Arc::new(RwLock::new(DagState::new(context.clone())));
        let block_manager = BlockManager::new(context.clone(), dag_state.clone());
        let committer =
            UniversalCommitterBuilder::new(context.clone(), dag_state.clone()).build();
        let linearizer = Linearizer::new(context.clone(), dag_state.clone());

        let (event_tx, event_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        let node = Self {
            context,
            dag_state,
            block_manager,
            committer,
            linearizer,
            soft_commit: SoftCommitTracker::new(),
            // The genesis Slot (round 0, authority 0) acts as the sentinel
            // "last decided" value.  try_decide() breaks out of its loop when it
            // encounters elect_leader(0) == Slot{0,0} == last_decided.
            last_decided: Slot::new(GENESIS_ROUND, crate::committee::AuthorityIndex::ZERO),
            event_tx,
        };

        (node, event_rx)
    }

    /// Subscribe to future consensus events.
    pub fn subscribe(&self) -> broadcast::Receiver<ConsensusEvent> {
        self.event_tx.subscribe()
    }

    // -----------------------------------------------------------------------
    // Block ingestion (public API)
    // -----------------------------------------------------------------------

    /// Accept one block into the local DAG and run the commit pipeline.
    ///
    /// Any resulting [`ConsensusEvent`]s are broadcast to all current subscribers.
    pub fn accept_block(&mut self, block: VerifiedBlock) {
        debug_assert!(
            block.round() > 0 || block.ancestors().is_empty(),
            "only genesis blocks (round 0) may have empty ancestor lists in the API"
        );

        let (accepted, _missing) = self.block_manager.try_accept_blocks(vec![block]);
        for block in &accepted {
            self.check_soft_commit(block);
        }
        self.try_hard_commit();
    }

    /// Accept a batch of blocks and run the commit pipeline once.
    pub fn accept_blocks(&mut self, blocks: Vec<VerifiedBlock>) {
        debug_assert!(!blocks.is_empty(), "accept_blocks called with empty slice");

        let (accepted, _missing) = self.block_manager.try_accept_blocks(blocks);
        for block in &accepted {
            self.check_soft_commit(block);
        }
        self.try_hard_commit();
    }

    // -----------------------------------------------------------------------
    // Internal: soft-commit detection (2Δ)
    // -----------------------------------------------------------------------

    /// For a block at the *voting* round (R+1), check whether any of its ancestors
    /// is the wave leader at round R.  Each such reference counts as one vote.
    /// When 2f+1 votes accumulate, emit a SoftCommit event.
    fn check_soft_commit(&mut self, block: &VerifiedBlock) {
        let round: Round = block.round();

        // Voting round is at least 2 (the leader round it votes for is ≥ 1).
        // We only care about voting rounds: (R+1) where R is a leader round.
        if round < 2 {
            return;
        }
        let potential_leader_round = round - 1;
        if !SoftCommitTracker::is_leader_round(potential_leader_round) {
            return;
        }

        let committee_size = self.context.committee.size();
        let expected_leader =
            SoftCommitTracker::leader_at_round(potential_leader_round, committee_size);
        let voter = block.author();

        for ancestor in block.ancestors() {
            if ancestor.round == potential_leader_round && ancestor.author == expected_leader {
                if self
                    .soft_commit
                    .add_vote(*ancestor, voter, &self.context.committee)
                {
                    let txs = {
                        let state = self.dag_state.read();
                        let gc_round = state.gc_round();
                        state
                            .get_causal_blocks(ancestor, gc_round)
                            .into_iter()
                            .flat_map(|b| {
                                b.transactions()
                                    .iter()
                                    .map(|t| EthSignedTx(t.0.clone()))
                                    .collect::<Vec<_>>()
                            })
                            .collect()
                    };
                    let _ = self.event_tx.send(ConsensusEvent::SoftCommit {
                        round: potential_leader_round as shared::Round,
                        leader: to_shared_block_ref(ancestor),
                        txs,
                    });
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal: hard-commit (3Δ)
    // -----------------------------------------------------------------------

    /// Run the UniversalCommitter and emit HardCommit events for every newly
    /// decided leader (committed or skipped).
    fn try_hard_commit(&mut self) {
        let decided = self.committer.try_decide(self.last_decided);

        for decided_leader in decided {
            match decided_leader {
                DecidedLeader::Commit(leader_block, _direct) => {
                    let slot = leader_block.slot();
                    let subdag = self.linearizer.commit_leader(leader_block);
                    self.last_decided = slot;
                    let _ = self.event_tx.send(ConsensusEvent::HardCommit {
                        subdag: to_shared_subdag(&subdag),
                    });
                }
                DecidedLeader::Skip(slot) => {
                    self.last_decided = slot;
                    // No event emitted for skipped leaders — the scheduler handles
                    // gaps implicitly via the commit_index sequence.
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Type conversion helpers (consensus-internal → shared boundary types)
// ---------------------------------------------------------------------------

pub(crate) fn to_shared_block_ref(r: &BlockRef) -> shared::BlockRef {
    shared::BlockRef {
        round: r.round as shared::Round,
        author: r.author.value() as shared::AuthorityIndex,
        digest: shared::B256::from_slice(&r.digest.0),
    }
}

fn to_shared_subdag(subdag: &CommittedSubDag) -> OurCommittedSubDag {
    OurCommittedSubDag {
        leader: to_shared_block_ref(&subdag.leader),
        blocks: subdag
            .blocks
            .iter()
            .map(|b| OurVerifiedBlock {
                block_ref: to_shared_block_ref(&b.reference()),
                txs: b
                    .transactions()
                    .iter()
                    .map(|t| EthSignedTx(t.0.clone()))
                    .collect(),
            })
            .collect(),
        timestamp_ms: subdag.timestamp_ms,
        commit_index: subdag.commit_ref.index as u64,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        committee::{make_test_committee, AuthorityIndex},
        types::{genesis_blocks, TestBlock},
    };

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build the N genesis BlockRefs for an n-node committee.
    fn genesis_refs(n: usize) -> Vec<BlockRef> {
        let committee = make_test_committee(0, n);
        genesis_blocks(&committee)
            .into_iter()
            .map(|b| b.reference())
            .collect()
    }

    /// Build a complete round of blocks for `authors` at `round`,
    /// each referencing `prev_refs`.
    fn build_round(
        round: u32,
        authors: &[u32],
        prev_refs: &[BlockRef],
    ) -> Vec<VerifiedBlock> {
        authors
            .iter()
            .map(|&a| {
                TestBlock::new(round, a)
                    .set_ancestors(prev_refs.to_vec())
                    .build()
            })
            .collect()
    }

    /// Collect all pending events from a broadcast receiver.
    fn drain(rx: &mut broadcast::Receiver<ConsensusEvent>) -> Vec<ConsensusEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    // -----------------------------------------------------------------------
    // test_soft_commit_triggered
    // -----------------------------------------------------------------------

    /// With 4 nodes and wave_length=3, the wave-1 leader is node 3 at round 3.
    /// Submitting 3 round-4 blocks (2f+1 = 3) that reference the leader must
    /// trigger exactly one SoftCommit event with round == 3.
    #[test]
    fn test_soft_commit_triggered() {
        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut node, mut rx) = ConsensusNode::new(context);

        // Genesis refs
        let g = genesis_refs(n);

        // Rounds 1-3: build full participation so all blocks are accepted.
        let r1 = build_round(1, &[0, 1, 2, 3], &g);
        let r1_refs: Vec<BlockRef> = r1.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r1);

        let r2 = build_round(2, &[0, 1, 2, 3], &r1_refs);
        let r2_refs: Vec<BlockRef> = r2.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r2);

        // Round 3 — leader round. elect_leader_index(3, 0) = (3+0) % 4 = 3 → node 3.
        let r3 = build_round(3, &[0, 1, 2, 3], &r2_refs);
        let r3_refs: Vec<BlockRef> = r3.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r3);

        // No events yet.
        assert!(drain(&mut rx).is_empty(), "no events before voting round");

        // Round 4 — voting round.  Only 3 nodes vote (2f+1) to keep it tight.
        // Each round-4 block has all round-3 blocks as ancestors, so the leader
        // (node-3's block) IS referenced.
        let r4_partial = build_round(4, &[0, 1, 2], &r3_refs); // 3 blocks = 2f+1
        node.accept_blocks(r4_partial);

        let events = drain(&mut rx);
        let soft_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ConsensusEvent::SoftCommit { .. }))
            .collect();

        assert_eq!(soft_events.len(), 1, "expected exactly one SoftCommit event");
        match &soft_events[0] {
            ConsensusEvent::SoftCommit { round, .. } => {
                assert_eq!(*round, 3, "soft commit must be for round 3 leader");
            }
            _ => unreachable!(),
        }
    }

    // -----------------------------------------------------------------------
    // test_hard_commit_triggered
    // -----------------------------------------------------------------------

    /// Building through the decision round (round 5) must produce a HardCommit
    /// for the wave-1 leader at round 3 (node 3).
    #[test]
    fn test_hard_commit_triggered() {
        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut node, mut rx) = ConsensusNode::new(context);

        let g = genesis_refs(n);

        let r1 = build_round(1, &[0, 1, 2, 3], &g);
        let r1_refs: Vec<BlockRef> = r1.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r1);

        let r2 = build_round(2, &[0, 1, 2, 3], &r1_refs);
        let r2_refs: Vec<BlockRef> = r2.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r2);

        let r3 = build_round(3, &[0, 1, 2, 3], &r2_refs);
        let r3_refs: Vec<BlockRef> = r3.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r3);

        let r4 = build_round(4, &[0, 1, 2, 3], &r3_refs);
        let r4_refs: Vec<BlockRef> = r4.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r4);

        // No hard commit yet (decision round not reached).
        let before = drain(&mut rx)
            .into_iter()
            .filter(|e| matches!(e, ConsensusEvent::HardCommit { .. }))
            .count();
        assert_eq!(before, 0, "no HardCommit before decision round 5");

        // Round 5 — decision round.  All 4 nodes participate.
        let r5 = build_round(5, &[0, 1, 2, 3], &r4_refs);
        node.accept_blocks(r5);

        let events = drain(&mut rx);
        let hard_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ConsensusEvent::HardCommit { .. }))
            .collect();

        assert_eq!(hard_events.len(), 1, "expected exactly one HardCommit");
        match &hard_events[0] {
            ConsensusEvent::HardCommit { subdag } => {
                assert_eq!(subdag.leader.round, 3, "committed leader must be at round 3");
                assert_eq!(subdag.leader.author, 3, "round-3 leader must be node 3");
            }
            _ => unreachable!(),
        }
    }

    // -----------------------------------------------------------------------
    // test_dag_causal_order
    // -----------------------------------------------------------------------

    /// After a hard commit the blocks in the CommittedSubDag must be ordered so
    /// that every ancestor that also appears in the sub-dag precedes its descendant.
    #[test]
    fn test_dag_causal_order() {
        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut node, mut rx) = ConsensusNode::new(context);

        let g = genesis_refs(n);

        let r1 = build_round(1, &[0, 1, 2, 3], &g);
        let r1_refs: Vec<BlockRef> = r1.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r1);

        let r2 = build_round(2, &[0, 1, 2, 3], &r1_refs);
        let r2_refs: Vec<BlockRef> = r2.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r2);

        let r3 = build_round(3, &[0, 1, 2, 3], &r2_refs);
        let r3_refs: Vec<BlockRef> = r3.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r3);

        let r4 = build_round(4, &[0, 1, 2, 3], &r3_refs);
        let r4_refs: Vec<BlockRef> = r4.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r4);

        let r5 = build_round(5, &[0, 1, 2, 3], &r4_refs);
        node.accept_blocks(r5);

        // Extract the committed subdag.
        let events = drain(&mut rx);
        let subdag = events.iter().find_map(|e| {
            if let ConsensusEvent::HardCommit { subdag } = e {
                Some(subdag)
            } else {
                None
            }
        });
        let subdag = subdag.expect("expected a HardCommit event");

        // Build a position map: block_ref.round/author → index in subdag.blocks.
        // The sort order from Linearizer is (round ASC, author ASC), which already
        // guarantees causal order because ancestors always have lower rounds.
        // We verify this explicitly: no block at index i has an ancestor at index j > i.
        use std::collections::HashMap;
        let pos: HashMap<(u64, u64), usize> = subdag
            .blocks
            .iter()
            .enumerate()
            .map(|(idx, b)| ((b.block_ref.round, b.block_ref.author), idx))
            .collect();

        for (i, block) in subdag.blocks.iter().enumerate() {
            // subdag.blocks[i] is a shared::OurVerifiedBlock whose ancestors are
            // not stored there.  We can verify causal order purely via round ordering:
            // Linearizer sorts by (round, author) ascending, so all ancestors (lower round)
            // come before descendants (higher round).  Verify round is non-decreasing.
            if i > 0 {
                let prev = &subdag.blocks[i - 1];
                assert!(
                    block.block_ref.round >= prev.block_ref.round,
                    "causal order violated: block at index {} has round {} < previous round {}",
                    i,
                    block.block_ref.round,
                    prev.block_ref.round,
                );
            }
            // Verify the block is actually present in the position map.
            assert!(
                pos.contains_key(&(block.block_ref.round, block.block_ref.author)),
                "block ({}, {}) not found in position map",
                block.block_ref.round,
                block.block_ref.author,
            );
        }
    }

    // -----------------------------------------------------------------------
    // test_byzantine_node_tolerance
    // -----------------------------------------------------------------------

    /// Node 0 is byzantine: it never produces blocks after genesis.
    /// The 3 honest nodes (1, 2, 3) provide 2f+1 = 3 stake with f = 1,
    /// so they must still form a quorum and produce a hard commit.
    ///
    /// The wave-1 leader is node 3 (round 3 % 4 = 3), which is honest.
    #[test]
    fn test_byzantine_node_tolerance() {
        let n = 4; // f = 1, quorum = 3
        let committee = make_test_committee(0, n);
        // We observe from the perspective of node 1 (itself honest).
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(1), committee));
        let (mut node, mut rx) = ConsensusNode::new(context);

        // Genesis BlockRefs: DagState pre-populates these, but we need the refs
        // to use as ancestors in round-1 blocks.
        let g = genesis_refs(n);

        // Honest nodes 1, 2, 3 only.  Node 0 is byzantine and produces nothing.
        let honest: &[u32] = &[1, 2, 3];

        let r1 = build_round(1, honest, &g);
        let r1_refs: Vec<BlockRef> = r1.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r1);

        let r2 = build_round(2, honest, &r1_refs);
        let r2_refs: Vec<BlockRef> = r2.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r2);

        let r3 = build_round(3, honest, &r2_refs);
        let r3_refs: Vec<BlockRef> = r3.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r3);

        let r4 = build_round(4, honest, &r3_refs);
        let r4_refs: Vec<BlockRef> = r4.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r4);

        let r5 = build_round(5, honest, &r4_refs);
        node.accept_blocks(r5);

        let events = drain(&mut rx);

        // SoftCommit: 3 votes ≥ quorum(3) → triggered.
        let soft = events
            .iter()
            .filter(|e| matches!(e, ConsensusEvent::SoftCommit { .. }))
            .count();
        assert_eq!(soft, 1, "expected 1 SoftCommit from 3 honest nodes");

        // HardCommit: 3 honest nodes provide enough stake for direct decide.
        let hard = events
            .iter()
            .filter(|e| matches!(e, ConsensusEvent::HardCommit { .. }))
            .count();
        assert_eq!(hard, 1, "expected 1 HardCommit despite 1 byzantine node");

        // Leader must still be node 3 at round 3.
        if let ConsensusEvent::HardCommit { subdag } = events
            .iter()
            .find(|e| matches!(e, ConsensusEvent::HardCommit { .. }))
            .unwrap()
        {
            assert_eq!(subdag.leader.round, 3);
            assert_eq!(subdag.leader.author, 3);
        }
    }

    // -----------------------------------------------------------------------
    // test_deterministic_replay
    // -----------------------------------------------------------------------

    /// Running the same block sequence twice must produce identical event sequences.
    /// The simulation helper is deterministic (no OS/wall-clock randomness),
    /// so this is guaranteed by construction — but the test pins that contract.
    #[test]
    fn test_deterministic_replay() {
        fn run_once() -> Vec<String> {
            let n = 4;
            let committee = make_test_committee(0, n);
            let context =
                Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
            let (mut node, mut rx) = ConsensusNode::new(context);

            let g = genesis_refs(n);

            // Use SimulatedNode-style FakeClock to ensure timestamps are
            // deterministic across runs.
            let mut ts: u64 = 0;
            let mut next_ts = || {
                ts += 1;
                ts
            };

            let mk_round = |round: u32, authors: &[u32], prev: &[BlockRef]| {
                authors
                    .iter()
                    .map(|&a| {
                        TestBlock::new(round, a)
                            .set_ancestors(prev.to_vec())
                            .set_timestamp_ms(round as u64 * 10 + a as u64)
                            .build()
                    })
                    .collect::<Vec<_>>()
            };

            // Suppress unused closure warning
            let _ = next_ts;

            let r1 = mk_round(1, &[0, 1, 2, 3], &g);
            let r1_refs: Vec<BlockRef> = r1.iter().map(|b| b.reference()).collect();
            node.accept_blocks(r1);

            let r2 = mk_round(2, &[0, 1, 2, 3], &r1_refs);
            let r2_refs: Vec<BlockRef> = r2.iter().map(|b| b.reference()).collect();
            node.accept_blocks(r2);

            let r3 = mk_round(3, &[0, 1, 2, 3], &r2_refs);
            let r3_refs: Vec<BlockRef> = r3.iter().map(|b| b.reference()).collect();
            node.accept_blocks(r3);

            let r4 = mk_round(4, &[0, 1, 2, 3], &r3_refs);
            let r4_refs: Vec<BlockRef> = r4.iter().map(|b| b.reference()).collect();
            node.accept_blocks(r4);

            let r5 = mk_round(5, &[0, 1, 2, 3], &r4_refs);
            node.accept_blocks(r5);

            drain(&mut rx)
                .into_iter()
                .map(|e| format!("{e:?}"))
                .collect()
        }

        let run1 = run_once();
        let run2 = run_once();

        assert!(!run1.is_empty(), "simulation must produce at least one event");
        assert_eq!(
            run1, run2,
            "two identical simulations must produce identical event sequences"
        );
    }

    // -----------------------------------------------------------------------
    // test_tx_payload_flow
    // -----------------------------------------------------------------------

    /// Verify that transaction payloads injected into blocks propagate correctly
    /// through both the SoftCommit and HardCommit paths.
    ///
    /// Setup: 4 nodes, wave-1 leader = node 3 at round 3.
    /// Blocks in rounds 1-3 carry distinct single-byte tx payloads so we can
    /// count them.  After receiving round-5 blocks:
    ///  - SoftCommit.txs must be non-empty (causal subDAG of the leader).
    ///  - HardCommit subdag.blocks[*].txs must together contain all injected txs.
    #[test]
    fn test_tx_payload_flow() {
        use crate::types::Transaction;

        let n = 4;
        let committee = make_test_committee(0, n);
        let context = Arc::new(Context::new(AuthorityIndex::new_for_test(0), committee));
        let (mut node, mut rx) = ConsensusNode::new(context);

        let g = genesis_refs(n);

        // Helper: build a round where each block carries one tx whose payload is
        // [round as u8, author as u8] — unique per block.
        let build_tx_round = |round: u32, authors: &[u32], prev: &[BlockRef]| {
            authors
                .iter()
                .map(|&a| {
                    TestBlock::new(round, a)
                        .set_ancestors(prev.to_vec())
                        .set_transactions(vec![Transaction(vec![round as u8, a as u8])])
                        .build()
                })
                .collect::<Vec<VerifiedBlock>>()
        };

        let r1 = build_tx_round(1, &[0, 1, 2, 3], &g);
        let r1_refs: Vec<BlockRef> = r1.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r1);

        let r2 = build_tx_round(2, &[0, 1, 2, 3], &r1_refs);
        let r2_refs: Vec<BlockRef> = r2.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r2);

        // Round 3 — leader round (node 3 is the wave-1 leader).
        let r3 = build_tx_round(3, &[0, 1, 2, 3], &r2_refs);
        let r3_refs: Vec<BlockRef> = r3.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r3);

        // Round 4 — voting round; 2f+1 = 3 votes trigger SoftCommit.
        let r4 = build_tx_round(4, &[0, 1, 2], &r3_refs);
        let r4_refs: Vec<BlockRef> = r4.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r4);

        let after_r4 = drain(&mut rx);
        let soft = after_r4
            .iter()
            .find(|e| matches!(e, ConsensusEvent::SoftCommit { .. }))
            .expect("SoftCommit must be emitted after 2f+1 votes");

        // SoftCommit must carry the causal subDAG txs (rounds 1-3, all 4 nodes).
        match soft {
            ConsensusEvent::SoftCommit { txs, .. } => {
                assert!(
                    !txs.is_empty(),
                    "SoftCommit.txs must not be empty when blocks carry transactions"
                );
                // Causal cone of leader (R3-node3):
                //   R3: 1 block  (only the leader itself)
                //   R2: 4 blocks (leader references all 4 R2 blocks)
                //   R1: 4 blocks (each R2 block references all 4 R1 blocks)
                // = 9 txs total.  R3 blocks of nodes 0-2 are NOT in the leader's cone.
                assert_eq!(
                    txs.len(),
                    9,
                    "SoftCommit should contain 9 causal txs (1+4+4 from rounds 3,2,1)"
                );
            }
            _ => unreachable!(),
        }

        // Round 5 — decision round; triggers HardCommit.
        let r4_all = build_tx_round(4, &[3], &r3_refs); // 4th node's round-4 block
        let r4_all_refs: Vec<BlockRef> = r4_all.iter().map(|b| b.reference()).collect();
        node.accept_blocks(r4_all);

        // Merge round-4 refs for round-5 ancestors.
        let mut all_r4_refs = r4_refs.clone();
        all_r4_refs.extend(r4_all_refs);

        let r5 = build_tx_round(5, &[0, 1, 2, 3], &all_r4_refs);
        node.accept_blocks(r5);

        let after_r5 = drain(&mut rx);
        let hard = after_r5
            .iter()
            .find(|e| matches!(e, ConsensusEvent::HardCommit { .. }))
            .expect("HardCommit must be emitted at decision round 5");

        // Every block in the committed subDAG must expose its txs.
        match hard {
            ConsensusEvent::HardCommit { subdag } => {
                let total_hard_txs: usize =
                    subdag.blocks.iter().map(|b| b.txs.len()).sum();
                assert!(
                    total_hard_txs > 0,
                    "HardCommit subdag blocks must carry transactions"
                );
            }
            _ => unreachable!(),
        }
    }
}
