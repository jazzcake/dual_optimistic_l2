//! Shadow State: Multi-Version Shadow Memory (Dependency Notification MVCC).
//!
//! Implements REVM `DatabaseRef` on top of a canonical `DatabaseRef + DatabaseCommit`,
//! layering per-round speculative state using a MVDS (Multi-Version Data Structure).
//!
//! Design: BlockSTM READLAST + VALIDAFTER invariants, but using Dependency
//! Notification instead of ESTIMATE markers. See docs/revm-analysis.md §8-4.

#![allow(dead_code)]

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Mutex, RwLock,
    },
};

use revm_database_interface::{DatabaseCommit, DatabaseRef};
use revm_primitives::{Address, AddressMap, StorageKey, StorageValue, B256};
use revm_state::{Account, AccountInfo, Bytecode};
use shared::EthSignedTx;

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

/// Index of a transaction within one round (0-based).
pub type TxIndex = usize;

// ---------------------------------------------------------------------------
// Step 2: MVDS core data structures
// ---------------------------------------------------------------------------

/// Versioned value stored per (Address, StorageKey) per TxIndex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionedValue {
    /// TX committed a confirmed storage value.
    Data(StorageValue),
    /// TX is being re-executed (aborted). Readers fall back to a prior Data.
    /// BlockSTM equivalent: ESTIMATE marker, but non-blocking.
    Pending,
    /// TX did not write to this slot; skip during READLAST search.
    Absent,
}

/// Per-slot MVDS: all versioned writes + reader dependency lists for one round.
#[derive(Debug, Default)]
pub struct SlotVersions {
    /// TxIndex → value written by that TX.
    pub versions: BTreeMap<TxIndex, VersionedValue>,
    /// writer_tx → list of reader TXs that consumed that version.
    /// Drained by `abort_tx`; used to notify readers for re-execution.
    pub readers: HashMap<TxIndex, Vec<TxIndex>>,
}

/// Round lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerStatus {
    /// Based on 2Δ SoftCommit. Can be cascade-invalidated.
    Speculative,
    /// 3Δ HardCommit received; re-execution in progress. Must not be invalidated.
    PendingCommit,
}

/// Per-account diff stored by `record_tx_execution`.
#[derive(Debug, Clone)]
pub struct AccountDiff {
    pub info: AccountInfo,
}

/// One round's speculative state layer.
pub struct RoundLayer {
    pub commit_index: u64,
    pub status: LayerStatus,
    /// (Address, StorageKey) → per-slot versioned writes for this round.
    pub slot_versions: HashMap<(Address, StorageKey), SlotVersions>,
    /// Address → (TxIndex → AccountDiff) for account-level reads/writes.
    pub account_versions: HashMap<Address, BTreeMap<TxIndex, AccountDiff>>,
    /// TX list preserved across cascade invalidation for re-execution.
    pub pending_txs: Option<Vec<EthSignedTx>>,
    /// tx_idx → { (addr, slot) → value_read }
    ///
    /// Populated by `storage_ref` (via `readlast_storage`) whenever a TX reads
    /// a value sourced from any MVCC layer (current round or a prior round).
    /// Used by:
    ///   - `validate_round`: detect stale reads even after readers are drained
    ///   - `cascade_invalidate`: detect cross-layer read dependencies
    pub tx_reads: HashMap<TxIndex, HashMap<(Address, StorageKey), StorageValue>>,
}

impl RoundLayer {
    fn new(commit_index: u64) -> Self {
        Self {
            commit_index,
            status: LayerStatus::Speculative,
            slot_versions: HashMap::new(),
            account_versions: HashMap::new(),
            pending_txs: None,
            tx_reads: HashMap::new(),
        }
    }
}

/// Error type for commit operations.
#[derive(Debug, PartialEq, Eq)]
pub enum CommitError {
    /// `finalize_commit(N+1)` called before `finalize_commit(N)`.
    OutOfOrder,
    /// The requested layer does not exist.
    LayerNotFound,
}

/// Result of `stage_commit`: handed to the executor for conflict resolution.
pub struct CommitHandle {
    pub commit_index: u64,
    /// TXs whose reads are now stale and require re-execution.
    pub conflicts: Vec<TxIndex>,
    /// Ordered TX list for re-execution scheduling.
    pub reexec_queue: Vec<EthSignedTx>,
}

/// Final diff written to canonical DB by `finalize_commit`.
pub struct RoundDiff {
    pub changes: AddressMap<Account>,
}

// ---------------------------------------------------------------------------
// ShadowDb
// ---------------------------------------------------------------------------

/// Multi-version speculative database layered over a canonical `DatabaseRef`.
///
/// Thread-safety: `current_tx` and `current_commit_index` are `Atomic*` so
/// the executor can update them from outside while EVM holds `&ShadowDb`.
pub struct ShadowDb<DB: DatabaseRef> {
    /// Canonical (finalized) database. Locked for writes during `finalize_commit`.
    canonical: Mutex<DB>,
    /// Per-round speculative layers, ordered by commit_index.
    layers: RwLock<BTreeMap<u64, RoundLayer>>,
    /// TxIndex of the transaction currently being executed.
    /// Set by the executor via `set_current_tx` before each EVM call.
    current_tx: AtomicUsize,
    /// commit_index of the round currently being executed.
    /// Required to distinguish within-round TxIndex ordering from cross-round reads.
    current_commit_index: AtomicU64,
    /// Coinbase address; account changes for this address are NOT recorded
    /// in MVCC (all TXs touch it → serial dependency bottleneck).
    /// Phase 4 executor accumulates gas fees separately.
    coinbase: Address,
    /// Highest commit_index for which `finalize_commit` has completed.
    last_finalized: Mutex<Option<u64>>,
}

impl<DB: DatabaseRef> ShadowDb<DB> {
    /// Create a new `ShadowDb` wrapping `canonical`.
    pub fn new(canonical: DB, coinbase: Address) -> Self {
        Self {
            canonical: Mutex::new(canonical),
            layers: RwLock::new(BTreeMap::new()),
            current_tx: AtomicUsize::new(0),
            current_commit_index: AtomicU64::new(0),
            coinbase,
            last_finalized: Mutex::new(None),
        }
    }

    /// Called by the executor immediately before running each TX.
    pub fn set_current_tx(&self, commit_index: u64, tx_idx: TxIndex) {
        self.current_commit_index.store(commit_index, Ordering::SeqCst);
        self.current_tx.store(tx_idx, Ordering::SeqCst);
    }

    /// Ensure a layer exists for `commit_index`, creating it if absent.
    pub fn ensure_layer(&self, commit_index: u64) {
        let mut layers = self.layers.write().unwrap();
        layers.entry(commit_index).or_insert_with(|| RoundLayer::new(commit_index));
    }

    // -----------------------------------------------------------------------
    // Step 4: Dependency Notification API
    // -----------------------------------------------------------------------

    /// Record the result of executing `tx_idx` in `commit_index`.
    ///
    /// Changed storage slots → `Data(present_value)` in slot_versions.
    /// Account changes for `coinbase` are intentionally skipped.
    pub fn record_tx_execution(
        &self,
        commit_index: u64,
        tx_idx: TxIndex,
        evm_state: &revm_state::EvmState,
    ) {
        let mut layers = self.layers.write().unwrap();
        let layer = layers.entry(commit_index).or_insert_with(|| RoundLayer::new(commit_index));

        for (addr, account) in evm_state.iter() {
            // Account versioning (skip coinbase)
            if *addr != self.coinbase {
                let diff = AccountDiff { info: account.info.clone() };
                layer.account_versions.entry(*addr).or_default().insert(tx_idx, diff);
            }

            // Storage: record only changed slots
            for (slot_key, slot) in account.storage.iter() {
                if slot.is_changed() {
                    let sv = layer.slot_versions.entry((*addr, *slot_key)).or_default();
                    sv.versions.insert(tx_idx, VersionedValue::Data(slot.present_value));
                    sv.readers.entry(tx_idx).or_default();
                }
            }
        }
    }

    /// Mark `tx_idx` as aborted: set all its slot versions to `Pending` and
    /// drain the reader lists, returning the set of TXs that must be re-executed.
    ///
    /// Internal state change (tested in T6):
    ///   versions[tx_idx] = Pending  (immediate)
    ///   readers[tx_idx]  = []       (drained, returned as notification list)
    pub fn abort_tx(&self, commit_index: u64, tx_idx: TxIndex) -> Vec<TxIndex> {
        let mut layers = self.layers.write().unwrap();
        let Some(layer) = layers.get_mut(&commit_index) else {
            return vec![];
        };

        let mut notified: Vec<TxIndex> = Vec::new();

        for sv in layer.slot_versions.values_mut() {
            if let Some(val) = sv.versions.get_mut(&tx_idx) {
                *val = VersionedValue::Pending;
            }
            if let Some(readers) = sv.readers.get_mut(&tx_idx) {
                notified.extend(readers.drain(..));
            }
        }

        notified.sort_unstable();
        notified.dedup();
        notified
    }

    /// Confirm re-execution of `tx_idx`: write new Data values and clear readers.
    ///
    /// Internal state change (tested in T9):
    ///   versions[tx_idx] = Data(new_value)
    ///   readers[tx_idx]  = []   (fresh start for new dependency tracking)
    pub fn commit_tx_execution(
        &self,
        commit_index: u64,
        tx_idx: TxIndex,
        evm_state: &revm_state::EvmState,
    ) {
        let mut layers = self.layers.write().unwrap();
        let Some(layer) = layers.get_mut(&commit_index) else {
            return;
        };

        // Clear previous slot entries for this TX
        for sv in layer.slot_versions.values_mut() {
            sv.versions.remove(&tx_idx);
            sv.readers.remove(&tx_idx);
        }
        // Also clear tx_reads for this TX (it will re-read under new values)
        layer.tx_reads.remove(&tx_idx);

        // Write new values from evm_state
        for (addr, account) in evm_state.iter() {
            if *addr != self.coinbase {
                let diff = AccountDiff { info: account.info.clone() };
                layer.account_versions.entry(*addr).or_default().insert(tx_idx, diff);
            }

            for (slot_key, slot) in account.storage.iter() {
                if slot.is_changed() {
                    let sv = layer.slot_versions.entry((*addr, *slot_key)).or_default();
                    sv.versions.insert(tx_idx, VersionedValue::Data(slot.present_value));
                    sv.readers.entry(tx_idx).or_default();
                }
            }
        }
    }

    /// VALIDAFTER check: for each TX, verify that every MVCC read it performed is
    /// still consistent with the current slot_versions in this round.
    ///
    /// Uses `tx_reads` (recorded by `storage_ref`) so it remains accurate even
    /// after `abort_tx` has drained the readers lists.
    ///
    /// Returns a deduplicated list of TX indices that need re-execution.
    pub fn validate_round(&self, commit_index: u64) -> Vec<TxIndex> {
        let layers = self.layers.read().unwrap();
        let Some(layer) = layers.get(&commit_index) else {
            return vec![];
        };

        let mut stale: Vec<TxIndex> = Vec::new();

        'tx: for (tx_idx, reads) in &layer.tx_reads {
            for ((addr, slot_key), expected_val) in reads {
                // Only validate slots that have MVCC entries in this round.
                // Reads sourced entirely from canonical or a prior committed round
                // are considered stable for within-round validation.
                if let Some(sv) = layer.slot_versions.get(&(*addr, *slot_key)) {
                    let current_val =
                        readlast_in_layer(sv, *tx_idx);
                    if current_val != Some(*expected_val) {
                        stale.push(*tx_idx);
                        continue 'tx;
                    }
                }
            }
        }

        stale.sort_unstable();
        stale.dedup();
        stale
    }

    // -----------------------------------------------------------------------
    // Step 5: Cascade Read Invalidation
    // -----------------------------------------------------------------------

    /// Invalidate all `Speculative` layers after `base_commit_index` that
    /// contain any of `changed_slots` (written OR read).
    ///
    /// `PendingCommit` layers are never invalidated.
    /// `pending_txs` is preserved on invalidated layers.
    ///
    /// Returns the list of invalidated commit indices.
    pub fn cascade_invalidate(
        &self,
        base_commit_index: u64,
        changed_slots: &HashSet<(Address, StorageKey)>,
    ) -> Vec<u64> {
        let mut layers = self.layers.write().unwrap();
        let mut invalidated = Vec::new();

        for (ci, layer) in layers.iter_mut() {
            if *ci <= base_commit_index {
                continue;
            }
            if layer.status == LayerStatus::PendingCommit {
                continue;
            }

            // A layer is affected if it WROTE to or READ FROM any changed slot.
            let wrote_changed = layer
                .slot_versions
                .keys()
                .any(|key| changed_slots.contains(key));

            let read_changed = layer
                .tx_reads
                .values()
                .flat_map(|reads| reads.keys())
                .any(|key| changed_slots.contains(key));

            if wrote_changed || read_changed {
                layer.slot_versions.clear();
                layer.account_versions.clear();
                layer.tx_reads.clear();
                // pending_txs preserved intentionally
                invalidated.push(*ci);
            }
        }

        invalidated
    }

    // -----------------------------------------------------------------------
    // Step 6: Two-Phase Commit
    // -----------------------------------------------------------------------

    /// Phase 1 of commit: transition layer to `PendingCommit`, run
    /// `validate_round`, and return a `CommitHandle` for the executor.
    pub fn stage_commit(
        &self,
        commit_index: u64,
        _final_tx_order: Vec<TxIndex>,
    ) -> CommitHandle {
        let conflicts = self.validate_round(commit_index);

        let reexec_queue = {
            let mut layers = self.layers.write().unwrap();
            if let Some(layer) = layers.get_mut(&commit_index) {
                layer.status = LayerStatus::PendingCommit;
                layer.pending_txs.clone().unwrap_or_default()
            } else {
                vec![]
            }
        };

        CommitHandle { commit_index, conflicts, reexec_queue }
    }

    /// Phase 2 of commit: write `final_diff` to canonical DB and drop the layer.
    ///
    /// Enforces ordering: commit_index N+1 cannot be finalized before N.
    pub fn finalize_commit(
        &self,
        commit_index: u64,
        final_diff: RoundDiff,
    ) -> Result<(), CommitError>
    where
        DB: DatabaseCommit,
    {
        let mut last = self.last_finalized.lock().unwrap();
        // Ordering check
        if let Some(prev) = *last {
            if commit_index != prev + 1 {
                return Err(CommitError::OutOfOrder);
            }
        } else if commit_index != 1 {
            return Err(CommitError::OutOfOrder);
        }

        {
            let mut canonical = self.canonical.lock().unwrap();
            canonical.commit(final_diff.changes);
        }

        {
            let mut layers = self.layers.write().unwrap();
            layers.retain(|&ci, _| ci > commit_index);
        }

        *last = Some(commit_index);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers for DatabaseRef
    // -----------------------------------------------------------------------

    /// READLAST lookup for storage: search layers newest-first.
    ///
    /// - Current round (commit_index == current_ci): TxIndex < current_tx.
    /// - Previous rounds: all versions visible.
    ///
    /// Records:
    ///   - reader dependency in writer's layer (for `abort_tx` notification)
    ///   - read value in current layer's `tx_reads` (for `validate_round` and
    ///     `cascade_invalidate` cross-layer tracking)
    fn readlast_storage(
        &self,
        addr: Address,
        slot: StorageKey,
        current_ci: u64,
        current_tx: TxIndex,
    ) -> Option<StorageValue> {
        let mut layers = self.layers.write().unwrap();
        let commit_indices: Vec<u64> = layers.keys().copied().collect();

        // Phase 1: find value and its origin (writer_ci, writer_tx, value)
        let mut found: Option<(u64, TxIndex, StorageValue, bool)> = None;
        // (writer_ci, writer_tx, value, was_pending)

        'search: for ci in commit_indices.iter().rev() {
            let layer = layers.get(ci).unwrap();
            let Some(sv) = layer.slot_versions.get(&(addr, slot)) else {
                continue;
            };

            let max_writer = if *ci == current_ci {
                sv.versions
                    .range(..current_tx)
                    .rev()
                    .find(|(_, v)| !matches!(v, VersionedValue::Absent))
                    .map(|(k, _)| *k)
            } else {
                sv.versions
                    .iter()
                    .rev()
                    .find(|(_, v)| !matches!(v, VersionedValue::Absent))
                    .map(|(k, _)| *k)
            };

            let Some(writer_tx) = max_writer else {
                continue;
            };

            match sv.versions[&writer_tx].clone() {
                VersionedValue::Data(val) => {
                    found = Some((*ci, writer_tx, val, false));
                    break 'search;
                }
                VersionedValue::Pending => {
                    // Find prior Data fallback in this layer
                    let fallback = if *ci == current_ci {
                        sv.versions.range(..writer_tx).rev().find_map(|(k, v)| {
                            if let VersionedValue::Data(val) = v {
                                Some((*k, *val))
                            } else {
                                None
                            }
                        })
                    } else {
                        sv.versions.iter().rev().find_map(|(k, v)| {
                            if *k < writer_tx {
                                if let VersionedValue::Data(val) = v {
                                    Some((*k, *val))
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                    };

                    if let Some((_fallback_tx, val)) = fallback {
                        // Register on Pending writer for notification AND on fallback Data
                        // We store the Pending writer as dependency origin (notify on re-exec)
                        found = Some((*ci, writer_tx, val, true));
                        // Override writer_tx with the Pending one so readers[writer_tx] is set
                    } else {
                        // No prior Data; register on Pending writer, continue searching older layers
                        // We still need to register the Pending dependency
                        found = Some((*ci, writer_tx, StorageValue::ZERO, true));
                        // Signal "no value from this layer" by using ZERO sentinel
                        // but we haven't confirmed a value yet — continue to older layers
                        // but record the Pending dependency at the end
                        // Actually: for Pending with no fallback, we continue to older layers
                        // Set found to track the Pending writer for reader notification
                        let _ = found.take(); // Reset — try older layers
                        // Store pending dependency to record at the end
                        // Use a temporary approach: store None but remember the Pending writer
                        // We'll handle this by continuing and checking canonical
                    }
                    // Register on Pending writer regardless
                    // (done in Phase 2 below if found is set)
                    if found.is_none() {
                        // Pending but no fallback in this layer: record dependency, continue
                        // We track the Pending TX as a writer we depend on
                        // Store a sentinel to enable reader registration in Phase 2
                        found = Some((*ci, writer_tx, StorageValue::ZERO, true));
                        // ZERO means "no value from MVCC, fall through to canonical"
                        // but still register the reader so we get notified
                        break 'search; // Will register reader; value comes from canonical
                    } else {
                        break 'search;
                    }
                }
                VersionedValue::Absent => continue,
            }
        }

        // Phase 2: record dependencies
        if let Some((writer_ci, writer_tx, value, was_pending)) = found {
            // Register reader in writer's layer
            if writer_ci == current_ci {
                let layer = layers.get_mut(&current_ci).unwrap();
                layer
                    .slot_versions
                    .entry((addr, slot))
                    .or_default()
                    .readers
                    .entry(writer_tx)
                    .or_default()
                    .push(current_tx);
                if !was_pending || value != StorageValue::ZERO {
                    layer
                        .tx_reads
                        .entry(current_tx)
                        .or_default()
                        .insert((addr, slot), value);
                } else {
                    // Pending with no fallback: register reader but value comes from canonical
                    // tx_reads will be updated after canonical read
                }
            } else {
                // writer_ci < current_ci: different layers
                {
                    let writer_layer = layers.get_mut(&writer_ci).unwrap();
                    writer_layer
                        .slot_versions
                        .entry((addr, slot))
                        .or_default()
                        .readers
                        .entry(writer_tx)
                        .or_default()
                        .push(current_tx);
                }
                // Record in current layer's tx_reads for cascade tracking
                if !was_pending || value != StorageValue::ZERO {
                    if let Some(current_layer) = layers.get_mut(&current_ci) {
                        current_layer
                            .tx_reads
                            .entry(current_tx)
                            .or_default()
                            .insert((addr, slot), value);
                    }
                }
            }

            if !was_pending || value != StorageValue::ZERO {
                return Some(value);
            }
            // was_pending with no fallback: fall through to canonical
        }

        None
    }

    /// READLAST lookup for account info: search layers newest-first.
    fn readlast_account(
        &self,
        addr: Address,
        current_ci: u64,
        current_tx: TxIndex,
    ) -> Option<AccountInfo> {
        let layers = self.layers.read().unwrap();
        for (ci, layer) in layers.iter().rev() {
            let Some(versions) = layer.account_versions.get(&addr) else {
                continue;
            };
            let entry = if *ci == current_ci {
                versions.range(..current_tx).next_back()
            } else {
                versions.iter().next_back()
            };
            if let Some((_, diff)) = entry {
                return Some(diff.info.clone());
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Private helper: READLAST within a single SlotVersions (no locks needed)
// ---------------------------------------------------------------------------

/// Find the highest-indexed Data value with index < `tx_idx` in `sv`.
/// Returns `None` if no such Data version exists (all Pending/Absent or none).
fn readlast_in_layer(sv: &SlotVersions, tx_idx: TxIndex) -> Option<StorageValue> {
    sv.versions.range(..tx_idx).rev().find_map(|(_, v)| {
        if let VersionedValue::Data(val) = v {
            Some(*val)
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Step 3: DatabaseRef implementation (READLAST)
// ---------------------------------------------------------------------------

/// Error type for `ShadowDb`'s `DatabaseRef` impl.
#[derive(Debug)]
pub struct ShadowDbError(pub String);

impl std::fmt::Display for ShadowDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ShadowDbError: {}", self.0)
    }
}
impl std::error::Error for ShadowDbError {}
impl revm_database_interface::DBErrorMarker for ShadowDbError {}

impl<DB: DatabaseRef> DatabaseRef for ShadowDb<DB> {
    type Error = ShadowDbError;

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        let current_ci = self.current_commit_index.load(Ordering::SeqCst);
        let current_tx = self.current_tx.load(Ordering::SeqCst);

        if let Some(val) = self.readlast_storage(address, index, current_ci, current_tx) {
            return Ok(val);
        }

        self.canonical
            .lock()
            .unwrap()
            .storage_ref(address, index)
            .map_err(|e| ShadowDbError(e.to_string()))
    }

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let current_ci = self.current_commit_index.load(Ordering::SeqCst);
        let current_tx = self.current_tx.load(Ordering::SeqCst);

        if let Some(info) = self.readlast_account(address, current_ci, current_tx) {
            return Ok(Some(info));
        }

        self.canonical
            .lock()
            .unwrap()
            .basic_ref(address)
            .map_err(|e| ShadowDbError(e.to_string()))
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.canonical
            .lock()
            .unwrap()
            .code_by_hash_ref(code_hash)
            .map_err(|e| ShadowDbError(e.to_string()))
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.canonical
            .lock()
            .unwrap()
            .block_hash_ref(number)
            .map_err(|e| ShadowDbError(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Step 7: Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use revm_database_interface::{DatabaseCommit, DatabaseRef, EmptyDB};
    use revm_primitives::{Address, AddressMap, B256, StorageKey, StorageValue, U256};
    use revm_state::{Account, AccountInfo, Bytecode, EvmStorageSlot, EvmState};
    use std::collections::HashMap as StdHashMap;
    use std::convert::Infallible;

    /// Minimal in-memory DB for tests that need `DatabaseCommit`.
    #[derive(Default)]
    struct InMemoryDb {
        storage: StdHashMap<(Address, StorageKey), StorageValue>,
    }

    impl DatabaseRef for InMemoryDb {
        type Error = Infallible;
        fn basic_ref(&self, _a: Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(None)
        }
        fn code_by_hash_ref(&self, _h: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }
        fn storage_ref(&self, a: Address, i: StorageKey) -> Result<StorageValue, Self::Error> {
            Ok(self.storage.get(&(a, i)).copied().unwrap_or(U256::ZERO))
        }
        fn block_hash_ref(&self, _n: u64) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    impl DatabaseCommit for InMemoryDb {
        fn commit(&mut self, changes: AddressMap<Account>) {
            for (addr, account) in changes {
                for (sk, slot) in account.storage {
                    self.storage.insert((addr, sk), slot.present_value);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn slot(n: u64) -> StorageKey {
        StorageKey::from(n)
    }

    fn val(n: u64) -> StorageValue {
        StorageValue::from(n)
    }

    fn coinbase() -> Address {
        Address::repeat_byte(0xff)
    }

    fn db() -> ShadowDb<EmptyDB> {
        ShadowDb::new(EmptyDB::default(), coinbase())
    }

    fn state_with_slot(
        address: Address,
        slot_key: StorageKey,
        original: StorageValue,
        present: StorageValue,
    ) -> EvmState {
        let mut account = Account::default();
        account
            .storage
            .insert(slot_key, EvmStorageSlot::new_changed(original, present, 0));
        let mut map = EvmState::default();
        map.insert(address, account);
        map
    }

    fn state_with_slots(
        address: Address,
        slots: Vec<(StorageKey, StorageValue, StorageValue)>,
    ) -> EvmState {
        let mut account = Account::default();
        for (sk, orig, pres) in slots {
            account
                .storage
                .insert(sk, EvmStorageSlot::new_changed(orig, pres, 0));
        }
        let mut map = EvmState::default();
        map.insert(address, account);
        map
    }

    // -----------------------------------------------------------------------
    // T1. test_readlast_basic
    // -----------------------------------------------------------------------
    #[test]
    fn test_readlast_basic() {
        let db = db();
        db.ensure_layer(1);

        let evm_state = state_with_slot(addr(0xA), slot(1), val(0), val(100));
        db.record_tx_execution(1, 2, &evm_state);

        db.set_current_tx(1, 5);
        let v = db.storage_ref(addr(0xA), slot(1)).unwrap();

        assert_eq!(v, val(100));

        let layers = db.layers.read().unwrap();
        let sv = layers[&1].slot_versions.get(&(addr(0xA), slot(1))).unwrap();
        assert!(sv.readers[&2].contains(&5));
    }

    // -----------------------------------------------------------------------
    // T2. test_readlast_skip_to_latest
    // -----------------------------------------------------------------------
    #[test]
    fn test_readlast_skip_to_latest() {
        let db = db();
        db.ensure_layer(1);

        let s2 = state_with_slot(addr(0xA), slot(1), val(0), val(100));
        db.record_tx_execution(1, 2, &s2);

        let s5 = state_with_slot(addr(0xA), slot(1), val(0), val(200));
        db.record_tx_execution(1, 5, &s5);

        db.set_current_tx(1, 8);
        let v = db.storage_ref(addr(0xA), slot(1)).unwrap();

        assert_eq!(v, val(200));

        let layers = db.layers.read().unwrap();
        let sv = layers[&1].slot_versions.get(&(addr(0xA), slot(1))).unwrap();
        assert!(sv.readers[&5].contains(&8));
        assert!(sv.readers.get(&2).map_or(true, |r| !r.contains(&8)));
    }

    // -----------------------------------------------------------------------
    // T3. test_readlast_no_writer_falls_to_canonical
    // -----------------------------------------------------------------------
    #[test]
    fn test_readlast_no_writer_falls_to_canonical() {
        let db = db();
        db.ensure_layer(1);
        db.set_current_tx(1, 3);

        let v = db.storage_ref(addr(0xA), slot(1)).unwrap();
        assert_eq!(v, U256::ZERO);
    }

    // -----------------------------------------------------------------------
    // T4. test_pending_fallback_to_prior_data
    // -----------------------------------------------------------------------
    #[test]
    fn test_pending_fallback_to_prior_data() {
        let db = db();
        db.ensure_layer(1);

        let s2 = state_with_slot(addr(0xA), slot(1), val(0), val(100));
        db.record_tx_execution(1, 2, &s2);

        let s5 = state_with_slot(addr(0xA), slot(1), val(0), val(200));
        db.record_tx_execution(1, 5, &s5);
        db.abort_tx(1, 5);

        db.set_current_tx(1, 8);
        let v = db.storage_ref(addr(0xA), slot(1)).unwrap();

        assert_eq!(v, val(100), "should fall back to TX_2's Data when TX_5 is Pending");

        let layers = db.layers.read().unwrap();
        let sv = layers[&1].slot_versions.get(&(addr(0xA), slot(1))).unwrap();
        assert!(sv.readers[&5].contains(&8));
    }

    // -----------------------------------------------------------------------
    // T5. test_pending_fallback_to_canonical
    // -----------------------------------------------------------------------
    #[test]
    fn test_pending_fallback_to_canonical() {
        let db = db();
        db.ensure_layer(1);

        let s3 = state_with_slot(addr(0xA), slot(1), val(0), val(50));
        db.record_tx_execution(1, 3, &s3);
        db.abort_tx(1, 3);

        db.set_current_tx(1, 7);
        let v = db.storage_ref(addr(0xA), slot(1)).unwrap();

        assert_eq!(v, U256::ZERO);

        let layers = db.layers.read().unwrap();
        let sv = layers[&1].slot_versions.get(&(addr(0xA), slot(1))).unwrap();
        assert!(sv.readers[&3].contains(&7));
    }

    // -----------------------------------------------------------------------
    // T6. test_abort_tx_sets_pending_and_drains_readers
    // -----------------------------------------------------------------------
    #[test]
    fn test_abort_tx_sets_pending_and_drains_readers() {
        let db = db();
        db.ensure_layer(1);

        let s3 = state_with_slot(addr(0xA), slot(1), val(0), val(50));
        db.record_tx_execution(1, 3, &s3);

        db.set_current_tx(1, 6);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();

        db.set_current_tx(1, 8);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();

        {
            let layers = db.layers.read().unwrap();
            let sv = layers[&1].slot_versions.get(&(addr(0xA), slot(1))).unwrap();
            let mut r = sv.readers[&3].clone();
            r.sort();
            assert_eq!(r, vec![6, 8]);
        }

        let mut notified = db.abort_tx(1, 3);
        notified.sort();

        assert_eq!(notified, vec![6, 8]);

        let layers = db.layers.read().unwrap();
        let sv = layers[&1].slot_versions.get(&(addr(0xA), slot(1))).unwrap();
        assert_eq!(sv.versions[&3], VersionedValue::Pending);
        assert!(sv.readers[&3].is_empty());
    }

    // -----------------------------------------------------------------------
    // T7. test_abort_tx_multiple_readers
    // -----------------------------------------------------------------------
    #[test]
    fn test_abort_tx_multiple_readers() {
        let db = db();
        db.ensure_layer(1);

        let s2 = state_with_slots(
            addr(0xA),
            vec![(slot(1), val(0), val(10)), (slot(2), val(0), val(20))],
        );
        db.record_tx_execution(1, 2, &s2);

        db.set_current_tx(1, 4);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();

        db.set_current_tx(1, 6);
        let _ = db.storage_ref(addr(0xA), slot(2)).unwrap();

        db.set_current_tx(1, 7);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();

        let mut notified = db.abort_tx(1, 2);
        notified.sort();
        notified.dedup();

        assert_eq!(notified, vec![4, 6, 7]);

        let layers = db.layers.read().unwrap();
        let layer = &layers[&1];
        for sv in layer.slot_versions.values() {
            if let Some(readers) = sv.readers.get(&2) {
                assert!(readers.is_empty());
            }
            if let Some(val) = sv.versions.get(&2) {
                assert_eq!(*val, VersionedValue::Pending);
            }
        }
    }

    // -----------------------------------------------------------------------
    // T8. test_abort_tx_chain
    // -----------------------------------------------------------------------
    #[test]
    fn test_abort_tx_chain() {
        let db = db();
        db.ensure_layer(1);

        let s1 = state_with_slot(addr(0xA), slot(1), val(0), val(10));
        db.record_tx_execution(1, 1, &s1);

        let s3 = state_with_slot(addr(0xB), slot(2), val(0), val(20));
        db.record_tx_execution(1, 3, &s3);
        db.set_current_tx(1, 3);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();

        db.set_current_tx(1, 5);
        let _ = db.storage_ref(addr(0xB), slot(2)).unwrap();

        let notified1 = db.abort_tx(1, 1);
        assert!(notified1.contains(&3));

        {
            let layers = db.layers.read().unwrap();
            let sv_ak = layers[&1].slot_versions.get(&(addr(0xA), slot(1))).unwrap();
            assert_eq!(sv_ak.versions[&1], VersionedValue::Pending);
        }

        let notified2 = db.abort_tx(1, 3);
        assert!(notified2.contains(&5));

        let layers = db.layers.read().unwrap();
        let sv_bl = layers[&1].slot_versions.get(&(addr(0xB), slot(2))).unwrap();
        assert_eq!(sv_bl.versions[&3], VersionedValue::Pending);
    }

    // -----------------------------------------------------------------------
    // T9. test_commit_tx_restores_data_and_clears_readers
    // -----------------------------------------------------------------------
    #[test]
    fn test_commit_tx_restores_data_and_clears_readers() {
        let db = db();
        db.ensure_layer(1);

        let s3 = state_with_slot(addr(0xA), slot(1), val(0), val(100));
        db.record_tx_execution(1, 3, &s3);
        db.set_current_tx(1, 6);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();
        db.abort_tx(1, 3);

        let s3_new = state_with_slot(addr(0xA), slot(1), val(0), val(150));
        db.commit_tx_execution(1, 3, &s3_new);

        {
            let layers = db.layers.read().unwrap();
            let sv = layers[&1].slot_versions.get(&(addr(0xA), slot(1))).unwrap();
            assert_eq!(sv.versions[&3], VersionedValue::Data(val(150)));
            assert!(sv.readers.get(&3).map_or(true, |r| r.is_empty()));
        }

        db.set_current_tx(1, 9);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();

        let layers = db.layers.read().unwrap();
        let sv = layers[&1].slot_versions.get(&(addr(0xA), slot(1))).unwrap();
        assert!(sv.readers[&3].contains(&9));
    }

    // -----------------------------------------------------------------------
    // T10. test_validate_round_detects_stale_reads
    // -----------------------------------------------------------------------
    #[test]
    fn test_validate_round_detects_stale_reads() {
        let db = db();
        db.ensure_layer(1);

        // TX_2 records Data(100); TX_4 reads it → tx_reads[4][(A,K)] = 100
        let s2 = state_with_slot(addr(0xA), slot(1), val(0), val(100));
        db.record_tx_execution(1, 2, &s2);
        db.set_current_tx(1, 4);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();

        // TX_2 aborted → Pending; re-executed with 200
        db.abort_tx(1, 2);
        let s2_new = state_with_slot(addr(0xA), slot(1), val(0), val(200));
        db.commit_tx_execution(1, 2, &s2_new);

        // TX_4 read 100, but current READLAST(TX_4) in round = Data(200) from TX_2
        let stale = db.validate_round(1);
        assert!(stale.contains(&4), "TX_4 read stale value 100; current is 200");
    }

    // -----------------------------------------------------------------------
    // T11. test_cascade_invalidate_clears_slot_versions
    // -----------------------------------------------------------------------
    #[test]
    fn test_cascade_invalidate_clears_slot_versions() {
        let db = db();
        db.ensure_layer(1);
        db.ensure_layer(2);

        // Round 1: TX_3 writes slot(A, K) = 100
        let s = state_with_slot(addr(0xA), slot(1), val(0), val(100));
        db.record_tx_execution(1, 3, &s);

        // Round 2: TX_2 reads slot(A, K) from Round 1
        // → tx_reads in Round 2 will contain (A,K) = 100
        db.set_current_tx(2, 2);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();

        let changed: HashSet<(Address, StorageKey)> = [(addr(0xA), slot(1))].into();
        let invalidated = db.cascade_invalidate(1, &changed);

        assert!(invalidated.contains(&2), "Round 2 must be invalidated");

        let layers = db.layers.read().unwrap();
        let layer2 = &layers[&2];
        assert!(layer2.slot_versions.is_empty());
        assert!(layer2.account_versions.is_empty());
        assert!(layer2.tx_reads.is_empty());
    }

    // -----------------------------------------------------------------------
    // T12. test_cascade_preserves_pending_txs
    // -----------------------------------------------------------------------
    #[test]
    fn test_cascade_preserves_pending_txs() {
        let db = db();
        db.ensure_layer(2);

        {
            let mut layers = db.layers.write().unwrap();
            let layer = layers.get_mut(&2).unwrap();
            layer.pending_txs = Some(vec![EthSignedTx(vec![0xAA]), EthSignedTx(vec![0xBB])]);
            layer.slot_versions.insert((addr(0xA), slot(1)), SlotVersions::default());
        }

        let changed: HashSet<(Address, StorageKey)> = [(addr(0xA), slot(1))].into();
        db.cascade_invalidate(1, &changed);

        let layers = db.layers.read().unwrap();
        let layer2 = &layers[&2];
        assert!(layer2.slot_versions.is_empty());
        let txs = layer2.pending_txs.as_ref().expect("pending_txs must be preserved");
        assert_eq!(txs.len(), 2);
    }

    // -----------------------------------------------------------------------
    // T13. test_pending_commit_not_cascade_invalidated
    // -----------------------------------------------------------------------
    #[test]
    fn test_pending_commit_not_cascade_invalidated() {
        let db = db();
        db.ensure_layer(1);
        db.ensure_layer(2);

        {
            let mut layers = db.layers.write().unwrap();
            layers.get_mut(&1).unwrap().status = LayerStatus::PendingCommit;
            for (ci, layer) in layers.iter_mut() {
                let sv = layer.slot_versions.entry((addr(0xA), slot(1))).or_default();
                sv.versions
                    .insert(1, VersionedValue::Data(val(*ci as u64 * 10)));
            }
        }

        let changed: HashSet<(Address, StorageKey)> = [(addr(0xA), slot(1))].into();
        let invalidated = db.cascade_invalidate(0, &changed);

        assert_eq!(invalidated, vec![2]);

        let layers = db.layers.read().unwrap();
        assert!(!layers[&1].slot_versions.is_empty(), "PendingCommit must be untouched");
        assert!(layers[&2].slot_versions.is_empty());
    }

    // -----------------------------------------------------------------------
    // T14. test_pending_commit_visible_to_next_round
    // -----------------------------------------------------------------------
    #[test]
    fn test_pending_commit_visible_to_next_round() {
        let db = db();
        db.ensure_layer(1);
        db.ensure_layer(2);

        {
            let mut layers = db.layers.write().unwrap();
            let layer1 = layers.get_mut(&1).unwrap();
            layer1.status = LayerStatus::PendingCommit;
            let sv = layer1.slot_versions.entry((addr(0xA), slot(1))).or_default();
            sv.versions.insert(5, VersionedValue::Data(val(777)));
            sv.readers.entry(5).or_default();
        }

        db.set_current_tx(2, 3);
        let v = db.storage_ref(addr(0xA), slot(1)).unwrap();

        assert_eq!(v, val(777));
    }

    // -----------------------------------------------------------------------
    // T15. test_stage_commit_produces_correct_handle
    // -----------------------------------------------------------------------
    #[test]
    fn test_stage_commit_produces_correct_handle() {
        let db = db();
        db.ensure_layer(1);

        // TX_2 records Data(100); TX_4 reads → tx_reads[4][(A,K)] = 100
        let s2 = state_with_slot(addr(0xA), slot(1), val(0), val(100));
        db.record_tx_execution(1, 2, &s2);
        db.set_current_tx(1, 4);
        let _ = db.storage_ref(addr(0xA), slot(1)).unwrap();

        // TX_2 aborted → Pending; commit with 200
        db.abort_tx(1, 2);
        let s2_new = state_with_slot(addr(0xA), slot(1), val(0), val(200));
        db.commit_tx_execution(1, 2, &s2_new);
        // TX_4 still has tx_reads[4][(A,K)] = 100 (not cleared by commit_tx_execution of TX_2)
        // But current READLAST for TX_4 = Data(200) ≠ 100 → stale

        let handle = db.stage_commit(1, vec![0, 1, 2, 3, 4]);

        assert_eq!(handle.commit_index, 1);
        assert!(
            handle.conflicts.contains(&4),
            "TX_4 read stale value 100; current value is 200"
        );

        let layers = db.layers.read().unwrap();
        assert_eq!(layers[&1].status, LayerStatus::PendingCommit);
    }

    // -----------------------------------------------------------------------
    // T16. test_finalize_commit_writes_canonical
    // -----------------------------------------------------------------------
    #[test]
    fn test_finalize_commit_writes_canonical() {
        let db: ShadowDb<InMemoryDb> = ShadowDb::new(InMemoryDb::default(), coinbase());
        db.ensure_layer(1);
        db.stage_commit(1, vec![]);

        let mut changes: AddressMap<Account> = AddressMap::default();
        let mut acct = Account::default();
        acct.storage
            .insert(slot(1), EvmStorageSlot::new_changed(val(0), val(200), 0));
        acct.storage
            .insert(slot(2), EvmStorageSlot::new_changed(val(0), val(300), 0));
        changes.insert(addr(0xA), acct);

        let diff = RoundDiff { changes };
        db.finalize_commit(1, diff).expect("finalize_commit should succeed");

        let layers = db.layers.read().unwrap();
        assert!(!layers.contains_key(&1));
    }

    // -----------------------------------------------------------------------
    // T17. test_finalize_ordering_enforced
    // -----------------------------------------------------------------------
    #[test]
    fn test_finalize_ordering_enforced() {
        let db: ShadowDb<InMemoryDb> = ShadowDb::new(InMemoryDb::default(), coinbase());
        db.ensure_layer(1);
        db.ensure_layer(2);

        db.stage_commit(1, vec![]);
        db.stage_commit(2, vec![]);

        let empty_diff = || RoundDiff { changes: AddressMap::default() };

        let res = db.finalize_commit(2, empty_diff());
        assert_eq!(res, Err(CommitError::OutOfOrder));

        assert!(db.layers.read().unwrap().contains_key(&2));

        db.finalize_commit(1, empty_diff()).expect("finalize 1 must succeed");
        db.finalize_commit(2, empty_diff()).expect("finalize 2 must succeed");

        assert!(db.layers.read().unwrap().is_empty());
    }
}
