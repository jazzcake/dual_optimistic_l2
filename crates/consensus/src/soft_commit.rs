// New file — no SUI equivalent.
// Mysticeti's BaseCommitter detects a leader commit only at the *decision* round (R+2).
// This module provides the earlier 2Δ "soft commit" signal: when 2f+1 validators have
// produced a block at the *voting* round (R+1) that references the wave leader at R,
// we optimistically pre-commit that leader even before the decision round arrives.

use std::collections::{BTreeSet, HashMap};

use crate::{
    commit::DEFAULT_WAVE_LENGTH,
    committee::{AuthorityIndex, Committee},
    stake_aggregator::{QuorumThreshold, StakeAggregator},
    types::{BlockRef, Round},
};

/// Tracks incoming "votes" for wave-leader blocks and fires once a quorum is reached.
///
/// A *vote* for leader block L at round R is any accepted block at voting round R+1
/// whose ancestor list contains the BlockRef of L.
pub(crate) struct SoftCommitTracker {
    /// Per-leader accumulator: BlockRef of the leader → stake aggregator.
    votes: HashMap<BlockRef, StakeAggregator<QuorumThreshold>>,
    /// Leaders that have already reached quorum (dedup guard).
    soft_committed: BTreeSet<BlockRef>,
}

impl SoftCommitTracker {
    pub(crate) fn new() -> Self {
        Self {
            votes: HashMap::new(),
            soft_committed: BTreeSet::new(),
        }
    }

    /// Record that `voter` at the voting round referenced `leader_ref` (at a leader round).
    ///
    /// Returns `true` the **first** time quorum (2f+1) is reached for `leader_ref`.
    /// Subsequent calls for the same `leader_ref` always return `false`.
    pub(crate) fn add_vote(
        &mut self,
        leader_ref: BlockRef,
        voter: AuthorityIndex,
        committee: &Committee,
    ) -> bool {
        debug_assert!(
            committee.is_valid_index(voter),
            "voter index {} out of committee size {}",
            voter.value(),
            committee.size()
        );
        debug_assert!(
            Self::is_leader_round(leader_ref.round),
            "add_vote called for non-leader round {}",
            leader_ref.round
        );

        // Already soft-committed — ignore duplicate quorum signals.
        if self.soft_committed.contains(&leader_ref) {
            return false;
        }

        let agg = self
            .votes
            .entry(leader_ref)
            .or_insert_with(StakeAggregator::new);

        if agg.add(voter, committee) {
            self.soft_committed.insert(leader_ref);
            return true;
        }
        false
    }

    pub(crate) fn is_soft_committed(&self, leader_ref: &BlockRef) -> bool {
        self.soft_committed.contains(leader_ref)
    }

    /// Returns `true` if `round` is a wave-leader round.
    ///
    /// With `DEFAULT_WAVE_LENGTH = 3` and `round_offset = 0`, leader rounds are
    /// 3, 6, 9, … (positive multiples of wave_length).
    pub(crate) fn is_leader_round(round: Round) -> bool {
        round > 0 && round % DEFAULT_WAVE_LENGTH == 0
    }

    /// Returns the expected leader `AuthorityIndex` for a leader round, using the
    /// same round-robin formula as `BaseCommitter::elect_leader_index`.
    pub(crate) fn leader_at_round(round: Round, committee_size: usize) -> AuthorityIndex {
        debug_assert!(
            Self::is_leader_round(round),
            "called leader_at_round for non-leader round {}",
            round
        );
        debug_assert!(committee_size > 0, "committee_size must be positive");
        AuthorityIndex::new_for_test(round % committee_size as u32)
    }
}
