# REVM 분석 문서

**분석 대상**: `extern/revm/` (revm v36.0.0)
**목적**: Shadow State 구현 진입점 파악 및 병렬 실행 통합 전략 도출

---

## 1. Crate 구조

```
revm/crates/
├── primitives/       핵심 타입 (Address, U256, B256, SpecId 등) — alloy_primitives 재export
├── bytecode/         바이트코드 파싱, OpCode, JumpTable
├── interpreter/      EVM 명령어 실행 엔진, 스택/메모리 관리, 가스 계량
├── precompile/       표준 프리컴파일 (ecrecover, sha256, BLS12-381 등)
├── context/          실행 컨텍스트 (BlockEnv, TxEnv, CfgEnv, Context, Evm 구조체)
├── handler/          트랜잭션 실행 핸들러, ExecuteEvm/ExecuteCommitEvm trait
├── inspector/        실행 추적/모니터링 (Tracer)
├── state/            계정/스토리지 상태 타입 (Account, EvmStorageSlot, AccountStatus)
├── database/         DB 구현체 + 상태 관리 계층 (CacheState, BundleState, State)
├── revm/             공개 API — 모든 crate 재export
└── op-revm/          Optimism 스택 전용 확장
```

**우리가 직접 연관된 crate**: `database`, `state`, `context`, `handler`, `revm`

---

## 2. Database Trait — Shadow State 구현 진입점

**위치**: `crates/database/interface/src/lib.rs`

### 2-1. Database trait (mutable)

```rust
pub trait Database {
    type Error: DBErrorMarker; // Send + Sync + 'static 필수

    fn basic(&mut self, address: Address)
        -> Result<Option<AccountInfo>, Self::Error>;

    fn code_by_hash(&mut self, code_hash: B256)
        -> Result<Bytecode, Self::Error>;

    fn storage(&mut self, address: Address, index: StorageKey)
        -> Result<StorageValue, Self::Error>;

    fn block_hash(&mut self, number: u64)
        -> Result<B256, Self::Error>;
}
```

| 메서드 | 역할 |
|--------|------|
| `basic` | 계정 정보 조회 (잔액, nonce, code_hash). 존재하지 않으면 None |
| `code_by_hash` | 바이트코드 해시로 컨트랙트 코드 조회 |
| `storage` | 특정 주소의 스토리지 슬롯 값 조회 |
| `block_hash` | 블록 번호로 블록 해시 조회 |

### 2-2. DatabaseRef trait (immutable — `&self`)

`Database`와 동일한 메서드이나 `&self`를 받음.
`auto_impl(&, &mut, Box, Rc, Arc)` — **Arc 지원**, 여러 스레드에서 공유 읽기 가능.

```rust
// Arc<ShadowDb>를 Database로 쓰려면 DatabaseRef를 구현 후 WrapDatabaseRef로 감싸면 됨
let db = WrapDatabaseRef(Arc::new(shadow_db));
```

### 2-3. DatabaseCommit trait

```rust
pub trait DatabaseCommit {
    fn commit(&mut self, changes: AddressMap<Account>);
}
```

실행 후 변경사항을 DB에 반영할 때 사용. 우리의 `commit_round()` 로직과 대응됨.

---

## 3. Evm Builder 패턴 및 트랜잭션 실행 흐름

### 3-1. 구성 방법

```rust
// 1. Context 생성 (DB + 블록/트랜잭션 환경 포함)
let ctx = Context::new(my_database, SpecId::PRAGUE)
    .with_block_env(block_env)
    .with_tx_env(tx_env);

// 2. Evm 빌드
let mut evm = ctx.build_mainnet();

// 3. 트랜잭션 실행
let result = evm.transact(tx)?;
// result.result  → ExecutionResult
// result.state   → EvmState (변경된 계정/스토리지 맵)
```

### 3-2. 주요 실행 메서드

| 메서드 | 동작 |
|--------|------|
| `transact_one(tx)` | 실행만 수행, state 미확정 (journal에 누적) |
| `finalize()` | journal → EvmState 추출 (journal 초기화) |
| `transact(tx)` | transact_one + finalize 한번에 |
| `transact_many(txs)` | 여러 tx 순차 실행, state는 누적 |
| `transact_many_finalize(txs)` | 여러 tx 실행 + 최종 state 추출 |

### 3-3. 실행 결과 타입

```rust
pub enum ExecutionResult {
    Success { reason, gas: ResultGas, logs: Vec<Log>, output: Output },
    Revert  { gas: ResultGas, logs: Vec<Log>, output: Bytes },
    Halt    { reason: HaltReason, gas: ResultGas, logs: Vec<Log> },
}

pub struct ExecResultAndState<R, S = EvmState> {
    pub result: R,   // ExecutionResult 또는 Vec<ExecutionResult>
    pub state: S,    // AddressMap<Account> — 변경된 상태 전체
}
```

---

## 4. 스레드 안전성 및 병렬 실행 가능성

| 항목 | 현황 |
|------|------|
| `Database` trait | Send/Sync 요구 없음. Error 타입만 `Send+Sync+'static` 필수 |
| `DatabaseRef` trait | Arc 지원 (auto_impl). 공유 읽기 가능 |
| `Evm` 구조체 | 명시적 Send/Sync 없음. 구성 타입이 Send/Sync면 자동 충족 |
| **병렬 실행** | **기본 미지원**. 내부 Journal이 단일 스레드 설계 |

**결론**: REVM 자체는 병렬 실행을 제공하지 않는다. 우리가 Shadow State로 격리 레이어를 만든 뒤, 각 트랜잭션마다 독립된 Evm 인스턴스를 스레드에 할당하는 방식으로 병렬화해야 한다.

---

## 5. 상태 관리 계층 — BundleState와의 관계

REVM의 내장 상태 계층:

```
Database (원장 DB, 영구 저장)
  └─ BundleState (멀티블록 상태, 블록 단위 revert 지원)
       └─ CacheState (현재 블록 캐시)
            └─ Journal (현재 트랜잭션 임시 상태)
```

**BundleState**는 각 슬롯의 `(original_value, present_value)` 쌍을 추적하며 블록 단위 revert를 지원한다. 이는 우리의 Shadow Memory 개념과 유사하나, 우리는 이보다 더 세밀한 **트랜잭션 단위 격리 + R/W 충돌 추적**이 필요하다.

---

## 6. Shadow State 구현 전략 (Phase 1 확정 사항 반영)

Phase 1 인터페이스 설계에서 아래 사항이 확정됐다.

| 결정 | 내용 |
|------|------|
| D2: DatabaseRef | `&self` 기반 `DatabaseRef` + `WrapDatabaseRef` 채택. Arc 공유 가능 |
| D5: 충돌 단위 | Storage slot 단위 `(Address, StorageKey)` |
| 재실행 전략 | MVCC + Dependency Notification (§8 참조) |

---

## 7. EvmState에서 R/W Set 추출 방법

REVM `transact()` 실행 후 반환되는 `ExecResultAndState.state`(`EvmState = AddressMap<Account>`)에서 슬롯별 R/W 여부를 직접 추출할 수 있다.

```rust
// 실행 후 EvmState에서 R/W set 추출
fn extract_rw_sets(
    state: &EvmState,
) -> (ReadSet, WriteSet) {
    let mut reads  = HashSet::new();
    let mut writes = HashMap::new();

    for (addr, account) in state.iter() {
        for (slot_key, slot) in account.storage.iter() {
            // 슬롯이 로드됐으면 = 읽음
            reads.insert((*addr, *slot_key));

            // original != present 이면 = 씀 (EvmStorageSlot::is_changed())
            if slot.is_changed() {
                writes.insert((*addr, *slot_key), slot.present_value);
            }
        }
    }
    (reads, writes)
}
```

**핵심**: `EvmStorageSlot.is_changed()` = `original_value != present_value`. REVM이 이미 슬롯 단위 변경 여부를 추적하므로 별도 추적 없이 실행 후 추출 가능하다.

---

## 8. 병렬 실행 전략 — BlockSTM 분석 및 우리 접근

### 8-1. BlockSTM 핵심 알고리즘

**출처**: "Block-STM: Scaling Blockchain Execution by Turning Ordering Curse to a Performance Blessing" (Aptos Labs, PPoPP '23, arXiv:2203.06871)

두 불변 조건:
- **READLAST(k)**: TX_k는 자신보다 앞선 TX 중 가장 높은 인덱스의 버전을 읽는다
- **VALIDAFTER(j,k)**: TX_j 재실행 시마다 TX_k의 read-set 검증 수행

**MVDS (Multi-Version Data Structure)**:

```
슬롯 X:
  BTreeMap<TxIndex, VersionedValue>
    ├── 2 → Data(100)     ← TX_2가 씀
    ├── 5 → ESTIMATE      ← TX_5 abort, 재실행 중
    └── 8 → Data(200)     ← TX_8이 씀

TX_6이 슬롯 X 읽기:
  → TxIndex < 6 중 최대 = 5 (ESTIMATE)
  → ESTIMATE 읽음 → TX_6 즉시 early abort
```

**ESTIMATE 메커니즘**:
- TX_j abort 시 write-set을 삭제하지 않고 `ESTIMATE` 마커로 교체
- 후속 TX가 ESTIMATE를 읽으면 즉시 early abort → 캐스케이딩 abort 방지
- TX_j 재실행 완료 시 ESTIMATE → Data 교체

### 8-2. 특허 / IP 현황

| 항목 | 내용 |
|------|------|
| Aptos Labs 등록 특허 | 공개 DB에서 BlockSTM 관련 등록 특허 미확인 |
| 선행 기술(prior art) | 2022년 3월 arXiv 공개 — 이후 특허 출원 어려움 |
| 알고리즘 특허 | Alice Corp. 판결 이후 미국에서 순수 알고리즘 특허 등록 난도 높음 |
| aptos-core 코드 | 현행 라이선스 제약 있음 (4년 후 Apache 2.0 전환). **코드 복사 금지, 독자 구현은 무방** |
| Starknet | BlockSTM을 Rust로 직접 채택 |

**결론**: 독자 구현은 법적 위험 낮음. aptos-core 코드 직접 사용은 금지.

### 8-3. 타 프로젝트 비교

| 프로젝트 | 방식 | ESTIMATE 유사 메커니즘 |
|---------|------|----------------------|
| Aptos | OCC + ESTIMATE 마커 | ESTIMATE |
| Starknet | BlockSTM Rust 직접 채택 | ESTIMATE |
| Polygon | BlockSTM 변형 + 블록에 DAG 메타데이터 기록 | 변형 |
| Monad | "최대 2회 실행" 보장 + serial merge 단계 | 없음 |
| Sei | heuristic 사전 예측 + Bloom filter | 없음 |
| NEMO (2024) | 정상 실행 중 proactive 의존성 추출 | 없음 (직접 통보) |

### 8-4. 우리 접근 — Dependency Notification MVCC

ESTIMATE 없이 동일한 목표를 달성한다. NEMO (2024) 논문의 접근과 유사.

**핵심 차이**:

```
BlockSTM:
  TX_j abort → ESTIMATE 마커 배치
  TX_k가 ESTIMATE 읽으면 → TX_k 스스로 early abort

우리 (Dependency Notification):
  TX_k가 TX_j 버전을 읽을 때 → dep_list[j].add(k) 기록
  TX_j abort 시 → dep_list[j]의 모든 TX에 직접 재실행 통보
  TX_j 재실행 완료 → dep_list[j] 초기화
```

**MVDS 자료구조**:

```rust
enum VersionedValue {
    Data(StorageValue),  // 확정된 값
    Pending,             // TX 재실행 중 (ESTIMATE 대신 — blocking 없음)
    Absent,              // 슬롯 미존재
}

struct SlotVersions {
    // TxIndex → 해당 TX가 쓴 값
    versions: BTreeMap<TxIndex, VersionedValue>,
    // writer TX → 이 버전을 읽은 reader TX들 (재실행 알림용)
    readers: HashMap<TxIndex, Vec<TxIndex>>,
}
```

**읽기 흐름 (READLAST 구현)**:

```
TX_k가 슬롯 X 읽기:
  1. versions에서 TxIndex < k인 최대 항목 탐색
  2. Data(v)   → v 반환, readers[j].add(k) 기록
  3. Pending   → 가장 가까운 이전 Data 버전으로 폴백
               → readers[j].add(k) 기록 (Pending 확정 시 재실행 통보 받음)
  4. 없음      → 상위 PendingCommit 레이어 또는 canonical 탐색
```

**abort 흐름**:

```
TX_j 검증 실패(abort):
  versions[j] = Pending
  dep_list = readers[j].drain()
  dep_list의 모든 TX → 재실행 큐에 추가
  TX_j 재실행 시작

TX_j 재실행 완료:
  versions[j] = Data(new_value)
  readers[j] 초기화 (새 독자 추적 재시작)
```

**BlockSTM 대비 장점**:
- ESTIMATE early-abort 복잡도 제거
- NEMO 측정 기준 재실행 횟수 약 절반 (고경합 워크로드)
- 구현 단순성 ↑, 특허 쟁점 요소 ↓

### 8-5. 라운드 간 계층 구조와 MVCC의 결합

```
Round N+1 (Speculative)
  SlotVersions { TX_0..TX_m, readers }
  ↑ Pending 폴백 시 상위 계층 탐색
Round N (PendingCommit)       ← finalize 전까지 보존
  SlotVersions { TX_0..TX_n, readers }
  ↑ 없으면
Canonical DB
```

- 라운드 간 계층: `BTreeMap<commit_index, RoundLayer>` (§9 Phase 2 설계)
- 라운드 내 TX 간 버전: `SlotVersions` MVDS
- 두 계층이 독립적으로 동작하며 읽기 시 순서대로 탐색

---

## 9. 체크리스트 대응

| phase-0 항목 | 결과 |
|--------------|------|
| `extern/revm` crate 구조 파악 | ✅ 14개 crate 역할 파악 |
| `Database` trait 인터페이스 파악 | ✅ 4개 메서드, Error 타입 조건 파악 |
| `Evm` builder 패턴 및 실행 흐름 | ✅ Context → build_mainnet → transact 흐름 파악 |
| 병렬 실행을 위한 상태 격리 방법 | ✅ 기본 미지원, 인스턴스 분리 전략 도출 |
