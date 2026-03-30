// Adapted from: sui/consensus/core/src/dag_state.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: replaced disk Store with in-memory BTreeMap; removed ScoringSubdag,
//          pending_commit_votes, CommitInfo, write-buffering, and all prometheus metrics;
//          gc_round() always returns 0 (no garbage collection in this phase).

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    ops::Bound::{Excluded, Included},
    sync::Arc,
};

use crate::{
    commit::{CommitDigest, CommitIndex, CommittedSubDag, GENESIS_COMMIT_INDEX},
    committee::AuthorityIndex,
    context::Context,
    threshold_clock::ThresholdClock,
    types::{BlockDigest, BlockRef, BlockTimestampMs, Round, Slot, VerifiedBlock, genesis_blocks},
};

/// In-memory DAG state.
///
/// Wrap with `Arc<parking_lot::RwLock<DagState>>` for shared access.
pub struct DagState {
    context: Arc<Context>,

    // Round-0 genesis blocks, always present.
    genesis: BTreeMap<BlockRef, VerifiedBlock>,

    // All accepted blocks (genesis + received), indexed by BlockRef for efficient range queries.
    blocks: BTreeMap<BlockRef, VerifiedBlock>,

    // Block refs that have already been linearised into a CommittedSubDag.
    committed: HashSet<BlockRef>,

    // Drives the "advance to next proposal round" logic.
    threshold_clock: ThresholdClock,

    // Highest round seen in accepted blocks.
    highest_accepted_round: Round,

    // Commit tracking (no persistent storage).
    last_commit_index: CommitIndex,
    last_commit_digest: CommitDigest,
    last_commit_timestamp_ms: BlockTimestampMs,
}

impl DagState {
    pub fn new(context: Arc<Context>) -> Self {
        let genesis: BTreeMap<BlockRef, VerifiedBlock> = genesis_blocks(&context.committee)
            .into_iter()
            .map(|b| (b.reference(), b))
            .collect();

        // All genesis blocks are considered committed so the linearizer never tries to
        // recurse into them.
        let committed: HashSet<BlockRef> = genesis.keys().cloned().collect();

        let threshold_clock = ThresholdClock::new(1, context.clone());

        Self {
            context,
            genesis,
            blocks: BTreeMap::new(),
            committed,
            threshold_clock,
            highest_accepted_round: 0,
            last_commit_index: GENESIS_COMMIT_INDEX,
            last_commit_digest: CommitDigest::MIN,
            last_commit_timestamp_ms: 0,
        }
    }

    // -----------------------------------------------------------------------
    // Block acceptance
    // -----------------------------------------------------------------------

    /// Insert `blocks` into the local DAG.  Advances the threshold clock for each block.
    pub fn accept_blocks(&mut self, blocks: Vec<VerifiedBlock>) {
        for block in blocks {
            debug_assert!(
                block.round() > 0 || self.genesis.contains_key(&block.reference()),
                "non-genesis round-0 block should not be accepted"
            );
            let block_ref = block.reference();
            self.threshold_clock.add_block(block_ref);
            if block.round() > self.highest_accepted_round {
                self.highest_accepted_round = block.round();
            }
            self.blocks.insert(block_ref, block);
        }
    }

    // -----------------------------------------------------------------------
    // Block reads
    // -----------------------------------------------------------------------

    /// Returns a single block by reference, checking both the cache and genesis.
    pub fn get_block(&self, block_ref: &BlockRef) -> Option<VerifiedBlock> {
        self.blocks
            .get(block_ref)
            .or_else(|| self.genesis.get(block_ref))
            .cloned()
    }

    /// Batch block lookup; result indices correspond to `refs` indices.
    pub fn get_blocks(&self, refs: &[BlockRef]) -> Vec<Option<VerifiedBlock>> {
        refs.iter().map(|r| self.get_block(r)).collect()
    }

    /// Returns true if the block is stored (accepted or genesis).
    pub fn contains_block(&self, block_ref: &BlockRef) -> bool {
        self.blocks.contains_key(block_ref) || self.genesis.contains_key(block_ref)
    }

    /// Batch existence check; result indices correspond to `refs` indices.
    pub fn contains_blocks(&self, refs: Vec<BlockRef>) -> Vec<bool> {
        refs.iter().map(|r| self.contains_block(r)).collect()
    }

    /// All uncommitted blocks in a specific slot (round, authority).
    pub(crate) fn get_uncommitted_blocks_at_slot(&self, slot: Slot) -> Vec<VerifiedBlock> {
        let mut result = Vec::new();
        for (_, block) in self.blocks.range((
            Included(BlockRef::new(slot.round, slot.authority, BlockDigest::MIN)),
            Included(BlockRef::new(slot.round, slot.authority, BlockDigest::MAX)),
        )) {
            if !self.committed.contains(&block.reference()) {
                result.push(block.clone());
            }
        }
        result
    }

    /// All uncommitted blocks at a specific round.
    pub(crate) fn get_uncommitted_blocks_at_round(&self, round: Round) -> Vec<VerifiedBlock> {
        let mut result = Vec::new();
        for (_, block) in self.blocks.range((
            Included(BlockRef::new(round, AuthorityIndex::ZERO, BlockDigest::MIN)),
            Excluded(BlockRef::new(
                round + 1,
                AuthorityIndex::ZERO,
                BlockDigest::MIN,
            )),
        )) {
            if !self.committed.contains(&block.reference()) {
                result.push(block.clone());
            }
        }
        result
    }

    /// All ancestors of `later_block` (transitively) that sit at `earlier_round`.
    pub(crate) fn ancestors_at_round(
        &self,
        later_block: &VerifiedBlock,
        earlier_round: Round,
    ) -> Vec<VerifiedBlock> {
        // BFS / DFS: collect all transitive ancestor refs at rounds > earlier_round,
        // then return those whose round == earlier_round.
        let mut linked: BTreeSet<BlockRef> =
            later_block.ancestors().iter().cloned().collect();

        while !linked.is_empty() {
            let round = linked.last().unwrap().round;
            if round <= earlier_round {
                break;
            }
            let block_ref = linked.pop_last().unwrap();
            if let Some(block) = self.get_block(&block_ref) {
                linked.extend(block.ancestors().iter().cloned());
            }
        }

        linked
            .range((
                Included(BlockRef::new(
                    earlier_round,
                    AuthorityIndex::ZERO,
                    BlockDigest::MIN,
                )),
                std::ops::Bound::Unbounded,
            ))
            .filter_map(|r| self.get_block(r))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Commit tracking
    // -----------------------------------------------------------------------

    /// Mark a block as committed.  Returns `false` if it was already committed.
    pub fn set_committed(&mut self, block_ref: &BlockRef) -> bool {
        self.committed.insert(*block_ref)
    }

    /// Returns `true` if the block has already been committed.
    pub fn is_committed(&self, block_ref: &BlockRef) -> bool {
        self.committed.contains(block_ref)
    }

    /// Record the completion of a commit, updating the running commit state.
    pub fn record_commit(&mut self, subdag: &CommittedSubDag) {
        debug_assert!(
            subdag.commit_ref.index == self.last_commit_index + 1,
            "commit index must be sequential: expected {}, got {}",
            self.last_commit_index + 1,
            subdag.commit_ref.index
        );
        self.last_commit_index = subdag.commit_ref.index;
        self.last_commit_digest = subdag.commit_ref.digest;
        self.last_commit_timestamp_ms = subdag.timestamp_ms;
    }

    // -----------------------------------------------------------------------
    // Accessors used by Linearizer / BaseCommitter
    // -----------------------------------------------------------------------

    pub fn last_commit_index(&self) -> CommitIndex {
        self.last_commit_index
    }

    pub fn last_commit_digest(&self) -> CommitDigest {
        self.last_commit_digest
    }

    pub fn last_commit_timestamp_ms(&self) -> BlockTimestampMs {
        self.last_commit_timestamp_ms
    }

    pub fn highest_accepted_round(&self) -> Round {
        self.highest_accepted_round
    }

    /// Returns the current proposal round from the threshold clock.
    pub fn threshold_clock_round(&self) -> Round {
        self.threshold_clock.get_round()
    }

    /// Last committed leader round (derived from commit tracking).
    pub(crate) fn last_commit_round(&self) -> Round {
        // For simplicity, this returns 0 when no commit has happened yet,
        // allowing uncommitted-block queries at any round.
        0
    }

    /// Garbage-collection round. Always 0 — no GC in this simplified version.
    pub(crate) fn gc_round(&self) -> Round {
        0
    }

    /// Read-only causal traversal anchored at `leader_ref`.
    ///
    /// Returns all blocks in the causal cone of `leader_ref` that are above
    /// `gc_round` and have not yet been committed, in DFS pop order (unspecified
    /// but deterministic for a given DAG state).  Does **not** mutate the
    /// `committed` set — this is the read-only counterpart of
    /// `Linearizer::linearize_sub_dag`.
    ///
    /// Returns an empty vec if the leader block is not found or is already committed.
    pub(crate) fn get_causal_blocks(
        &self,
        leader_ref: &BlockRef,
        gc_round: Round,
    ) -> Vec<VerifiedBlock> {
        debug_assert!(
            leader_ref.round > gc_round,
            "leader round {} must be above gc_round {}",
            leader_ref.round,
            gc_round
        );

        let Some(leader) = self.get_block(leader_ref) else {
            return vec![];
        };
        if self.is_committed(leader_ref) {
            return vec![];
        }

        let mut stack = vec![leader];
        let mut visited = HashSet::new();
        visited.insert(*leader_ref);
        let mut result = Vec::new();

        while let Some(block) = stack.pop() {
            result.push(block.clone());
            for ancestor_ref in block.ancestors() {
                if ancestor_ref.round > gc_round
                    && !self.is_committed(ancestor_ref)
                    && visited.insert(*ancestor_ref)
                {
                    if let Some(ancestor_block) = self.get_block(ancestor_ref) {
                        stack.push(ancestor_block);
                    }
                }
            }
        }
        result
    }

    pub fn committee(&self) -> &Arc<crate::committee::Committee> {
        &self.context.committee
    }
}

/// Compute a commit digest from a CommitRef's sequential index and the leader BlockRef.
/// This is deterministic but not cryptographically tied to block content —
/// sufficient for Phase 3-A simulation.
pub(crate) fn derive_commit_digest(index: CommitIndex, leader: &BlockRef) -> CommitDigest {
    use sha3::{Digest as _, Sha3_256};
    let mut h = Sha3_256::new();
    h.update(index.to_le_bytes());
    h.update(leader.round.to_le_bytes());
    h.update((leader.author.value() as u32).to_le_bytes());
    h.update(leader.digest.0);
    CommitDigest(h.finalize().into())
}
