// Adapted from: sui/consensus/core/src/stake_aggregator.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: replaced consensus_config imports with local committee module.

use std::{collections::BTreeSet, marker::PhantomData};

use crate::committee::{AuthorityIndex, Committee, Stake};

pub(crate) trait CommitteeThreshold {
    fn is_threshold(committee: &Committee, amount: Stake) -> bool;
    fn threshold(committee: &Committee) -> Stake;
}

#[derive(Default)]
pub(crate) struct QuorumThreshold;

#[cfg(test)]
#[derive(Default)]
pub(crate) struct ValidityThreshold;

impl CommitteeThreshold for QuorumThreshold {
    fn is_threshold(committee: &Committee, amount: Stake) -> bool {
        committee.reached_quorum(amount)
    }
    fn threshold(committee: &Committee) -> Stake {
        committee.quorum_threshold()
    }
}

#[cfg(test)]
impl CommitteeThreshold for ValidityThreshold {
    fn is_threshold(committee: &Committee, amount: Stake) -> bool {
        committee.reached_validity(amount)
    }
    fn threshold(committee: &Committee) -> Stake {
        committee.validity_threshold()
    }
}

#[derive(Default)]
pub(crate) struct StakeAggregator<T> {
    votes: BTreeSet<AuthorityIndex>,
    stake: Stake,
    _phantom: PhantomData<T>,
}

impl<T: CommitteeThreshold> StakeAggregator<T> {
    pub(crate) fn new() -> Self {
        Self {
            votes: Default::default(),
            stake: 0,
            _phantom: Default::default(),
        }
    }

    /// Adds a vote for `vote`. Returns true when the threshold is first reached.
    /// Duplicate votes from the same authority are ignored.
    pub(crate) fn add(&mut self, vote: AuthorityIndex, committee: &Committee) -> bool {
        debug_assert!(
            committee.is_valid_index(vote),
            "AuthorityIndex {} out of committee range {}",
            vote.value(),
            committee.size()
        );
        if self.votes.insert(vote) {
            self.stake += committee.stake(vote);
            if T::is_threshold(committee, self.stake) {
                return true;
            }
        }
        false
    }

    pub(crate) fn stake(&self) -> Stake {
        self.stake
    }

    pub(crate) fn clear(&mut self) {
        self.votes.clear();
        self.stake = 0;
    }
}
