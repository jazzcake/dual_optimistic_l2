# Phase 2: Shadow State 구현

**상태**: 🔄 진행중
**목표**: REVM의 `Database` trait을 구현하는 Multi-Version Shadow Memory를 완성한다. 합의/스케줄러 없이 독립적으로 테스트 가능해야 한다.

---

## 작업 목록

### Step 1: shared 타입 교체
- [ ] `shared` crate에 revm-primitives/revm-state 의존성 추가
- [ ] placeholder 타입을 revm 실제 타입으로 교체
- [ ] `cargo check --workspace` 통과

### Step 2: ShadowDb 핵심 구조 설계 (MVCC)
- [ ] `LayerStatus` enum (`Speculative` / `PendingCommit`)
- [ ] `VersionedValue` enum (`Data` / `Pending` / `Absent`)
- [ ] `SlotVersions` 구조체 (versions BTreeMap + readers 의존성 목록)
- [ ] `RoundLayer` 구조체 (slot_versions MVDS + account_versions + pending_txs)
- [ ] `ShadowDb<DB>` 제네릭 구조체 (layers + canonical Mutex + current_tx AtomicUsize)

### Step 3: DatabaseRef 구현 (READLAST)
- [ ] `storage_ref` — READLAST: TxIndex < current_tx 최대 버전, Pending 폴백, readers 기록
- [ ] `basic_ref` — account_versions 동일 탐색 → canonical 위임
- [ ] `code_by_hash_ref` / `block_hash_ref` — canonical 위임

### Step 4: Dependency Notification 기반 TX 실행 관리
- [ ] `record_tx_execution` — REVM EvmState → SlotVersions 기록
- [ ] `abort_tx` — Pending 전환 + readers drain → 연쇄 재실행 목록 반환
- [ ] `commit_tx_execution` — 재실행 완료, Data 확정 + readers 초기화
- [ ] `validate_round` — VALIDAFTER 불변 조건 검증

### Step 5: Cascade Read Invalidation
- [ ] `cascade_invalidate` — 변경 슬롯 기반 연쇄 무효화
- [ ] PendingCommit 레이어 cascade 제외 처리
- [ ] 무효화 시 account_diffs/tx_traces 제거, pending_txs 보존

### Step 6: Two-Phase Commit 파이프라인
- [ ] `stage_commit` — HardCommit 수신, 레이어 PendingCommit 전환, CommitHandle 반환
- [ ] `finalize_commit` — 재실행 완료 후 canonical write, 레이어 Drop
- [ ] commit_index 순서 강제 (`Mutex<DB>` 직렬화)
- [ ] PendingCommit slot 가시성 — 이후 Speculative 라운드 읽기 기준 포함

### Step 7: 테스트
- [ ] `test_rw_conflict_detection`
- [ ] `test_ww_conflict_detection`
- [ ] `test_cascade_invalidation`
- [ ] `test_pending_commit_visible_to_reads`
- [ ] `test_pending_not_cascade_invalidated`
- [ ] `test_stage_then_finalize_commit`
- [ ] `test_commit_applies_diff`
- [ ] `test_discard_no_side_effects`
- [ ] `test_multi_round_isolation`
- [ ] `test_finalize_ordering_enforced`

---

## 실행 계획 (Execution Plan)

**작업 순서 원칙**: 타입 기반 → 구조 설계 → DatabaseRef 구현 → 충돌 감지 → Commit 파이프라인 → 테스트.

---

### Step 1: `shared` 타입을 revm-primitives로 교체

Phase 1에서 임시로 정의한 `Address([u8;20])`, `U256([u64;4])` 등 placeholder를
`revm-primitives` 실제 타입으로 교체한다.

- `shared/Cargo.toml` → `revm-primitives`, `revm-state` path 의존성 추가
- `shared/src/lib.rs` → `Address`, `U256`, `B256`, `AccountInfo`, `StorageKey`, `StorageValue`를
  revm 타입으로 교체. EVM에 없는 자체 타입(`EthSignedTx`, `BlockRef` 등)만 유지.
- `cargo check --workspace` 통과 확인

---

### Step 2: `ShadowDb` 핵심 구조 설계

설계 근거: `docs/revm-analysis.md` §8 (BlockSTM 분석 및 Dependency Notification MVCC)

#### 2-1. LayerStatus — 라운드 레이어 생명주기

```rust
enum LayerStatus {
    /// 2Δ SoftCommit 기반. 충돌 시 discard 가능.
    Speculative,
    /// 3Δ HardCommit 수신. 재실행 진행 중.
    /// 이후 라운드의 읽기 기준이 되므로 slot 데이터 보존 필수.
    /// cascade_invalidate 대상에서 제외.
    PendingCommit,
}
```

#### 2-2. MVDS — 라운드 내 TX 간 Multi-Version Data Structure

BlockSTM의 ESTIMATE 대신 **Dependency Notification** 방식 채택.

```rust
/// Per-slot versioned value within one round's execution.
enum VersionedValue {
    Data(StorageValue),  // TX가 쓴 확정 값
    Pending,             // TX 재실행 중 (ESTIMATE 없이 이전 버전으로 폴백)
    Absent,              // 슬롯 미존재
}

/// Per-slot MVDS: TX 인덱스별 버전 + 의존성 목록
struct SlotVersions {
    /// TxIndex → 해당 TX가 쓴 값 (READLAST 구현)
    versions: BTreeMap<TxIndex, VersionedValue>,
    /// writer_tx → 이 버전을 읽은 reader TX들 (abort 시 직접 재실행 통보)
    readers: HashMap<TxIndex, Vec<TxIndex>>,
}

pub type TxIndex = usize;
```

#### 2-3. RoundLayer 구조

```rust
struct RoundLayer {
    commit_index: u64,
    status: LayerStatus,
    /// (Address, StorageKey) → 슬롯별 MVDS
    slot_versions: HashMap<(Address, StorageKey), SlotVersions>,
    /// TX별 계정 정보 변경 (balance, nonce, code)
    account_versions: HashMap<Address, BTreeMap<TxIndex, AccountDiff>>,
    /// discard 후에도 보존 — 재실행 스케줄러가 사용
    pending_txs: Option<Vec<EthSignedTx>>,
}
```

#### 2-4. ShadowDb 구조

```rust
pub struct ShadowDb<DB: DatabaseRef> {
    canonical: Mutex<DB>,                           // canonical write 직렬화
    layers: RwLock<BTreeMap<u64, RoundLayer>>,      // commit_index 오름차순
    /// 현재 실행 중인 TX 인덱스 (DatabaseRef 호출 시 READLAST에 사용)
    current_tx: AtomicUsize,
}
```

읽기 해소 순서: layers 내림차순 (Speculative + PendingCommit 모두) → canonical.
PendingCommit 레이어는 "확정된 미래 canonical"이므로 읽기 기준에 포함.

---

### Step 3: `DatabaseRef` 구현 — READLAST 포함

```rust
impl<DB: DatabaseRef> DatabaseRef for ShadowDb<DB> {
    fn storage_ref(&self, address: Address, index: StorageKey)
    // layers 내림차순 탐색:
    //   각 레이어의 SlotVersions에서 TxIndex < current_tx인 최대 버전 탐색
    //   → Data(v)  : v 반환 + readers[j].add(current_tx) 기록
    //   → Pending  : 이전 Data 버전으로 폴백, readers[j].add(current_tx) 기록
    //   → 없음     : 다음 레이어로
    // 모든 레이어에 없으면 canonical.storage_ref 위임

    fn basic_ref(&self, address: Address)
    // account_versions에서 동일 방식 탐색 → canonical 위임

    fn code_by_hash_ref / fn block_hash_ref
    // canonical 위임 (불변 데이터)
}
```

---

### Step 4: Dependency Notification 기반 TX 실행 기록 및 재실행

REVM 실행 후 `EvmStorageSlot.is_changed()`로 R/W set 추출 (`docs/revm-analysis.md` §7).

```rust
/// TX 실행 완료 후 REVM EvmState를 SlotVersions에 기록.
pub fn record_tx_execution(
    &self, commit_index: u64, tx_idx: TxIndex, evm_state: &EvmState,
)

/// TX 검증 실패 시 호출.
/// 1. versions[tx_idx] = Pending
/// 2. readers[tx_idx] drain → 반환 (연쇄 재실행 대상)
/// 3. TX 재실행 준비 (호출자가 스케줄링)
pub fn abort_tx(
    &self, commit_index: u64, tx_idx: TxIndex,
) -> Vec<TxIndex>

/// TX 재실행 완료 후 호출.
/// versions[tx_idx] = Data(new_value), readers 초기화.
pub fn commit_tx_execution(
    &self, commit_index: u64, tx_idx: TxIndex, evm_state: &EvmState,
)

/// 라운드 내 전체 검증 — VALIDAFTER 불변 조건 구현.
/// 반환: 추가 재실행 필요 TX 목록.
pub fn validate_round(&self, commit_index: u64) -> Vec<TxIndex>
```

---

### Step 5: Cascade Read Invalidation (연쇄 무효화)

Round N 충돌 해소 재실행 결과가 투기적 값과 다를 때,
N 이후 **Speculative** 레이어 중 변경된 슬롯을 읽은 것들을 무효화한다.

```rust
/// Round N 재실행 후 변경된 슬롯을 받아,
/// N 이후 Speculative 레이어 중 해당 슬롯을 읽은 것을 무효화.
/// PendingCommit 레이어는 대상에서 제외 (commit 보장).
/// 반환: 무효화된 commit_index 목록 (재실행 대기열 진입).
pub fn cascade_invalidate(
    &self,
    base_commit_index: u64,
    changed_slots: &HashSet<(Address, StorageKey)>,
) -> Vec<u64>
```

무효화된 레이어 처리:
- `account_diffs` 제거 (재실행 시 새로 계산됨)
- `tx_traces` 제거
- `pending_txs` **보존** (재실행 스케줄러가 이 목록을 사용)
- `status` = Speculative 유지 (아직 HardCommit 안 받은 상태)

---

### Step 6: Two-Phase Commit 파이프라인

3Δ HardCommit 수신 → canonical write 사이의 "pending" 구간을 명시적으로 관리한다.

```rust
/// Phase 1: HardCommit 수신 시 호출.
/// 레이어를 PendingCommit으로 전환.
/// 충돌 TX 목록과 최종 TX 순서를 반환 (executor가 재실행에 사용).
pub fn stage_commit(
    &self,
    commit_index: u64,
    final_tx_order: Vec<usize>,
) -> CommitHandle

pub struct CommitHandle {
    pub commit_index: u64,
    pub conflicts: Vec<usize>,        // 재실행 필요 TX 인덱스
    pub reexec_queue: Vec<EthSignedTx>, // 재실행 대상 TX (순서 포함)
}

/// Phase 2: 재실행 완료 후 호출.
/// canonical에 최종 diff를 쓰고 레이어를 제거.
/// commit_index 이하의 PendingCommit 레이어도 함께 정리.
pub fn finalize_commit(
    &self,
    commit_index: u64,
    final_diff: RoundDiff,
) -> Result<(), DB::Error>
where
    DB: DatabaseRef + DatabaseCommit,
```

**Pending 구간 중 슬롯 가시성 정책**:
- PendingCommit 레이어의 slot 데이터는 이후 Speculative 라운드에게 읽기 기준으로 노출됨
- canonical write 전까지 레이어 데이터 보존 필수
- `finalize_commit` 완료 후에만 레이어 Drop

**직렬화 보장**:
- `finalize_commit`은 commit_index 순서대로 호출되어야 함 (N 완료 전 N+1 finalize 불가)
- `canonical`을 `Mutex<DB>`로 감싸 write 직렬화

---

### Step 7: 테스트 작성 및 `cargo test -p shadow-state` 통과

| 테스트 | 검증 내용 |
|--------|----------|
| `test_rw_conflict_detection` | A 쓴 슬롯을 B가 읽으면 충돌 감지 |
| `test_ww_conflict_detection` | A·B가 같은 슬롯에 쓰면 충돌 감지 |
| `test_cascade_invalidation` | Round N 재실행 결과 변경 시 N+1 레이어 무효화 |
| `test_pending_commit_visible_to_reads` | PendingCommit 레이어 slot이 N+1 읽기에 노출됨 |
| `test_pending_not_cascade_invalidated` | PendingCommit 레이어는 cascade 대상 제외 |
| `test_stage_then_finalize_commit` | stage→재실행→finalize 전체 흐름 통과 |
| `test_commit_applies_diff` | finalize 후 canonical에 최종 diff 반영 확인 |
| `test_discard_no_side_effects` | discard 후 canonical 불변 + pending_txs 보존 |
| `test_multi_round_isolation` | 라운드 간 speculative 격리 확인 |
| `test_finalize_ordering_enforced` | N 미완료 시 N+1 finalize 차단 |

---

---

## 완료 기준 (Done Criteria)

1. `cargo test -p shadow-state` 13개 테스트 모두 통과한다.
2. READLAST: TX_k는 자신보다 앞선 TX 중 최신 버전을 올바르게 읽는다.
3. Dependency Notification: TX_j abort 시 TX_j 버전을 읽은 TX들에 직접 재실행 통보된다.
4. 충돌 감지: 동일 슬롯에 접근하는 두 TX가 RW/WR/WW 유형별로 올바르게 식별된다.
3. Cascade 무효화: Round N 재실행 값 변경 시 N+1 Speculative 레이어가 무효화되고, pending_txs는 보존된다.
4. Pending 가시성: PendingCommit 레이어의 slot이 이후 라운드 읽기 기준에 포함된다.
5. Two-phase commit: `stage_commit` → 재실행 → `finalize_commit` 순서로 canonical write가 이루어진다.
6. 직렬화 보장: commit_index N이 finalize되기 전에 N+1 finalize가 차단된다.

---

## 테스트 기준

```
cargo test -p shadow-state
```

- [ ] `test_readlast_within_round` — TX_k가 TX_j(j<k)의 최신 버전을 읽음
- [ ] `test_pending_fallback` — TX_j Pending 시 이전 Data 버전으로 폴백
- [ ] `test_dependency_notification` — TX_j abort 시 readers에 직접 재실행 통보
- [ ] `test_rw_conflict_detection` — RW/WR 충돌 정상 감지
- [ ] `test_ww_conflict_detection` — WW 충돌 정상 감지
- [ ] `test_cascade_invalidation` — N 변경 시 N+1 무효화, pending_txs 보존
- [ ] `test_pending_commit_visible_to_reads` — PendingCommit slot이 읽기에 노출
- [ ] `test_pending_not_cascade_invalidated` — PendingCommit은 cascade 제외
- [ ] `test_stage_then_finalize_commit` — two-phase commit 전체 흐름
- [ ] `test_commit_applies_diff` — finalize 후 canonical 반영 확인
- [ ] `test_discard_no_side_effects` — discard 후 canonical 불변 + pending_txs 보존
- [ ] `test_multi_round_isolation` — 라운드 간 speculative 격리
- [ ] `test_finalize_ordering_enforced` — 순서 위반 finalize 차단
