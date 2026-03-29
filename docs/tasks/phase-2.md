# Phase 2: Shadow State 구현

**상태**: 🔄 진행중
**목표**: REVM `DatabaseRef`를 구현하는 Multi-Version Shadow Memory를 완성한다.
Dependency Notification MVCC 방식. 합의/스케줄러 없이 독립적으로 테스트 가능해야 한다.

설계 근거: `docs/revm-analysis.md` §7, §8

---

## 작업 목록

### Step 1: shared 타입 교체
- [ ] `shared` crate에 revm-primitives / revm-state path 의존성 추가
- [ ] placeholder 타입을 revm 실제 타입으로 교체 (Address, U256, B256, AccountInfo, StorageKey, StorageValue)
- [ ] `cargo check --workspace` 통과

### Step 2: MVDS 핵심 자료구조
- [ ] `VersionedValue` enum (`Data` / `Pending` / `Absent`)
- [ ] `SlotVersions` 구조체 (versions BTreeMap + readers 의존성 목록)
- [ ] `LayerStatus` enum (`Speculative` / `PendingCommit`)
- [ ] `RoundLayer` 구조체 (slot_versions + account_versions + pending_txs)
- [ ] `ShadowDb<DB>` 제네릭 구조체 (layers + canonical Mutex + current_tx AtomicUsize)

### Step 3: DatabaseRef 구현 (READLAST)
- [ ] `storage_ref` — READLAST: TxIndex < current_tx 최대 버전, Pending 폴백, readers 기록
- [ ] `basic_ref` — account_versions 동일 탐색 → canonical 위임
- [ ] `code_by_hash_ref` / `block_hash_ref` — canonical 위임

### Step 4: Dependency Notification API
- [ ] `record_tx_execution` — REVM EvmState → SlotVersions 기록
- [ ] `abort_tx` — Pending 전환 + readers drain → 연쇄 재실행 목록 반환
- [ ] `commit_tx_execution` — Data 확정 + readers 초기화
- [ ] `validate_round` — VALIDAFTER 불변 조건 검증, 추가 재실행 목록 반환

### Step 5: Cascade Read Invalidation
- [ ] `cascade_invalidate` — 변경 슬롯 기반 N 이후 Speculative 레이어 무효화
- [ ] PendingCommit 레이어 cascade 제외
- [ ] 무효화 시 slot_versions / account_versions 제거, pending_txs 보존

### Step 6: Two-Phase Commit 파이프라인
- [ ] `stage_commit` — PendingCommit 전환 + CommitHandle 반환
- [ ] `finalize_commit` — canonical write + 레이어 Drop
- [ ] commit_index 순서 강제 (N 미완료 시 N+1 finalize 차단)
- [ ] PendingCommit 슬롯 가시성 — 이후 Speculative 라운드 읽기 기준 포함

### Step 7: 테스트 (시나리오 기반, 총 17개)
- [ ] `test_readlast_basic`
- [ ] `test_readlast_skip_to_latest`
- [ ] `test_readlast_no_writer_falls_to_canonical`
- [ ] `test_pending_fallback_to_prior_data`
- [ ] `test_pending_fallback_to_canonical`
- [ ] `test_abort_tx_sets_pending_and_drains_readers`
- [ ] `test_abort_tx_multiple_readers`
- [ ] `test_abort_tx_chain`
- [ ] `test_commit_tx_restores_data_and_clears_readers`
- [ ] `test_validate_round_detects_stale_reads`
- [ ] `test_cascade_invalidate_clears_slot_versions`
- [ ] `test_cascade_preserves_pending_txs`
- [ ] `test_pending_commit_not_cascade_invalidated`
- [ ] `test_pending_commit_visible_to_next_round`
- [ ] `test_stage_commit_produces_correct_handle`
- [ ] `test_finalize_commit_writes_canonical`
- [ ] `test_finalize_ordering_enforced`

---

## 실행 계획 (Execution Plan)

**구현 원칙**: 테스트 먼저 작성(TDD) → 구현 → `cargo test -p shadow-state` 통과.

---

### Step 1: `shared` 타입을 revm-primitives로 교체

```
shared/Cargo.toml:
  revm-primitives  = { path = "../../extern/revm/crates/primitives" }
  revm-state       = { path = "../../extern/revm/crates/state" }
  revm-database-interface = { path = "../../extern/revm/crates/database/interface" }

shared/src/lib.rs:
  Address, U256, B256          → revm_primitives::{Address, U256, B256}
  StorageKey, StorageValue     → revm_primitives::{StorageKey, StorageValue}
  AccountInfo                  → revm_state::AccountInfo
  자체 유지 타입: EthSignedTx, BlockRef, Round, ConsensusEvent, TxBatch 등
```

산출물: `cargo check --workspace` 경고 없이 통과.

---

### Step 2: MVDS 핵심 자료구조

```rust
pub type TxIndex = usize;

/// Per-slot versioned value within one round.
pub enum VersionedValue {
    /// TX가 확정적으로 기록한 값.
    Data(StorageValue),
    /// TX 재실행 중. 이 버전을 읽은 TX는 재실행 통보 대기.
    /// BlockSTM ESTIMATE와 달리 blocking 없이 이전 Data로 폴백.
    Pending,
    /// 이 TX가 해당 슬롯에 쓰지 않음 (탐색 계속).
    Absent,
}

/// READLAST 구현 단위: 슬롯 하나의 모든 버전 + 의존성 목록.
pub struct SlotVersions {
    /// TxIndex → 해당 TX가 기록한 값.
    pub versions: BTreeMap<TxIndex, VersionedValue>,
    /// writer_tx → 이 버전을 읽어간 reader TX 목록.
    /// abort_tx 호출 시 drain → 재실행 통보.
    pub readers: HashMap<TxIndex, Vec<TxIndex>>,
}

pub enum LayerStatus {
    Speculative,   // 2Δ SoftCommit 기반. cascade 무효화 가능.
    PendingCommit, // 3Δ HardCommit 수신. 재실행 진행 중. 무효화 불가.
}

pub struct RoundLayer {
    pub commit_index: u64,
    pub status: LayerStatus,
    /// (Address, StorageKey) → MVDS
    pub slot_versions: HashMap<(Address, StorageKey), SlotVersions>,
    /// Address → (TxIndex → AccountDiff)
    pub account_versions: HashMap<Address, BTreeMap<TxIndex, AccountDiff>>,
    /// HardCommit 이후 재실행 대상 TX 목록. discard 후에도 보존.
    pub pending_txs: Option<Vec<EthSignedTx>>,
}

pub struct ShadowDb<DB: DatabaseRef> {
    canonical: Mutex<DB>,
    layers: RwLock<BTreeMap<u64, RoundLayer>>,
    /// storage_ref 호출 시 READLAST에 사용할 현재 TxIndex.
    /// executor가 TX 실행 직전 set_current_tx()로 설정.
    current_tx: AtomicUsize,
}
```

---

### Step 3: DatabaseRef 구현 — READLAST

읽기 해소 순서: **layers 내림차순** (Speculative + PendingCommit 모두 포함) → canonical.

```
storage_ref(addr, slot):
  for layer in layers.values().rev():   // 최신 commit_index부터
    SlotVersions 탐색 (TxIndex < current_tx 중 최대):
      Data(v)  → v 반환 + readers[writer].push(current_tx)
      Pending  → 해당 SlotVersions에서 더 낮은 TxIndex의 Data로 폴백
                 + readers[writer].push(current_tx) 기록 (Pending 해소 시 통보 받음)
      Absent   → 이 TxIndex 건너뜀
    일치 버전 없음 → 다음 layer로
  모든 layer 없음 → canonical.storage_ref(addr, slot)
```

`basic_ref`: account_versions에서 동일 방식 탐색 → canonical 위임.
`code_by_hash_ref`, `block_hash_ref`: canonical 위임 (불변).

---

### Step 4: Dependency Notification API

```rust
/// TX 실행 완료 후 REVM EvmState → SlotVersions 기록.
/// is_changed() 슬롯 → Data, 읽기만 한 슬롯 → readers 갱신.
pub fn record_tx_execution(
    &self, commit_index: u64, tx_idx: TxIndex, evm_state: &EvmState,
)

/// TX 검증 실패. 롤백 파이프라인 진입점.
/// 반환 전 내부 상태 변화 (테스트에서 검증):
///   versions[tx_idx] = Pending
///   readers[tx_idx] drain → 반환값으로 전달
pub fn abort_tx(
    &self, commit_index: u64, tx_idx: TxIndex,
) -> Vec<TxIndex>   // 연쇄 재실행 필요 TX 목록

/// TX 재실행 완료. abort_tx의 역방향.
/// 반환 전 내부 상태 변화:
///   versions[tx_idx] = Data(new_value)
///   readers[tx_idx] 초기화 (새 독자 추적 재시작)
pub fn commit_tx_execution(
    &self, commit_index: u64, tx_idx: TxIndex, evm_state: &EvmState,
)

/// 라운드 내 전체 검증 — VALIDAFTER 불변 조건.
/// 각 TX의 readers가 현재도 올바른 버전을 읽고 있는지 확인.
/// 반환: 추가 재실행이 필요한 TX 목록.
pub fn validate_round(&self, commit_index: u64) -> Vec<TxIndex>
```

---

### Step 5: Cascade Read Invalidation

```rust
/// Round N 재실행 결과 일부 슬롯 값이 변경됐을 때 호출.
/// N 이후 Speculative 레이어 중 changed_slots를 읽은 레이어를 무효화.
/// PendingCommit 레이어는 건드리지 않음.
///
/// 무효화된 레이어 처리:
///   slot_versions    → 전부 제거 (재실행 시 새로 계산)
///   account_versions → 전부 제거
///   pending_txs      → 보존 (재실행 스케줄러가 사용)
///   status           → Speculative 유지
///
/// 반환: 무효화된 commit_index 목록.
pub fn cascade_invalidate(
    &self,
    base_commit_index: u64,
    changed_slots: &HashSet<(Address, StorageKey)>,
) -> Vec<u64>
```

---

### Step 6: Two-Phase Commit 파이프라인

```rust
pub struct CommitHandle {
    pub commit_index: u64,
    /// validate_round 결과 — 재실행 필요 TxIndex 목록.
    pub conflicts: Vec<TxIndex>,
    /// 재실행 대상 EthSignedTx (최종 순서).
    pub reexec_queue: Vec<EthSignedTx>,
}

/// Phase 1: 3Δ HardCommit 수신 시 호출.
///   레이어 status = PendingCommit
///   validate_round() 수행 → CommitHandle 반환
///   (executor가 CommitHandle.conflicts 기반으로 재실행)
pub fn stage_commit(
    &self, commit_index: u64, final_tx_order: Vec<TxIndex>,
) -> CommitHandle

/// Phase 2: 재실행 완료 후 호출.
///   canonical.commit(final_diff) 수행
///   commit_index 이하 레이어 전부 Drop
///   commit_index 순서 강제: N 미완료 시 N+1 호출 → Err 반환
pub fn finalize_commit(
    &self, commit_index: u64, final_diff: RoundDiff,
) -> Result<(), CommitError>
where DB: DatabaseRef + DatabaseCommit,
```

---

### Step 7: 테스트 시나리오 명세

테스트 환경: `EmptyDB` 위에 `ShadowDb` 생성. REVM 실제 실행 없이 `record_tx_execution`에 수동 EvmState 주입.

---

#### T1. `test_readlast_basic`

```
시나리오:
  commit_index=1, TX_2가 slot(A, K) = 100 기록
  current_tx = 5

검증:
  storage_ref(A, K) → 100 반환
  SlotVersions.readers[2] == [5]  // 의존성 기록됨
```

---

#### T2. `test_readlast_skip_to_latest`

```
시나리오:
  commit_index=1
  TX_2 기록: slot(A, K) = 100
  TX_5 기록: slot(A, K) = 200
  current_tx = 8

검증:
  storage_ref(A, K) → 200 반환  // TX_5가 최신 (TxIndex < 8 중 최대 = 5)
  SlotVersions.readers[5] == [8]
  SlotVersions.readers[2] == []   // TX_2 버전은 선택되지 않음
```

---

#### T3. `test_readlast_no_writer_falls_to_canonical`

```
시나리오:
  EmptyDB에 slot(A, K) = 999 사전 설정
  commit_index=1의 슬롯 기록 없음
  current_tx = 3

검증:
  storage_ref(A, K) → 999 반환  // canonical 폴백
```

---

#### T4. `test_pending_fallback_to_prior_data`

```
시나리오:
  commit_index=1
  TX_2 기록: slot(A, K) = 100  (Data)
  TX_5 abort_tx 호출 전 record → slot(A, K) = 200, 이후 abort_tx(1, 5)
  current_tx = 8

검증:
  abort_tx(1, 5) 호출 후:
    SlotVersions.versions[5] == Pending
  storage_ref(A, K) → 100 반환  // TX_5 Pending이므로 TX_2 Data로 폴백
  SlotVersions.readers[5] == [8]  // Pending 해소 시 8에 통보 예약
```

---

#### T5. `test_pending_fallback_to_canonical`

```
시나리오:
  EmptyDB에 slot(A, K) = 999
  commit_index=1에서 TX_3만 기록했고 abort_tx(1, 3) 호출
  current_tx = 7

검증:
  storage_ref(A, K) → 999 반환  // TX_3 Pending, 라운드 내 이전 Data 없음 → canonical
  SlotVersions.readers[3] == [7]
```

---

#### T6. `test_abort_tx_sets_pending_and_drains_readers`

```
시나리오:
  commit_index=1
  TX_3 record: slot(A, K) = 50
  current_tx=6 → storage_ref → readers[3]=[6] 기록
  current_tx=8 → storage_ref → readers[3]=[6,8] 기록

  abort_tx(1, 3) 호출

검증 (중간 상태):
  반환값 == [6, 8]                 // 재실행 파이프라인으로 전달할 목록
  SlotVersions.versions[3] == Pending
  SlotVersions.readers[3] == []    // drain됨
```

---

#### T7. `test_abort_tx_multiple_readers`

```
시나리오:
  commit_index=1, TX_2 record: slot(A,K)=10, slot(B,L)=20
  TX_4: storage_ref(A,K) → readers[(A,K)][2]=[4]
  TX_6: storage_ref(B,L) → readers[(B,L)][2]=[6]
  TX_7: storage_ref(A,K) → readers[(A,K)][2]=[4,7]

  abort_tx(1, 2) 호출

검증:
  반환값 == [4, 6, 7] (순서 무관, 중복 없음)
  versions[2] == Pending (slot(A,K), slot(B,L) 모두)
  모든 readers[2] drain됨
```

---

#### T8. `test_abort_tx_chain`

```
시나리오:
  commit_index=1
  TX_1 record: slot(A,K) = 10
  TX_3 record: slot(B,L) = 20
    TX_3 실행 중 storage_ref(A,K) → readers[(A,K)][1]=[3]
  TX_5 실행 중 storage_ref(B,L) → readers[(B,L)][3]=[5]

  abort_tx(1, 1) 호출

검증 (1단계):
  반환값_1 = [3]
  versions[(A,K)][1] == Pending

  abort_tx(1, 3) 호출 (반환값_1을 받아 연쇄 abort)

검증 (2단계):
  반환값_2 = [5]
  versions[(B,L)][3] == Pending

  // 최종: TX_5도 재실행 파이프라인에 진입함을 확인
  assert!(반환값_2.contains(&5))
```

---

#### T9. `test_commit_tx_restores_data_and_clears_readers`

```
시나리오:
  commit_index=1
  TX_3 record: slot(A,K)=100
  storage_ref(current_tx=6) → readers[3]=[6]
  abort_tx(1, 3) → Pending 상태, readers drain

  // TX_3 재실행
  commit_tx_execution(1, 3, new_state: slot(A,K)=150)

검증:
  versions[(A,K)][3] == Data(150)
  readers[(A,K)][3] == []         // 초기화됨

  // 이후 새 TX가 읽으면 새 의존성 추적 재시작
  storage_ref(current_tx=9)
  readers[(A,K)][3] == [9]
```

---

#### T10. `test_validate_round_detects_stale_reads`

```
시나리오:
  commit_index=1
  TX_2 record: slot(A,K)=100
  TX_4: storage_ref → 100 읽음, readers[2]=[4]
  // TX_2 재실행으로 값 변경
  abort_tx(1, 2) → Pending
  commit_tx_execution(1, 2, slot(A,K)=200)  // 값이 100 → 200으로 변경

  validate_round(1) 호출

검증:
  반환값.contains(&4)  // TX_4가 읽은 버전(100)이 현재(200)와 다름 → 재실행 필요
```

---

#### T11. `test_cascade_invalidate_clears_slot_versions`

```
시나리오:
  Round N (commit_index=1): TX_3 record slot(A,K)=100
  Round N+1 (commit_index=2):
    TX_2: storage_ref(A,K) → 100 읽음 (Round 1 레이어에서)

  // Round N 재실행으로 slot(A,K) 값 변경
  cascade_invalidate(base=1, changed={slot(A,K)})

검증:
  Round N+1 레이어:
    slot_versions 비어있음 (제거됨)
    account_versions 비어있음
  반환값 == [2]  // commit_index=2가 무효화됨
```

---

#### T12. `test_cascade_preserves_pending_txs`

```
시나리오:
  Round N+1 (commit_index=2):
    pending_txs = Some([tx_a, tx_b])
    slot_versions에 데이터 있음

  cascade_invalidate(base=1, changed={아무 슬롯})

검증:
  Round N+1.slot_versions == empty
  Round N+1.pending_txs == Some([tx_a, tx_b])  // 보존됨
```

---

#### T13. `test_pending_commit_not_cascade_invalidated`

```
시나리오:
  Round N (commit_index=1): status=PendingCommit
  Round N+1 (commit_index=2): status=Speculative
    둘 다 slot(A,K)를 기록함

  cascade_invalidate(base=0, changed={slot(A,K)})

검증:
  반환값 == [2]              // Speculative만 무효화
  Round N.slot_versions 유지  // PendingCommit 보호됨
  Round N+1.slot_versions 비어있음
```

---

#### T14. `test_pending_commit_visible_to_next_round`

```
시나리오:
  Round N (commit_index=1): status=PendingCommit
    TX_5 record: slot(A,K) = 777
  Round N+1 (commit_index=2): status=Speculative
    current_tx = 3

  Round N+1 컨텍스트에서 storage_ref(A,K) 호출

검증:
  반환값 == 777         // PendingCommit 레이어에서 읽힘
  canonical에는 없는 값
```

---

#### T15. `test_stage_commit_produces_correct_handle`

```
시나리오:
  commit_index=1
  TX_2 record: slot(A,K)=100
  TX_4: storage_ref → 100, readers[2]=[4]
  abort_tx(1, 2) → [4]
  commit_tx_execution(1, 2, slot(A,K)=200)
  // TX_4는 100을 읽었지만 실제는 200이므로 stale

  stage_commit(1, final_tx_order=[0,1,2,3,4]) 호출

검증:
  CommitHandle.commit_index == 1
  CommitHandle.conflicts.contains(&4)  // TX_4가 stale read로 재실행 필요
  레이어 status == PendingCommit
```

---

#### T16. `test_finalize_commit_writes_canonical`

```
시나리오:
  commit_index=1, stage_commit 완료 후
  final_diff: slot(A,K)=200, slot(B,L)=300 포함

  finalize_commit(1, final_diff) 호출

검증:
  canonical.storage_ref(A, K) == 200
  canonical.storage_ref(B, L) == 300
  layers에 commit_index=1 레이어 없음 (Drop됨)
```

---

#### T17. `test_finalize_ordering_enforced`

```
시나리오:
  commit_index=1 stage_commit 완료 (PendingCommit 상태)
  commit_index=2 stage_commit 완료 (PendingCommit 상태)

  finalize_commit(2, diff) 먼저 호출 시도 (순서 위반)

검증:
  반환값 == Err(CommitError::OutOfOrder)  // 차단됨
  commit_index=2 레이어 상태 유지됨

  finalize_commit(1, diff1) 호출 → Ok(())
  finalize_commit(2, diff2) 호출 → Ok(())  // 순서 맞으면 통과
```

---

## 완료 기준 (Done Criteria)

1. `cargo test -p shadow-state` 17개 테스트 모두 통과한다.
2. **READLAST**: TX_k는 자신보다 앞선 TX 중 최신 Data 버전을 읽는다. Pending 시 이전 Data로 폴백.
3. **Dependency Notification**: `abort_tx` 반환값에 해당 버전을 읽은 모든 TX가 포함된다 (체인 포함).
4. **중간 상태 검증**: `abort_tx` 직후 `Pending` 전환과 readers drain이 즉시 반영된다.
5. **Rollback 파이프라인**: T6~T10이 보여주듯, abort → 연쇄 통보 → validate_round → CommitHandle 흐름이 올바른 TX 목록을 전달한다.
6. **Cascade**: Round N 변경이 Round N+1 Speculative 레이어를 무효화하되 PendingCommit 레이어는 보호한다.
7. **Two-phase commit**: finalize_commit 후 canonical DB에 최종 diff가 반영되고 레이어가 제거된다.
8. **순서 강제**: commit_index 순서를 위반한 finalize_commit은 Err 반환으로 차단된다.

---

## 테스트 기준

```
cargo test -p shadow-state
```

- [ ] `test_readlast_basic`
- [ ] `test_readlast_skip_to_latest`
- [ ] `test_readlast_no_writer_falls_to_canonical`
- [ ] `test_pending_fallback_to_prior_data`
- [ ] `test_pending_fallback_to_canonical`
- [ ] `test_abort_tx_sets_pending_and_drains_readers`
- [ ] `test_abort_tx_multiple_readers`
- [ ] `test_abort_tx_chain`
- [ ] `test_commit_tx_restores_data_and_clears_readers`
- [ ] `test_validate_round_detects_stale_reads`
- [ ] `test_cascade_invalidate_clears_slot_versions`
- [ ] `test_cascade_preserves_pending_txs`
- [ ] `test_pending_commit_not_cascade_invalidated`
- [ ] `test_pending_commit_visible_to_next_round`
- [ ] `test_stage_commit_produces_correct_handle`
- [ ] `test_finalize_commit_writes_canonical`
- [ ] `test_finalize_ordering_enforced`
