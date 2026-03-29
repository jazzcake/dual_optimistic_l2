// Adapted from: sui/consensus/core/src/linearizer.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: removed all prometheus metrics; removed TrustedCommit serialization (no bcs/storage);
//          CommittedSubDag constructed directly without CommitFinalizer fast-path fields;
//          commit timestamp simplified to leader block timestamp (no stake-median calculation).

use std::sync::Arc;

use parking_lot::RwLock;

use crate::{
    commit::{CommitRef, CommittedSubDag, sort_sub_dag_blocks},
    context::Context,
    dag_state::{DagState, derive_commit_digest},
    types::{Round, VerifiedBlock},
};

/// Expands a sequence of committed leader blocks into ordered CommittedSubDag values.
#[derive(Clone)]
pub struct Linearizer {
    context: Arc<Context>,
    dag_state: Arc<RwLock<DagState>>,
}

impl Linearizer {
    pub fn new(context: Arc<Context>, dag_state: Arc<RwLock<DagState>>) -> Self {
        debug_assert!(
            context.committee.size() > 0,
            "cannot create Linearizer for empty committee"
        );
        Self { context, dag_state }
    }

    /// Collect the sub-dag anchored at `leader_block` and produce a `CommittedSubDag`.
    pub fn commit_leader(&mut self, leader_block: VerifiedBlock) -> CommittedSubDag {
        debug_assert!(
            leader_block.round() > 0,
            "cannot commit genesis block (round 0)"
        );

        let mut dag_state = self.dag_state.write();

        let last_commit_index = dag_state.last_commit_index();
        let next_index = last_commit_index + 1;
        let last_commit_timestamp = dag_state.last_commit_timestamp_ms();

        // Linearise the causal sub-dag rooted at `leader_block`.
        let to_commit = Self::linearize_sub_dag(leader_block.clone(), &mut *dag_state);

        // Commit timestamp: max(leader.timestamp_ms, last_commit_timestamp) for monotonicity.
        let timestamp_ms = leader_block
            .timestamp_ms()
            .max(last_commit_timestamp);

        let leader_ref = leader_block.reference();
        let digest = derive_commit_digest(next_index, &leader_ref);
        let commit_ref = CommitRef::new(next_index, digest);

        let subdag = CommittedSubDag::new(leader_ref, to_commit, timestamp_ms, commit_ref);
        dag_state.record_commit(&subdag);

        subdag
    }

    /// DFS/BFS to collect all uncommitted causal ancestors of `leader_block`
    /// above `gc_round`, then sort them deterministically.
    fn linearize_sub_dag(
        leader_block: VerifiedBlock,
        dag_state: &mut DagState,
    ) -> Vec<VerifiedBlock> {
        let gc_round: Round = dag_state.gc_round();
        let leader_ref = leader_block.reference();

        assert!(
            dag_state.set_committed(&leader_ref),
            "leader block {leader_ref:?} was already committed"
        );

        let mut buffer = vec![leader_block];
        let mut to_commit = Vec::new();

        while let Some(block) = buffer.pop() {
            to_commit.push(block.clone());

            let ancestors: Vec<VerifiedBlock> = dag_state
                .get_blocks(
                    &block
                        .ancestors()
                        .iter()
                        .copied()
                        .filter(|ancestor| {
                            ancestor.round > gc_round && !dag_state.is_committed(ancestor)
                        })
                        .collect::<Vec<_>>(),
                )
                .into_iter()
                .map(|opt| opt.expect("all uncommitted ancestor blocks should be in DagState"))
                .collect();

            for ancestor in ancestors {
                assert!(
                    dag_state.set_committed(&ancestor.reference()),
                    "block {:?} attempted to be committed twice",
                    ancestor.reference()
                );
                buffer.push(ancestor);
            }
        }

        assert!(
            to_commit.iter().all(|b| b.round() > gc_round),
            "no blocks at or below gc_round={gc_round} should be committed"
        );

        sort_sub_dag_blocks(&mut to_commit);
        to_commit
    }
}
