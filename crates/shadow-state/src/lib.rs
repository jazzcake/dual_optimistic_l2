//! Shadow state crate: Multi-Version Shadow Memory.
//!
//! Responsibilities:
//! - Layer speculative per-round state on top of canonical ledger
//! - Implement revm::DatabaseRef (D2: &self for Arc sharing)
//! - Detect R/W conflicts at storage slot granularity (D5)
//! - Apply (commit) or discard StateDiff on HardCommit resolution

#![allow(dead_code, unused_variables)]

use std::{
    collections::BTreeMap,
    sync::RwLock,
};
use shared::{DbError, StateDiff, TxHash};

// ---------------------------------------------------------------------------
// ShadowDb: multi-version speculative state (D2: DatabaseRef + Arc)
// ---------------------------------------------------------------------------

pub struct ShadowDb {
    // Phase 2: replace with actual revm::DatabaseRef implementor
    _canonical: std::marker::PhantomData<()>,
    /// Per-round speculative diffs, keyed by commit_index.
    speculative: RwLock<BTreeMap<u64, StateDiff>>,
}

impl ShadowDb {
    pub fn new() -> Self {
        Self {
            _canonical: std::marker::PhantomData,
            speculative: RwLock::new(BTreeMap::new()),
        }
    }

    /// Apply a finalized StateDiff to canonical state.
    /// Drops all speculative diffs with commit_index ≤ applied index.
    pub fn commit(&self, diff: StateDiff) -> Result<(), DbError> {
        todo!()
    }

    /// Discard a speculative StateDiff (conflict resolution on HardCommit).
    pub fn discard(&self, commit_index: u64) {
        todo!()
    }

    /// Detect R/W conflicts between a new diff and existing speculative diffs.
    /// Returns TxHash values of conflicting transactions.
    pub fn detect_conflicts(&self, diff: &StateDiff) -> Vec<TxHash> {
        todo!()
    }
}

impl Default for ShadowDb {
    fn default() -> Self {
        Self::new()
    }
}

// Note: revm::DatabaseRef impl for ShadowDb will be added in Phase 2
// once the canonical DB type is wired in.
