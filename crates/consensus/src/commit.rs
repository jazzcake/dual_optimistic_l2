// Adapted from: sui/consensus/core/src/commit.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: removed TrustedCommit serialization (no bcs/storage), removed CommitVote,
//          CommitInfo, ReputationScores, ScoringSubdag, load_committed_subdag_from_store;
//          CommittedSubDag simplified (no rejected_transactions, no protocol_config flags).

use std::fmt::{self, Display, Formatter};

use crate::types::{BlockRef, BlockTimestampMs, Round, Slot, VerifiedBlock};

#[cfg(test)]
use crate::committee::AuthorityIndex;

/// Index of a commit among all consensus commits.
pub type CommitIndex = u32;

pub(crate) const GENESIS_COMMIT_INDEX: CommitIndex = 0;

/// We need at least one leader round, one voting round, and one decision round.
pub(crate) const MINIMUM_WAVE_LENGTH: Round = 3;

/// Default wave length for all committers.
pub(crate) const DEFAULT_WAVE_LENGTH: Round = MINIMUM_WAVE_LENGTH;

/// The consensus protocol operates in 'waves' of `wave_length` rounds.
pub(crate) type WaveNumber = u32;

// ---------------------------------------------------------------------------
// CommitDigest
// ---------------------------------------------------------------------------

pub const COMMIT_DIGEST_LENGTH: usize = 32;

/// 32-byte digest of a consensus commit.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitDigest(pub [u8; COMMIT_DIGEST_LENGTH]);

impl CommitDigest {
    pub const MIN: Self = Self([u8::MIN; COMMIT_DIGEST_LENGTH]);
    pub const MAX: Self = Self([u8::MAX; COMMIT_DIGEST_LENGTH]);
}

impl fmt::Display for CommitDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0[..4] {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl fmt::Debug for CommitDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// ---------------------------------------------------------------------------
// CommitRef
// ---------------------------------------------------------------------------

/// Uniquely identifies a commit with its sequential index and digest.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommitRef {
    pub index: CommitIndex,
    pub digest: CommitDigest,
}

impl CommitRef {
    pub fn new(index: CommitIndex, digest: CommitDigest) -> Self {
        Self { index, digest }
    }
}

impl fmt::Display for CommitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "C{}({})", self.index, self.digest)
    }
}

impl fmt::Debug for CommitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// ---------------------------------------------------------------------------
// CommittedSubDag
// ---------------------------------------------------------------------------

/// The ordered set of blocks that form one consensus commit.
/// Sent from `Linearizer` to the execution layer.
#[derive(Clone, PartialEq)]
pub struct CommittedSubDag {
    /// Reference to the wave leader block.
    pub leader: BlockRef,
    /// All committed blocks, sorted by (round, author).
    pub blocks: Vec<VerifiedBlock>,
    /// Commit timestamp (max of parent timestamps by stake).
    pub timestamp_ms: BlockTimestampMs,
    /// Sequential commit reference.
    pub commit_ref: CommitRef,
}

impl CommittedSubDag {
    pub fn new(
        leader: BlockRef,
        blocks: Vec<VerifiedBlock>,
        timestamp_ms: BlockTimestampMs,
        commit_ref: CommitRef,
    ) -> Self {
        Self {
            leader,
            blocks,
            timestamp_ms,
            commit_ref,
        }
    }
}

impl Display for CommittedSubDag {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{} [{}]",
            self.commit_ref,
            self.leader,
            self.blocks
                .iter()
                .map(|b| b.reference().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl fmt::Debug for CommittedSubDag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Sort sub-dag blocks by round then authority (deterministic).
pub(crate) fn sort_sub_dag_blocks(blocks: &mut [VerifiedBlock]) {
    blocks.sort_by(|a, b| {
        a.round()
            .cmp(&b.round())
            .then_with(|| a.author().cmp(&b.author()))
    });
}

// ---------------------------------------------------------------------------
// LeaderStatus / DecidedLeader / Decision
// ---------------------------------------------------------------------------

/// The status of a leader slot from the direct and indirect commit rules.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LeaderStatus {
    Commit(VerifiedBlock),
    Skip(Slot),
    Undecided(Slot),
}

impl LeaderStatus {
    pub(crate) fn round(&self) -> Round {
        match self {
            Self::Commit(block) => block.round(),
            Self::Skip(leader) => leader.round,
            Self::Undecided(leader) => leader.round,
        }
    }

    pub(crate) fn is_decided(&self) -> bool {
        matches!(self, Self::Commit(_) | Self::Skip(_))
    }

    pub(crate) fn into_decided_leader(self, direct: bool) -> Option<DecidedLeader> {
        match self {
            Self::Commit(block) => Some(DecidedLeader::Commit(block, direct)),
            Self::Skip(slot) => Some(DecidedLeader::Skip(slot)),
            Self::Undecided(..) => None,
        }
    }
}

impl Display for LeaderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit(block) => write!(f, "Commit({})", block.reference()),
            Self::Skip(slot) => write!(f, "Skip({slot})"),
            Self::Undecided(slot) => write!(f, "Undecided({slot})"),
        }
    }
}

/// Decision of each leader slot, after all undecided leaders are resolved.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DecidedLeader {
    /// The committed leader block; the bool indicates whether it was a direct commit.
    Commit(VerifiedBlock, bool),
    /// The leader slot was skipped (no block committed).
    Skip(Slot),
}

impl DecidedLeader {
    pub(crate) fn slot(&self) -> Slot {
        match self {
            Self::Commit(block, _) => block.reference().into(),
            Self::Skip(slot) => *slot,
        }
    }

    pub(crate) fn into_committed_block(self) -> Option<VerifiedBlock> {
        match self {
            Self::Commit(block, _) => Some(block),
            Self::Skip(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn round(&self) -> Round {
        match self {
            Self::Commit(block, _) => block.round(),
            Self::Skip(leader) => leader.round,
        }
    }

    #[cfg(test)]
    pub(crate) fn authority(&self) -> AuthorityIndex {
        match self {
            Self::Commit(block, _) => block.author(),
            Self::Skip(leader) => leader.authority,
        }
    }
}

/// Whether a commit was reached via direct or indirect rule.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Decision {
    Direct,
    Indirect,
}
