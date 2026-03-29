# Phase 1: 인터페이스 설계

**상태**: 🔄 진행중
**목표**: 코드를 작성하기 전에 모든 컴포넌트 간의 경계면(API/타입)을 문서로 확정한다. 구현은 하지 않는다.

---

## 작업 목록

### 핵심 타입 정의
- [ ] `ConsensusEvent` 열거형 설계 (`SoftCommit { round, block }`, `HardCommit { round, block }`)
- [ ] `TxBatch` 타입 설계 (스케줄러 → 실행 엔진 전달 단위)
- [ ] `StateDiff` 타입 설계 (Shadow Memory → 메인 원장 머지 단위)
- [ ] `ExecutionResult` 타입 설계 (성공/실패, 가스 사용량 등)

### 컴포넌트 인터페이스 설계
- [ ] `ConsensusModule` trait (합의 모듈이 외부에 노출하는 인터페이스)
- [ ] `PipelineScheduler` trait (스케줄러 인터페이스)
- [ ] `ParallelExecutor` trait (병렬 실행 엔진 인터페이스)
- [ ] `ShadowDatabase` trait (REVM `Database` 확장 인터페이스)
- [ ] `CommitWrapper` trait (확정/롤백 래퍼 인터페이스)

### 비동기 채널 설계
- [ ] 합의 → 스케줄러 채널 타입 결정 (tokio mpsc / broadcast)
- [ ] 스케줄러 → 실행 엔진 채널 타입 결정
- [ ] 실행 엔진 → 래퍼 채널 타입 결정
- [ ] Backpressure 신호 채널 방향 및 타입 결정

### 테스트 하네스 인터페이스 설계
- [ ] `SimulatedNetwork` trait 설계 — 노드 간 메시지 전달 추상화 (지연 주입 포함)
- [ ] `SimulatedNode` trait 설계 — in-process 노드 추상화
- [ ] 결정론적 시뮬레이터용 인터페이스 확정 (Phase 0 전략 문서 기반)
- [ ] 벤치마크용 멀티스레드 환경 인터페이스 확정 (Δ 실제 측정 방법 포함)

### Docker 배포 가능성 설계
- [ ] 노드 설정을 환경변수 / 설정 파일로 외부화 (하드코딩 금지)
- [ ] 노드 간 통신 포트 및 프로토콜 명세 (Docker 네트워크 기준)
- [ ] 헬스체크 엔드포인트 설계 (합의 진행 상태 외부 조회용)

### 문서화
- [ ] `docs/interfaces.md` — 모든 trait 및 타입 명세 작성 (테스트 하네스 포함)
- [ ] 컴포넌트 상호작용 다이어그램 (ASCII)

---

## 실행 계획 (Execution Plan)

**작업 순서 원칙**: 결정 → 명세 → stub 선언 → 검증. 코드 구현은 없음.

---

### Step 1: 아키텍처 결정 사항 확정

Phase 0 분석에서 Phase 1에서 결정하기로 미룬 항목들을 확정한다.
결정 내용은 `docs/interfaces.md` 서두에 "결정 근거" 섹션으로 기록한다.

| # | 결정 사항 | 후보 | 근거 |
|---|---------|------|------|
| D1 | SoftCommit 감지 위치 | (A) `crates/consensus` 내부 hook vs (B) `crates/scheduler`가 DAG 관찰 | 합의 내부 상태에 직접 접근해야 하므로 A가 유리 |
| D2 | ShadowDb 구현 방식 | (A) `Database` (`&mut self`) vs (B) `DatabaseRef` (`&self`) + `WrapDatabaseRef` | 병렬 Arc 공유 필요 → B 유력 |
| D3 | Epoch 처리 | (A) MVP: epoch=0 고정 vs (B) 완전 지원 | MVP 단순화 → A 유력 |
| D4 | Quorum 방식 | (A) Node-based (1 node = 1 vote) vs (B) Stake-weighted | MVP 단순화 → A 유력 |
| D5 | Conflict 감지 단위 | (A) 슬롯(storage slot) 단위 vs (B) 계정 단위 | 정밀도 vs 오버헤드 트레이드오프 |

결정 산출물: `docs/interfaces.md` §1 "아키텍처 결정 기록"

---

### Step 2: 핵심 타입 명세

모든 crate 경계를 가로지르는 공유 타입을 Rust 수준으로 명세한다.
구현 없이 타입 정의만. `crates/shared/` 또는 `crates/consensus/`에 위치 결정.

#### 2-1. ConsensusEvent

```rust
// consensus → scheduler 채널로 전달되는 이벤트
pub enum ConsensusEvent {
    SoftCommit {
        round: Round,
        leader: BlockRef,
        txs: Vec<Transaction>,    // SubDAG 미확정, 예측 TX 목록
    },
    HardCommit {
        subdag: OurCommittedSubDag,  // CommittedSubDag에서 SUI 전용 필드 제거
    },
}
```

#### 2-2. OurCommittedSubDag (CommittedSubDag 슬림화)

```rust
// SUI CommittedSubDag에서 우리에게 필요한 필드만 추출
pub struct OurCommittedSubDag {
    pub leader: BlockRef,
    pub blocks: Vec<OurVerifiedBlock>,  // VerifiedBlock 래퍼 또는 그대로
    pub timestamp_ms: u64,
    pub commit_index: u64,
}
```

#### 2-3. TxBatch

```rust
// scheduler → executor 전달 단위
pub struct TxBatch {
    pub round: Round,
    pub commit_index: u64,
    pub txs: Vec<EthSignedTx>,       // EIP-2718 인코딩 Ethereum TX
    pub is_optimistic: bool,          // true=SoftCommit 기반, false=HardCommit 확정
}
```

#### 2-4. StateDiff

```rust
// executor → shadow state 커밋/폐기 단위
pub struct StateDiff {
    pub round: Round,
    pub changes: AddressMap<Account>,  // revm::EvmState와 동일 타입 재사용
}
```

#### 2-5. ExecutionResult

```rust
pub struct RoundExecutionResult {
    pub round: Round,
    pub results: Vec<TxExecutionResult>,
    pub state_diff: StateDiff,
    pub conflict_txs: Vec<TxIndex>,  // R/W 충돌 감지된 TX 인덱스
}
```

결정 산출물: `docs/interfaces.md` §2 "핵심 타입"

---

### Step 3: Trait 인터페이스 명세

각 crate의 공개 인터페이스를 Rust trait 수준으로 명세한다.

#### 3-1. ConsensusHandle (`crates/consensus`)

```rust
// 합의 모듈의 외부 인터페이스
pub trait ConsensusHandle: Send + Sync {
    // 합의 이벤트 수신 채널 (SoftCommit / HardCommit)
    fn event_receiver(&self) -> broadcast::Receiver<ConsensusEvent>;

    // TX 제출 (외부 → 합의)
    fn submit_transactions(&self, txs: Vec<Transaction>) -> Result<(), ConsensusError>;

    // 노드 시작/종료
    fn start(&self) -> impl Future<Output = Result<(), ConsensusError>>;
    fn stop(&self) -> impl Future<Output = ()>;
}
```

#### 3-2. InMemoryNetworkClient (테스트용 ValidatorNetworkClient)

```rust
// Phase 3 시뮬레이터용 mock 구현 명세
// ValidatorNetworkClient trait을 in-process 채널로 구현
pub struct InMemoryNetworkClient {
    // node_id → sender 맵
    // latency_model: Box<dyn LatencyModel>
}

// LatencyModel trait: uniform / bimodal / zero 지연 지원
pub trait LatencyModel: Send + Sync {
    fn delay(&self) -> Duration;
}
```

#### 3-3. ShadowDb (`crates/shadow_state`)

```rust
// revm::DatabaseRef 구현 (Arc 공유 가능)
pub struct ShadowDb {
    main_db: Arc<dyn DatabaseRef<Error = ...>>,     // 원장 DB (읽기 전용)
    write_sets: HashMap<TxId, WriteSet>,             // TX별 쓰기 추적
    read_sets:  HashMap<TxId, ReadSet>,              // TX별 읽기 추적
}

impl DatabaseRef for ShadowDb { ... }
impl DatabaseRef for Arc<ShadowDb> { ... }  // Arc 지원
```

#### 3-4. Scheduler 채널 인터페이스 (`crates/scheduler`)

```rust
// scheduler의 역할: ConsensusEvent 수신 → TxBatch 발행
pub struct SchedulerHandle {
    // input:  broadcast::Receiver<ConsensusEvent>
    // output: mpsc::Sender<TxBatch> → executor
    // backpressure: mpsc::Receiver<BackpressureSignal> ← executor
}
```

결정 산출물: `docs/interfaces.md` §3 "Trait 인터페이스"

---

### Step 4: 채널 배선 명세

모든 컴포넌트 간 채널의 방향, 타입, 용량을 확정한다.

```
[consensus]  --broadcast::Sender<ConsensusEvent>-->  [scheduler]
[scheduler]  --mpsc::Sender<TxBatch>-------------->  [executor]
[executor]   --mpsc::Sender<RoundExecutionResult>-->  [shadow_state]
[executor]   --mpsc::Sender<BackpressureSignal>---->  [scheduler]  (역방향)
[shadow_state] --oneshot::Sender<CommitResult>----->  [executor]
```

| 채널 | 타입 | 용량 | 비고 |
|------|------|------|------|
| consensus → scheduler | `broadcast` | 128 | 다수 구독자 가능성 |
| scheduler → executor | `mpsc` | 32 | 라운드 단위 배치 |
| executor → shadow_state | `mpsc` | 32 | StateDiff 전달 |
| executor → scheduler | `mpsc` | 8 | 백프레셔 신호 |

결정 산출물: `docs/interfaces.md` §4 "채널 배선"

---

### Step 5: 테스트 하네스 인터페이스 설계

Phase 3에서 구현할 결정론적 시뮬레이터의 인터페이스를 먼저 확정한다.

```rust
// 테스트 하네스 핵심 인터페이스
pub struct SimulatedNetwork {
    nodes: Vec<SimulatedNode>,
    latency_model: Box<dyn LatencyModel>,
    // fake clock: tokio::time::pause() 기반
}

impl SimulatedNetwork {
    pub fn new(committee: Committee, latency: impl LatencyModel) -> Self;
    pub fn node(&self, idx: usize) -> &SimulatedNode;
    pub fn partition(&mut self, group_a: &[usize], group_b: &[usize]);  // 파티션 주입
    pub async fn run_until_commit(&mut self, target_round: Round);
}

pub struct SimulatedNode {
    pub consensus: Box<dyn ConsensusHandle>,
    // 이 노드의 합의 이벤트 직접 접근용
}
```

결정 산출물: `docs/interfaces.md` §5 "테스트 하네스 인터페이스"

---

### Step 6: docs/interfaces.md 완성

Step 1~5의 모든 결정과 명세를 단일 문서로 통합한다.

구성:
1. 아키텍처 결정 기록 (D1~D5)
2. 핵심 타입 (`ConsensusEvent`, `TxBatch`, `StateDiff`, etc.)
3. Trait 인터페이스 (`ConsensusHandle`, `ShadowDb`, etc.)
4. 채널 배선 다이어그램
5. 테스트 하네스 인터페이스
6. crate별 구현 책임 매트릭스

---

### Step 7: Stub 선언 및 cargo check

각 crate의 `lib.rs`에 Step 1~5에서 정의한 trait/struct를 stub으로 선언한다.
실제 구현 없이 타입 시그니처만 (`todo!()` 또는 빈 본문).

```
crates/consensus/src/lib.rs    — ConsensusHandle stub
crates/shadow_state/src/lib.rs — ShadowDb stub, DatabaseRef stub
crates/scheduler/src/lib.rs    — SchedulerHandle stub
crates/executor/src/lib.rs     — ParallelExecutor stub
crates/node/src/lib.rs         — 채널 배선 stub
```

**목표**: `cargo check --workspace` 통과 + 순환 의존성 없음.

---

### Step 8: 완료 기준 검토 및 승인 요청

- phase-1.md 체크리스트 전체 완료 확인
- `docs/interfaces.md` 내용이 Phase 2~4 독립 진행에 충분한지 검토
- 사용자 승인 후 Phase 2~4 병렬 진행 여부 결정

---

## 완료 기준 (Done Criteria)

1. 5개 컴포넌트 간의 모든 인터페이스가 Rust trait/struct 수준으로 명세되어 있다.
2. `crates/` 의 각 크레이트가 어떤 trait을 구현하는지 명확하다.
3. 채널 방향과 타입이 확정되어 있다.
4. 테스트 하네스 인터페이스가 설계되어 있다 — 결정론적 시뮬레이터(정확성)와 멀티스레드 환경(벤치마크) 양쪽 모두.
5. 노드 바이너리가 Docker 배포 가능하도록 설정 외부화 및 통신 프로토콜이 명세되어 있다.
6. 이 설계를 기반으로 Phase 2~4를 독립적으로 병렬 진행할 수 있다.

---

## 테스트 기준

- [ ] `docs/interfaces.md`에 정의된 모든 trait을 `crates/` 각 `lib.rs`에 stub으로 선언했을 때 `cargo check` 통과
- [ ] 순환 의존성 없음 (`consensus` → `scheduler` → `executor` 단방향)
