use std::collections::BTreeMap;

use shared::{EthSignedTx, Round};

/// Number of rounds between consecutive leader rounds (wave length).
const WAVE_LENGTH: Round = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryStatus {
    Queued,
    Dispatched,
}

#[derive(Debug)]
struct PendingEntry {
    txs: Vec<EthSignedTx>,
    status: EntryStatus,
}

/// Decision returned when a HardCommit arrives for a given round.
#[derive(Debug, PartialEq, Eq)]
pub enum HardCommitDecision {
    /// A SoftCommit for this round was already dispatched to the executor.
    /// The executor already holds the txs; we only need to finalize them.
    Commit { round: Round, commit_index: u64 },
    /// No SoftCommit was dispatched for this round.
    /// The executor must run the txs from scratch (non-optimistic path).
    FreshExecution { round: Round, commit_index: u64 },
}

/// Out-of-order buffer for SoftCommit transactions.
///
/// Guarantees that [`drain_dispatchable`] emits entries in ascending round
/// order, even when SoftCommits arrive out of order from the consensus layer.
pub struct PendingQueue {
    entries: BTreeMap<Round, PendingEntry>,
    /// Next leader round that should be dispatched to the executor.
    next_dispatch_round: Round,
    /// Queue-depth threshold exposed for the backpressure controller.
    pub threshold: usize,
}

impl PendingQueue {
    /// Create a new queue. Dispatch begins at `first_leader_round` (typically 3).
    pub fn new(first_leader_round: Round, threshold: usize) -> Self {
        debug_assert!(first_leader_round > 0, "first leader round must be positive");
        debug_assert!(threshold > 0, "threshold must be positive");
        Self {
            entries: BTreeMap::new(),
            next_dispatch_round: first_leader_round,
            threshold,
        }
    }

    /// Buffer a SoftCommit payload for `round`.
    pub fn insert(&mut self, round: Round, txs: Vec<EthSignedTx>) {
        debug_assert!(round > 0, "round must be positive");
        debug_assert!(
            !self.entries.contains_key(&round),
            "duplicate SoftCommit for round {}",
            round
        );
        self.entries.insert(round, PendingEntry { txs, status: EntryStatus::Queued });
    }

    /// Drain all consecutive dispatchable entries starting from `next_dispatch_round`.
    ///
    /// Returns `(round, txs)` pairs in ascending round order.
    /// Each returned entry is marked [`EntryStatus::Dispatched`] so a later
    /// [`on_hard_commit`] can distinguish "already sent" from "never seen".
    pub fn drain_dispatchable(&mut self) -> Vec<(Round, Vec<EthSignedTx>)> {
        let mut result = Vec::new();
        loop {
            let round = self.next_dispatch_round;
            match self.entries.get_mut(&round) {
                Some(entry) if matches!(entry.status, EntryStatus::Queued) => {
                    let txs = entry.txs.clone();
                    entry.status = EntryStatus::Dispatched;
                    self.next_dispatch_round += WAVE_LENGTH;
                    result.push((round, txs));
                }
                _ => break,
            }
        }
        result
    }

    /// Handle a HardCommit for `round` with the given `commit_index`.
    ///
    /// Removes the buffered entry (if any) and returns the appropriate decision:
    /// - [`HardCommitDecision::Commit`] if the SoftCommit was already dispatched.
    /// - [`HardCommitDecision::FreshExecution`] otherwise.
    pub fn on_hard_commit(&mut self, round: Round, commit_index: u64) -> HardCommitDecision {
        debug_assert!(round > 0, "round must be positive");
        debug_assert!(commit_index > 0, "commit_index must be positive");
        match self.entries.remove(&round) {
            Some(entry) if matches!(entry.status, EntryStatus::Dispatched) => {
                HardCommitDecision::Commit { round, commit_index }
            }
            _ => HardCommitDecision::FreshExecution { round, commit_index },
        }
    }

    /// Total number of entries in the queue (queued + dispatched).
    pub fn depth(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(byte: u8) -> EthSignedTx {
        EthSignedTx(vec![byte])
    }

    #[test]
    fn test_in_order_processing() {
        let mut pq = PendingQueue::new(3, 16);
        pq.insert(3, vec![tx(1)]);
        pq.insert(6, vec![tx(2)]);

        let dispatched = pq.drain_dispatchable();
        assert_eq!(dispatched.len(), 2);
        assert_eq!(dispatched[0].0, 3, "R3 must be dispatched first");
        assert_eq!(dispatched[1].0, 6, "R6 must be dispatched second");
    }

    #[test]
    fn test_out_of_order_reorder() {
        let mut pq = PendingQueue::new(3, 16);

        // R6 arrives first — R3 is not yet in the buffer, nothing can be dispatched
        pq.insert(6, vec![tx(2)]);
        let d1 = pq.drain_dispatchable();
        assert!(d1.is_empty(), "R6 must be buffered until R3 arrives");

        // R3 arrives — now both R3 and R6 are consecutive and should drain in order
        pq.insert(3, vec![tx(1)]);
        let d2 = pq.drain_dispatchable();
        assert_eq!(d2.len(), 2);
        assert_eq!(d2[0].0, 3, "R3 must come first");
        assert_eq!(d2[1].0, 6, "R6 must come second");
    }

    #[test]
    fn test_hard_commit_match() {
        let mut pq = PendingQueue::new(3, 16);
        pq.insert(3, vec![tx(1)]);
        let _ = pq.drain_dispatchable(); // marks R3 as Dispatched

        let decision = pq.on_hard_commit(3, 1);
        assert_eq!(
            decision,
            HardCommitDecision::Commit { round: 3, commit_index: 1 },
            "HardCommit after dispatch should confirm Commit"
        );
    }

    #[test]
    fn test_hard_commit_mismatch() {
        let mut pq = PendingQueue::new(3, 16);
        // No SoftCommit for R3 was ever inserted or dispatched
        let decision = pq.on_hard_commit(3, 1);
        assert_eq!(
            decision,
            HardCommitDecision::FreshExecution { round: 3, commit_index: 1 },
            "HardCommit without prior SoftCommit dispatch should require FreshExecution"
        );
    }
}
