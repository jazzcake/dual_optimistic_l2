// Adapted from: sui/consensus/types/src/block.rs, sui/consensus/core/src/block.rs
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: removed fastcrypto, bcs, signature handling; single Block struct (no enum_dispatch);
//          BlockDigest computed with sha3-256 over (round, author, timestamp_ms, ancestor_digests);
//          removed CommitVote, MisbehaviorReport, transaction_votes, SignedBlock.

use std::{
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
    sync::Arc,
};

use sha3::{Digest as _, Sha3_256};

use crate::committee::{AuthorityIndex, Committee, Epoch};

pub const DIGEST_LENGTH: usize = 32;

/// Round number of a block (u32, same as SUI).
pub type Round = u32;

/// Block proposal timestamp in milliseconds.
pub type BlockTimestampMs = u64;

/// Round 0 is genesis; real proposals start at round 1.
pub const GENESIS_ROUND: Round = 0;

// ---------------------------------------------------------------------------
// BlockDigest
// ---------------------------------------------------------------------------

/// 32-byte digest of a block.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockDigest(pub [u8; DIGEST_LENGTH]);

impl BlockDigest {
    pub const MIN: Self = Self([u8::MIN; DIGEST_LENGTH]);
    pub const MAX: Self = Self([u8::MAX; DIGEST_LENGTH]);
}

impl Hash for BlockDigest {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&self.0[..8]);
    }
}

impl fmt::Display for BlockDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0[..4] {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl fmt::Debug for BlockDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// ---------------------------------------------------------------------------
// BlockRef
// ---------------------------------------------------------------------------

/// Uniquely identifies a VerifiedBlock by (round, author, digest).
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockRef {
    pub round: Round,
    pub author: AuthorityIndex,
    pub digest: BlockDigest,
}

impl BlockRef {
    pub const MIN: Self = Self {
        round: 0,
        author: AuthorityIndex::MIN,
        digest: BlockDigest::MIN,
    };
    pub const MAX: Self = Self {
        round: u32::MAX,
        author: AuthorityIndex::MAX,
        digest: BlockDigest::MAX,
    };

    pub fn new(round: Round, author: AuthorityIndex, digest: BlockDigest) -> Self {
        Self {
            round,
            author,
            digest,
        }
    }
}

impl Hash for BlockRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&self.digest.0[..8]);
    }
}

impl fmt::Display for BlockRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "B{}({},{})", self.round, self.author, self.digest)
    }
}

impl fmt::Debug for BlockRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// ---------------------------------------------------------------------------
// Transaction
// ---------------------------------------------------------------------------

/// Raw transaction bytes.
#[derive(Clone, Eq, PartialEq, Default, Debug)]
pub struct Transaction(pub Vec<u8>);

// ---------------------------------------------------------------------------
// Slot
// ---------------------------------------------------------------------------

/// (round, authority) position in the DAG. One slot may hold 0..n blocks
/// (>1 only if the authority equivocates).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Slot {
    pub round: Round,
    pub authority: AuthorityIndex,
}

impl Slot {
    pub fn new(round: Round, authority: AuthorityIndex) -> Self {
        Self { round, authority }
    }

    pub fn new_for_test(round: Round, authority: u32) -> Self {
        Self {
            round,
            authority: AuthorityIndex::new_for_test(authority),
        }
    }
}

impl From<BlockRef> for Slot {
    fn from(b: BlockRef) -> Self {
        Slot::new(b.round, b.author)
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.authority, self.round)
    }
}

impl fmt::Debug for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// ---------------------------------------------------------------------------
// Block  (single version, no enum_dispatch)
// ---------------------------------------------------------------------------

/// Core block data produced by one authority at one round.
#[derive(Clone, Default, Debug)]
pub struct Block {
    pub epoch: Epoch,
    pub round: Round,
    pub author: AuthorityIndex,
    pub timestamp_ms: BlockTimestampMs,
    pub ancestors: Vec<BlockRef>,
    pub transactions: Vec<Transaction>,
}

impl Block {
    pub fn slot(&self) -> Slot {
        Slot::new(self.round, self.author)
    }
}

// ---------------------------------------------------------------------------
// VerifiedBlock
// ---------------------------------------------------------------------------

/// Opaque trust-marker wrapper: a Block that has been validated and accepted
/// into the local DAG. Digest is computed lazily and cached on first call.
///
/// clone() is cheap (Arc).
#[derive(Clone)]
pub struct VerifiedBlock(Arc<VerifiedBlockInner>);

struct VerifiedBlockInner {
    block: Block,
    digest: BlockDigest,
}

impl VerifiedBlock {
    fn compute_digest(block: &Block) -> BlockDigest {
        let mut h = Sha3_256::new();
        h.update(block.round.to_le_bytes());
        h.update((block.author.value() as u32).to_le_bytes());
        h.update(block.epoch.to_le_bytes());
        h.update(block.timestamp_ms.to_le_bytes());
        for ancestor in &block.ancestors {
            h.update(ancestor.digest.0);
        }
        // Include number of txs so two blocks with different transactions but
        // same ancestors produce different digests.
        h.update((block.transactions.len() as u32).to_le_bytes());
        for tx in &block.transactions {
            h.update((tx.0.len() as u32).to_le_bytes());
            h.update(&tx.0);
        }
        BlockDigest(h.finalize().into())
    }

    pub fn new_verified(block: Block) -> Self {
        let digest = Self::compute_digest(&block);
        Self(Arc::new(VerifiedBlockInner { block, digest }))
    }

    /// Creates a VerifiedBlock for testing without actual verification.
    pub fn new_for_test(block: Block) -> Self {
        Self::new_verified(block)
    }

    pub fn reference(&self) -> BlockRef {
        BlockRef::new(self.0.block.round, self.0.block.author, self.0.digest)
    }

    pub fn digest(&self) -> BlockDigest {
        self.0.digest
    }

    pub fn epoch(&self) -> Epoch {
        self.0.block.epoch
    }

    pub fn round(&self) -> Round {
        self.0.block.round
    }

    pub fn author(&self) -> AuthorityIndex {
        self.0.block.author
    }

    pub fn slot(&self) -> Slot {
        self.0.block.slot()
    }

    pub fn timestamp_ms(&self) -> BlockTimestampMs {
        self.0.block.timestamp_ms
    }

    pub fn ancestors(&self) -> &[BlockRef] {
        &self.0.block.ancestors
    }

    pub fn transactions(&self) -> &[Transaction] {
        &self.0.block.transactions
    }
}

impl Deref for VerifiedBlock {
    type Target = Block;
    fn deref(&self) -> &Self::Target {
        &self.0.block
    }
}

impl PartialEq for VerifiedBlock {
    fn eq(&self, other: &Self) -> bool {
        self.0.digest == other.0.digest
    }
}

impl fmt::Display for VerifiedBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reference())
    }
}

impl fmt::Debug for VerifiedBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}([{}];{}t)",
            self.reference(),
            self.ancestors()
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            self.transactions().len(),
        )
    }
}

// ---------------------------------------------------------------------------
// TestBlock builder
// ---------------------------------------------------------------------------

/// Convenience builder for producing VerifiedBlocks in tests.
pub struct TestBlock {
    block: Block,
}

impl TestBlock {
    pub fn new(round: Round, author: u32) -> Self {
        Self {
            block: Block {
                round,
                author: AuthorityIndex::new_for_test(author),
                ..Default::default()
            },
        }
    }

    pub fn set_epoch(mut self, epoch: Epoch) -> Self {
        self.block.epoch = epoch;
        self
    }

    pub fn set_round(mut self, round: Round) -> Self {
        self.block.round = round;
        self
    }

    pub fn set_author(mut self, author: AuthorityIndex) -> Self {
        self.block.author = author;
        self
    }

    pub fn set_timestamp_ms(mut self, ts: BlockTimestampMs) -> Self {
        self.block.timestamp_ms = ts;
        self
    }

    /// Sorts ancestors by (author), with the block's own author first.
    pub fn set_ancestors(mut self, mut ancestors: Vec<BlockRef>) -> Self {
        let own = self.block.author;
        ancestors.sort_by(|a, b| {
            if a.author == own {
                return std::cmp::Ordering::Less;
            }
            if b.author == own {
                return std::cmp::Ordering::Greater;
            }
            a.author.cmp(&b.author)
        });
        self.block.ancestors = ancestors;
        self
    }

    pub fn set_ancestors_raw(mut self, ancestors: Vec<BlockRef>) -> Self {
        self.block.ancestors = ancestors;
        self
    }

    pub fn set_transactions(mut self, txs: Vec<Transaction>) -> Self {
        self.block.transactions = txs;
        self
    }

    pub fn build(self) -> VerifiedBlock {
        VerifiedBlock::new_for_test(self.block)
    }
}

// ---------------------------------------------------------------------------
// Genesis helpers
// ---------------------------------------------------------------------------

/// Generate one genesis VerifiedBlock per committee authority (round 0).
pub fn genesis_blocks(committee: &Committee) -> Vec<VerifiedBlock> {
    committee
        .authorities()
        .map(|(authority_index, _)| {
            VerifiedBlock::new_verified(Block {
                epoch: committee.epoch(),
                round: GENESIS_ROUND,
                author: authority_index,
                ..Default::default()
            })
        })
        .collect()
}
