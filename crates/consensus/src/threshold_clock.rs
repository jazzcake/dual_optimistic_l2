// Adapted from: sui/consensus/core/src/threshold_clock.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: removed prometheus metrics (block_receive_delay); removed tokio::time::Instant
//          in favour of no timing (quorum_ts field removed entirely).

use std::{cmp::Ordering, sync::Arc};

use tracing::debug;

use crate::{
    context::Context,
    stake_aggregator::{QuorumThreshold, StakeAggregator},
    types::{BlockRef, Round},
};

pub(crate) struct ThresholdClock {
    context: Arc<Context>,
    aggregator: StakeAggregator<QuorumThreshold>,
    round: Round,
}

impl ThresholdClock {
    pub(crate) fn new(round: Round, context: Arc<Context>) -> Self {
        debug_assert!(
            context.committee.size() > 0,
            "committee must be non-empty to create ThresholdClock"
        );
        Self {
            context,
            aggregator: StakeAggregator::new(),
            round,
        }
    }

    /// Adds a block reference that has been accepted and advances the round when
    /// a quorum (2f+1) of blocks at the current round is collected.
    /// Returns `true` if the round advanced.
    pub(crate) fn add_block(&mut self, block: BlockRef) -> bool {
        debug_assert!(
            self.context.committee.is_valid_index(block.author),
            "block author {} is not a valid committee member",
            block.author.value()
        );
        match block.round.cmp(&self.round) {
            // Blocks from an older round than what we are building are irrelevant.
            Ordering::Less => false,
            Ordering::Equal => {
                if self.aggregator.add(block.author, &self.context.committee) {
                    self.aggregator.clear();
                    self.round = block.round + 1;
                    debug!(
                        "ThresholdClock advanced to round {} (block {} completed quorum)",
                        self.round, block
                    );
                    true
                } else {
                    false
                }
            }
            // A block from a future round implies we already have 2f+1 blocks from
            // every intermediate round; advance accordingly.
            Ordering::Greater => {
                self.aggregator.clear();
                if self.aggregator.add(block.author, &self.context.committee) {
                    // This single block already forms a quorum (committee size == 1).
                    self.round = block.round + 1;
                } else {
                    // Quorum at block.round - 1 but not yet at block.round.
                    self.round = block.round;
                }
                debug!(
                    "ThresholdClock advanced to round {} (catch-up via block {})",
                    self.round, block
                );
                true
            }
        }
    }

    pub(crate) fn get_round(&self) -> Round {
        self.round
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{committee::AuthorityIndex, types::BlockDigest};

    fn make_block_ref(round: Round, author: u32) -> BlockRef {
        BlockRef::new(round, AuthorityIndex::new_for_test(author), BlockDigest::default())
    }

    #[test]
    fn test_threshold_clock_add_block() {
        let context = Arc::new(Context::new_for_test(4));
        let mut clock = ThresholdClock::new(0, context);

        assert!(!clock.add_block(make_block_ref(0, 0)));
        assert_eq!(clock.get_round(), 0);
        assert!(!clock.add_block(make_block_ref(0, 1)));
        assert_eq!(clock.get_round(), 0);
        // 3rd block (3 out of 4 == 2f+1 for f=1) → quorum
        assert!(clock.add_block(make_block_ref(0, 2)));
        assert_eq!(clock.get_round(), 1);
        assert!(!clock.add_block(make_block_ref(1, 0)));
        assert_eq!(clock.get_round(), 1);
        assert!(!clock.add_block(make_block_ref(1, 3)));
        assert_eq!(clock.get_round(), 1);
        // Block from round 2 — catch-up advance
        assert!(clock.add_block(make_block_ref(2, 1)));
        assert_eq!(clock.get_round(), 2);
        // Late block from round 1 — ignored (< current round)
        assert!(!clock.add_block(make_block_ref(1, 1)));
        assert_eq!(clock.get_round(), 2);
        // Far-future block — big catch-up
        assert!(clock.add_block(make_block_ref(5, 2)));
        assert_eq!(clock.get_round(), 5);
    }

    #[test]
    fn test_threshold_clock_min_committee() {
        let context = Arc::new(Context::new_for_test(1));
        let mut clock = ThresholdClock::new(10, context);

        // Past block — no advance
        assert!(!clock.add_block(make_block_ref(9, 0)));
        assert_eq!(clock.get_round(), 10);

        // One block is a quorum when committee size == 1
        assert!(clock.add_block(make_block_ref(10, 0)));
        assert_eq!(clock.get_round(), 11);

        // Catch-up block
        clock.add_block(make_block_ref(20, 0));
        assert_eq!(clock.get_round(), 21);
    }
}
