// Adapted from: sui/consensus/core/src/base_committer.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: replaced score-based LeaderSchedule with round-robin leader election
//          (leader = round % committee.size()); removed all prometheus metrics.

use std::{collections::HashMap, fmt::Display, sync::Arc};

use parking_lot::RwLock;
use tracing::trace;

use crate::{
    commit::{DEFAULT_WAVE_LENGTH, LeaderStatus, WaveNumber},
    committee::AuthorityIndex,
    context::Context,
    dag_state::DagState,
    stake_aggregator::{QuorumThreshold, StakeAggregator},
    types::{BlockRef, Round, Slot, VerifiedBlock},
};

pub(crate) struct BaseCommitterOptions {
    /// Length of one wave (≥ MINIMUM_WAVE_LENGTH = 3).
    pub wave_length: Round,
    /// Leader offset for multi-leader setups — selects a different leader per committer.
    pub leader_offset: u32,
    /// Round offset for pipelining — shifts when each BaseCommitter looks for a leader.
    pub round_offset: Round,
}

impl Default for BaseCommitterOptions {
    fn default() -> Self {
        Self {
            wave_length: DEFAULT_WAVE_LENGTH,
            leader_offset: 0,
            round_offset: 0,
        }
    }
}

pub(crate) struct BaseCommitter {
    context: Arc<Context>,
    dag_state: Arc<RwLock<DagState>>,
    options: BaseCommitterOptions,
}

impl BaseCommitter {
    pub fn new(
        context: Arc<Context>,
        dag_state: Arc<RwLock<DagState>>,
        options: BaseCommitterOptions,
    ) -> Self {
        debug_assert!(
            options.wave_length >= 3,
            "wave_length must be at least 3, got {}",
            options.wave_length
        );
        debug_assert!(
            context.committee.size() > 0,
            "committee must be non-empty"
        );
        Self {
            context,
            dag_state,
            options,
        }
    }

    // -----------------------------------------------------------------------
    // Round-robin leader election (replaces score-based LeaderSchedule)
    // -----------------------------------------------------------------------

    fn elect_leader_index(&self, round: Round, offset: u32) -> AuthorityIndex {
        let size = self.context.committee.size() as u32;
        // Rotate starting from (round + offset) mod size to distribute leaders evenly.
        let idx = (round.wrapping_add(offset)) % size;
        AuthorityIndex::new_for_test(idx)
    }

    /// Returns the `Slot` that is the leader for `round`, or `None` if this
    /// committer instance is not responsible for `round`.
    pub fn elect_leader(&self, round: Round) -> Option<Slot> {
        let wave = self.wave_number(round);
        if self.leader_round(wave) != round {
            return None;
        }
        Some(Slot::new(round, self.elect_leader_index(round, self.options.leader_offset)))
    }

    // -----------------------------------------------------------------------
    // Commit rules
    // -----------------------------------------------------------------------

    /// Direct-decide rule: try to commit or skip the leader at `leader`.
    pub fn try_direct_decide(&self, leader: Slot) -> LeaderStatus {
        debug_assert!(leader.round > 0, "cannot decide genesis (round 0)");

        let voting_round = leader.round + 1;
        if self.enough_leader_blame(voting_round, leader.authority) {
            return LeaderStatus::Skip(leader);
        }

        let wave = self.wave_number(leader.round);
        let decision_round = self.decision_round(wave);
        let leader_blocks = self
            .dag_state
            .read()
            .get_uncommitted_blocks_at_slot(leader);

        let mut leaders_with_enough_support: Vec<_> = leader_blocks
            .into_iter()
            .filter(|l| self.enough_leader_support(decision_round, l))
            .map(LeaderStatus::Commit)
            .collect();

        if leaders_with_enough_support.len() > 1 {
            panic!(
                "[{self}] More than one candidate for {leader}: {leaders_with_enough_support:?}"
            );
        }

        leaders_with_enough_support
            .pop()
            .unwrap_or(LeaderStatus::Undecided(leader))
    }

    /// Indirect-decide rule: try to commit or skip `leader_slot` using already-decided
    /// anchor leaders.
    pub fn try_indirect_decide<'a>(
        &self,
        leader_slot: Slot,
        leaders: impl Iterator<Item = &'a LeaderStatus>,
    ) -> LeaderStatus {
        debug_assert!(leader_slot.round > 0, "cannot decide genesis (round 0)");

        let anchors = leaders
            .filter(|x| leader_slot.round + self.options.wave_length <= x.round());

        for anchor in anchors {
            trace!("[{self}] trying indirect-decide {leader_slot} via {anchor}");
            match anchor {
                LeaderStatus::Commit(anchor_block) => {
                    return self.decide_leader_from_anchor(anchor_block, leader_slot);
                }
                LeaderStatus::Skip(..) => (),
                LeaderStatus::Undecided(..) => break,
            }
        }

        LeaderStatus::Undecided(leader_slot)
    }

    // -----------------------------------------------------------------------
    // Wave / round helpers
    // -----------------------------------------------------------------------

    pub(crate) fn leader_round(&self, wave: WaveNumber) -> Round {
        wave * self.options.wave_length + self.options.round_offset
    }

    pub(crate) fn decision_round(&self, wave: WaveNumber) -> Round {
        let wl = self.options.wave_length;
        wave * wl + wl - 1 + self.options.round_offset
    }

    pub(crate) fn wave_number(&self, round: Round) -> WaveNumber {
        round.saturating_sub(self.options.round_offset) / self.options.wave_length
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Find which block at `leader_slot` is supported (directly or indirectly) by `from`.
    fn find_supported_block(&self, leader_slot: Slot, from: &VerifiedBlock) -> Option<BlockRef> {
        if from.round() < leader_slot.round {
            return None;
        }
        for ancestor in from.ancestors() {
            if Slot::from(*ancestor) == leader_slot {
                return Some(*ancestor);
            }
        }
        // Weak links may reference lower rounds — recurse through ancestors above leader_slot.
        for ancestor in from.ancestors() {
            if ancestor.round <= leader_slot.round {
                continue;
            }
            let Some(ancestor_block) = self.dag_state.read().get_block(ancestor) else {
                panic!("Block not found in storage: {ancestor:?}");
            };
            if let Some(support) = self.find_supported_block(leader_slot, &ancestor_block) {
                return Some(support);
            }
        }
        None
    }

    fn is_vote(&self, potential_vote: &VerifiedBlock, leader_block: &VerifiedBlock) -> bool {
        let reference = leader_block.reference();
        let leader_slot = Slot::from(reference);
        self.find_supported_block(leader_slot, potential_vote) == Some(reference)
    }

    fn is_certificate(
        &self,
        potential_certificate: &VerifiedBlock,
        leader_block: &VerifiedBlock,
        all_votes: &mut HashMap<BlockRef, bool>,
    ) -> bool {
        let gc_round = self.dag_state.read().gc_round();
        let mut votes_stake = StakeAggregator::<QuorumThreshold>::new();

        for reference in potential_certificate.ancestors() {
            let is_vote = if let Some(&cached) = all_votes.get(reference) {
                cached
            } else {
                let potential_vote = self.dag_state.read().get_block(reference);
                let vote = if let Some(potential_vote) = potential_vote {
                    self.is_vote(&potential_vote, leader_block)
                } else {
                    assert!(
                        reference.round <= gc_round,
                        "Block {reference:?} not in DAG and not below gc_round {gc_round}"
                    );
                    false
                };
                all_votes.insert(*reference, vote);
                vote
            };

            if is_vote
                && votes_stake.add(reference.author, &self.context.committee)
            {
                return true;
            }
        }
        false
    }

    fn decide_leader_from_anchor(
        &self,
        anchor: &VerifiedBlock,
        leader_slot: Slot,
    ) -> LeaderStatus {
        let leader_blocks = self
            .dag_state
            .read()
            .get_uncommitted_blocks_at_slot(leader_slot);

        if leader_blocks.len() > 1 {
            tracing::debug!(
                "Multiple blocks for leader slot {leader_slot}: {leader_blocks:?}",
            );
        }

        let wave = self.wave_number(leader_slot.round);
        let decision_round = self.decision_round(wave);
        let potential_certificates = self
            .dag_state
            .read()
            .ancestors_at_round(anchor, decision_round);

        let mut certified: Vec<_> = leader_blocks
            .into_iter()
            .filter(|leader_block| {
                let mut all_votes = HashMap::new();
                potential_certificates.iter().any(|cert| {
                    self.is_certificate(cert, leader_block, &mut all_votes)
                })
            })
            .collect();

        if certified.len() > 1 {
            panic!("More than one certified leader at wave {wave} in {leader_slot}");
        }

        match certified.pop() {
            Some(b) => LeaderStatus::Commit(b),
            None => LeaderStatus::Skip(leader_slot),
        }
    }

    fn enough_leader_blame(&self, voting_round: Round, leader: AuthorityIndex) -> bool {
        debug_assert!(voting_round > 0, "voting_round must be positive");
        let voting_blocks = self
            .dag_state
            .read()
            .get_uncommitted_blocks_at_round(voting_round);

        let mut blame = StakeAggregator::<QuorumThreshold>::new();
        for voting_block in &voting_blocks {
            let voter = voting_block.reference().author;
            if voting_block
                .ancestors()
                .iter()
                .all(|ancestor| ancestor.author != leader)
            {
                if blame.add(voter, &self.context.committee) {
                    return true;
                }
            }
        }
        false
    }

    fn enough_leader_support(&self, decision_round: Round, leader_block: &VerifiedBlock) -> bool {
        debug_assert!(decision_round > 0, "decision_round must be positive");
        let decision_blocks = self
            .dag_state
            .read()
            .get_uncommitted_blocks_at_round(decision_round);

        let mut support = StakeAggregator::<QuorumThreshold>::new();
        let mut all_votes: HashMap<BlockRef, bool> = HashMap::new();
        for decision_block in &decision_blocks {
            if self.is_certificate(decision_block, leader_block, &mut all_votes) {
                let voter = decision_block.reference().author;
                if support.add(voter, &self.context.committee) {
                    return true;
                }
            }
        }
        false
    }
}

impl Display for BaseCommitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BaseCommitter(wave_len={}, leader_off={}, round_off={})",
            self.options.wave_length, self.options.leader_offset, self.options.round_offset
        )
    }
}
