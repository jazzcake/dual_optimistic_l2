// Adapted from: sui/consensus/core/src/block_manager.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: removed mysten_metrics::monitored_scope, all prometheus metrics fields,
//          received_block_rounds metrics reporting; kept core causal-history suspension logic.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use parking_lot::RwLock;
use tracing::{debug, trace, warn};

use crate::{
    context::Context,
    dag_state::DagState,
    types::{BlockRef, Round, VerifiedBlock, GENESIS_ROUND},
};

struct SuspendedBlock {
    block: VerifiedBlock,
    missing_ancestors: BTreeSet<BlockRef>,
}

impl SuspendedBlock {
    fn new(block: VerifiedBlock, missing_ancestors: BTreeSet<BlockRef>) -> Self {
        Self {
            block,
            missing_ancestors,
        }
    }
}

enum TryAcceptResult {
    Accepted(VerifiedBlock),
    Suspended(BTreeSet<BlockRef>),
    Processed,
}

/// Suspends incoming blocks until their full causal history is available,
/// then returns them in causally-ordered batches to be added to DagState.
pub(crate) struct BlockManager {
    context: Arc<Context>,
    dag_state: Arc<RwLock<DagState>>,

    /// Blocks waiting for missing ancestors.
    suspended_blocks: BTreeMap<BlockRef, SuspendedBlock>,
    /// Missing ancestor ref → set of suspended blocks that need it.
    missing_ancestors: BTreeMap<BlockRef, BTreeSet<BlockRef>>,
    /// The subset of missing_ancestors not yet fetched (i.e. not in suspended_blocks).
    missing_blocks: BTreeSet<BlockRef>,
}

impl BlockManager {
    pub(crate) fn new(context: Arc<Context>, dag_state: Arc<RwLock<DagState>>) -> Self {
        debug_assert!(
            context.committee.size() > 0,
            "cannot create BlockManager with empty committee"
        );
        Self {
            context,
            dag_state,
            suspended_blocks: BTreeMap::new(),
            missing_ancestors: BTreeMap::new(),
            missing_blocks: BTreeSet::new(),
        }
    }

    /// Try to accept `blocks`.  Returns (accepted_blocks, missing_ancestor_refs).
    pub(crate) fn try_accept_blocks(
        &mut self,
        blocks: Vec<VerifiedBlock>,
    ) -> (Vec<VerifiedBlock>, BTreeSet<BlockRef>) {
        self.try_accept_blocks_internal(blocks, false)
    }

    /// Accept blocks that are known to be committed (no missing-ancestor tracking).
    pub(crate) fn try_accept_committed_blocks(
        &mut self,
        blocks: Vec<VerifiedBlock>,
    ) -> Vec<VerifiedBlock> {
        let (accepted, missing) = self.try_accept_blocks_internal(blocks, true);
        assert!(
            missing.is_empty(),
            "committed blocks must not have missing ancestors"
        );
        accepted
    }

    fn try_accept_blocks_internal(
        &mut self,
        mut blocks: Vec<VerifiedBlock>,
        committed: bool,
    ) -> (Vec<VerifiedBlock>, BTreeSet<BlockRef>) {
        blocks.sort_by_key(|b| b.round());

        if !blocks.is_empty() {
            debug!(
                "Trying to accept blocks: {}",
                blocks
                    .iter()
                    .map(|b| b.reference().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let mut accepted_blocks = Vec::new();
        let mut missing_blocks_out = BTreeSet::new();

        for block in blocks {
            let block_ref = block.reference();

            if committed {
                match self.try_accept_one_committed_block(block) {
                    TryAcceptResult::Accepted(b) => {
                        accepted_blocks.push(b);
                    }
                    TryAcceptResult::Processed => continue,
                    TryAcceptResult::Suspended(_) => {
                        panic!("committed block should never be suspended: {block_ref:?}");
                    }
                }
            } else {
                let mut blocks_to_accept = Vec::new();
                match self.try_accept_one_block(block) {
                    TryAcceptResult::Accepted(b) => {
                        blocks_to_accept.push(b);
                    }
                    TryAcceptResult::Suspended(missing) => {
                        debug!(
                            "Suspended block {block_ref}: missing {}",
                            missing
                                .iter()
                                .map(|r| r.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        missing_blocks_out.extend(missing);
                        continue;
                    }
                    TryAcceptResult::Processed => continue,
                }

                // Unsuspend any children that were waiting on this block.
                let unsuspended = self.try_unsuspend_children_blocks(block_ref);
                blocks_to_accept.extend(unsuspended);

                self.dag_state
                    .write()
                    .accept_blocks(blocks_to_accept.clone());
                accepted_blocks.extend(blocks_to_accept);
            }
        }

        (accepted_blocks, missing_blocks_out)
    }

    fn try_accept_one_committed_block(&mut self, block: VerifiedBlock) -> TryAcceptResult {
        let block_ref = block.reference();
        if self.dag_state.read().contains_block(&block_ref) {
            return TryAcceptResult::Processed;
        }

        self.missing_blocks.remove(&block_ref);

        if let Some(suspended) = self.suspended_blocks.remove(&block_ref) {
            for ancestor in &suspended.missing_ancestors {
                if let Some(refs) = self.missing_ancestors.get_mut(ancestor) {
                    refs.remove(&block_ref);
                }
            }
        }

        self.dag_state.write().accept_blocks(vec![block.clone()]);
        TryAcceptResult::Accepted(block)
    }

    fn try_accept_one_block(&mut self, block: VerifiedBlock) -> TryAcceptResult {
        let block_ref = block.reference();

        // Already in DagState?
        if self.dag_state.read().contains_block(&block_ref) {
            return TryAcceptResult::Processed;
        }

        // Identify missing ancestors.
        let gc_round = self.dag_state.read().gc_round();
        let mut missing = BTreeSet::new();

        for ancestor_ref in block.ancestors() {
            if ancestor_ref.round == GENESIS_ROUND {
                // Genesis blocks are always present.
                continue;
            }
            if ancestor_ref.round <= gc_round {
                // Below GC horizon — treated as present.
                continue;
            }
            if !self.dag_state.read().contains_block(ancestor_ref)
                && !self.suspended_blocks.contains_key(ancestor_ref)
            {
                missing.insert(*ancestor_ref);
            }
        }

        if !missing.is_empty() {
            // Register this block as needing its missing ancestors.
            for ancestor_ref in &missing {
                self.missing_ancestors
                    .entry(*ancestor_ref)
                    .or_default()
                    .insert(block_ref);
                self.missing_blocks.insert(*ancestor_ref);

                warn!(
                    "Block {block_ref} missing ancestor {ancestor_ref}, suspending"
                );
            }
            let suspended = SuspendedBlock::new(block, missing.clone());
            self.suspended_blocks.insert(block_ref, suspended);
            return TryAcceptResult::Suspended(missing);
        }

        TryAcceptResult::Accepted(block)
    }

    /// Try to unsuspend blocks that were waiting for `newly_accepted`.
    fn try_unsuspend_children_blocks(
        &mut self,
        newly_accepted: BlockRef,
    ) -> Vec<VerifiedBlock> {
        let Some(waiting) = self.missing_ancestors.remove(&newly_accepted) else {
            return Vec::new();
        };

        let mut unsuspended = Vec::new();
        for child_ref in waiting {
            let Some(suspended) = self.suspended_blocks.get_mut(&child_ref) else {
                continue;
            };
            suspended.missing_ancestors.remove(&newly_accepted);
            if suspended.missing_ancestors.is_empty() {
                let block = self
                    .suspended_blocks
                    .remove(&child_ref)
                    .unwrap()
                    .block;
                trace!("Unsuspended block {child_ref}");
                unsuspended.push(block);
            }
        }
        unsuspended
    }

    /// Check which of `block_refs` are missing from both DagState and suspended set.
    pub(crate) fn try_find_blocks(&mut self, block_refs: Vec<BlockRef>) -> BTreeSet<BlockRef> {
        let gc_round = self.dag_state.read().gc_round();
        let block_refs: Vec<BlockRef> = block_refs
            .into_iter()
            .filter(|r| r.round > gc_round)
            .collect();

        let mut missing = BTreeSet::new();
        for (found, block_ref) in self
            .dag_state
            .read()
            .contains_blocks(block_refs.clone())
            .into_iter()
            .zip(block_refs.iter())
        {
            if !found && !self.suspended_blocks.contains_key(block_ref) {
                missing.insert(*block_ref);
                self.missing_blocks.insert(*block_ref);
                self.missing_ancestors.entry(*block_ref).or_default();
            }
        }
        missing
    }

    pub(crate) fn missing_blocks(&self) -> &BTreeSet<BlockRef> {
        &self.missing_blocks
    }

    pub(crate) fn suspended_blocks_count(&self) -> usize {
        self.suspended_blocks.len()
    }

    /// Round range `[from, to]` for the highest and lowest rounds seen per authority.
    pub(crate) fn highest_received_round(&self) -> Round {
        self.dag_state.read().highest_accepted_round()
    }
}
