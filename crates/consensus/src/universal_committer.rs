// Adapted from: sui/consensus/core/src/universal_committer.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: removed all prometheus metrics; removed protocol_config.num_leaders_per_round()
//          (always uses single-leader path); removed LeaderSchedule from builder
//          (BaseCommitter now uses round-robin internally); simplified builder API.

use std::{collections::VecDeque, sync::Arc};

use parking_lot::RwLock;

use crate::{
    base_committer::{BaseCommitter, BaseCommitterOptions},
    commit::{DecidedLeader, Decision, DEFAULT_WAVE_LENGTH},
    context::Context,
    dag_state::DagState,
    types::{Round, Slot, GENESIS_ROUND},
};

/// Drives commit decisions using one or more BaseCommitter instances.
pub(crate) struct UniversalCommitter {
    context: Arc<Context>,
    dag_state: Arc<RwLock<DagState>>,
    committers: Vec<BaseCommitter>,
}

impl UniversalCommitter {
    /// Try to decide as many leaders as possible, starting from `last_decided`.
    /// Returns decided leaders in ascending round order.
    #[tracing::instrument(skip_all, fields(last_decided = %last_decided))]
    pub(crate) fn try_decide(&self, last_decided: Slot) -> Vec<DecidedLeader> {
        let highest_accepted_round = self.dag_state.read().highest_accepted_round();

        let mut leaders: VecDeque<(LeaderStatus, Decision)> = VecDeque::new();

        // Commit requires blocks at decision_round = leader_round + wave_length - 1.
        // So we can only try leaders up to highest_accepted_round - (wave_length - 1).
        'outer: for round in (last_decided.round..=highest_accepted_round.saturating_sub(2)).rev()
        {
            for committer in self.committers.iter().rev() {
                let Some(slot) = committer.elect_leader(round) else {
                    tracing::debug!("No leader for round {round}");
                    continue;
                };

                if slot == last_decided {
                    tracing::debug!("Reached last committed {slot}, stopping");
                    break 'outer;
                }

                tracing::trace!("Trying to decide {slot}");

                let mut status = committer.try_direct_decide(slot);
                tracing::debug!("Direct rule for {slot}: {status}");

                if status.is_decided() {
                    leaders.push_front((status, Decision::Direct));
                } else {
                    status = committer
                        .try_indirect_decide(slot, leaders.iter().map(|(x, _)| x));
                    tracing::debug!("Indirect rule for {slot}: {status}");
                    leaders.push_front((status, Decision::Indirect));
                }
            }
        }

        // The committed sequence is the longest prefix of decided leaders.
        let mut decided_leaders = Vec::new();
        for (leader, decision) in leaders {
            if leader.round() == GENESIS_ROUND {
                continue;
            }
            let Some(decided_leader) =
                leader.into_decided_leader(decision == Decision::Direct)
            else {
                break;
            };
            decided_leaders.push(decided_leader);
        }

        if !decided_leaders.is_empty() {
            tracing::debug!("Decided: {decided_leaders:?}");
        }
        decided_leaders
    }

    /// All leader slots at `round` across all committer instances.
    pub(crate) fn get_leaders(&self, round: Round) -> Vec<crate::committee::AuthorityIndex> {
        self.committers
            .iter()
            .filter_map(|c| c.elect_leader(round))
            .map(|l| l.authority)
            .collect()
    }
}

// Re-export LeaderStatus for use inside this module.
use crate::commit::LeaderStatus;

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

pub(crate) mod universal_committer_builder {
    use super::*;

    pub(crate) struct UniversalCommitterBuilder {
        context: Arc<Context>,
        dag_state: Arc<RwLock<DagState>>,
        wave_length: Round,
        number_of_leaders: usize,
        pipeline: bool,
    }

    impl UniversalCommitterBuilder {
        pub(crate) fn new(context: Arc<Context>, dag_state: Arc<RwLock<DagState>>) -> Self {
            debug_assert!(
                context.committee.size() > 0,
                "cannot build committer for empty committee"
            );
            Self {
                context,
                dag_state,
                wave_length: DEFAULT_WAVE_LENGTH,
                number_of_leaders: 1,
                pipeline: false,
            }
        }

        pub(crate) fn with_wave_length(mut self, wave_length: Round) -> Self {
            self.wave_length = wave_length;
            self
        }

        pub(crate) fn with_number_of_leaders(mut self, n: usize) -> Self {
            self.number_of_leaders = n;
            self
        }

        pub(crate) fn with_pipeline(mut self, pipeline: bool) -> Self {
            self.pipeline = pipeline;
            self
        }

        pub(crate) fn build(self) -> UniversalCommitter {
            debug_assert!(self.wave_length >= 3, "wave_length must be at least 3");
            debug_assert!(
                self.number_of_leaders > 0,
                "number_of_leaders must be positive"
            );

            let mut committers = Vec::new();
            if self.pipeline {
                for round_offset in 0..self.wave_length {
                    for leader_offset in 0..self.number_of_leaders as u32 {
                        committers.push(BaseCommitter::new(
                            self.context.clone(),
                            self.dag_state.clone(),
                            BaseCommitterOptions {
                                wave_length: self.wave_length,
                                leader_offset,
                                round_offset,
                            },
                        ));
                    }
                }
            } else {
                for leader_offset in 0..self.number_of_leaders as u32 {
                    committers.push(BaseCommitter::new(
                        self.context.clone(),
                        self.dag_state.clone(),
                        BaseCommitterOptions {
                            wave_length: self.wave_length,
                            leader_offset,
                            round_offset: 0,
                        },
                    ));
                }
            }

            UniversalCommitter {
                context: self.context,
                dag_state: self.dag_state,
                committers,
            }
        }
    }
}
