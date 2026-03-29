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

## 6. Shadow State 구현 전략 (Phase 2 입력)

### 구현할 것

```
ShadowDb
  implements: Database (+ DatabaseRef for Arc sharing)

내부 구조:
  - main_db: Arc<dyn DatabaseRef>       ← 원장 DB (읽기 전용)
  - versions: BTreeMap<TxId, TxState>   ← 트랜잭션별 쓰기 세트
  - read_set: HashMap<TxId, ReadSet>    ← R/W 충돌 감지용 읽기 추적
  - write_set: HashMap<TxId, WriteSet>  ← 쓰기 추적
```

### 병렬 실행 패턴

```
round_txs = [tx0, tx1, tx2, ...]

병렬:
  thread_0: Evm::new(ShadowDb::for_tx(0, ..)).transact(tx0)
  thread_1: Evm::new(ShadowDb::for_tx(1, ..)).transact(tx1)
  thread_2: Evm::new(ShadowDb::for_tx(2, ..)).transact(tx2)

충돌 감지:
  ShadowDb 전체 R/W 세트 비교
  → 충돌 tx 재실행 (의존성 순서대로 직렬 실행)

HardCommit 시:
  충돌 없으면: 모든 write_set → main_db 병합
  충돌 있으면: round 전체 shadow 폐기 (Drop)
```

### 핵심 결정 사항 (Phase 1에서 확정)

1. `ShadowDb`는 `Database` 구현 (`&mut self`) vs `DatabaseRef` 구현 (`&self`) 중 어느 것인가?
   - 병렬 읽기를 위해 `DatabaseRef` + `WrapDatabaseRef`가 더 적합할 수 있음
2. 충돌 감지 단위: 슬롯 단위 vs 계정 단위
3. 재실행 전략: 충돌 tx만 직렬 재실행 vs 라운드 전체 재실행

---

## 7. 체크리스트 대응

| phase-0 항목 | 결과 |
|--------------|------|
| `extern/revm` crate 구조 파악 | ✅ 14개 crate 역할 파악 |
| `Database` trait 인터페이스 파악 | ✅ 4개 메서드, Error 타입 조건 파악 |
| `Evm` builder 패턴 및 실행 흐름 | ✅ Context → build_mainnet → transact 흐름 파악 |
| 병렬 실행을 위한 상태 격리 방법 | ✅ 기본 미지원, 인스턴스 분리 전략 도출 |
