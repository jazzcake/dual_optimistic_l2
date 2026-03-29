# Interface Design Document

> Phase 1 산출물. 구현 없이 타입/trait 경계면만 확정한다.

---

## §1 아키텍처 결정 기록 (Architecture Decision Records)

### D1 — SoftCommit 감지 위치

**결정**: (A) `crates/consensus` 내부 hook

**근거**: Wave 판정(`is_direct_commit` / `try_direct_commit`)은 Mysticeti DAG 내부 상태에 직접 접근해야 계산된다. 외부 컴포넌트(`scheduler`)가 DAG를 관찰하는 방식은 별도 폴링 + latency 추가 + 구현 복잡도 증가를 초래한다. `consensus` 크레이트가 SoftCommit 시점을 직접 감지하고 `ConsensusEvent::SoftCommit`을 emit한다.

---

### D2 — ShadowDb 구현 방식

**결정**: (B) `revm::DatabaseRef` (`&self`) + `WrapDatabaseRef`

**근거**: 병렬 TX 실행 시 여러 스레드가 동일 `ShadowDb`를 `Arc<ShadowDb>`로 공유해야 한다. `revm::Database` trait은 `&mut self`를 요구하므로 단일 소유권 제약으로 병렬화 불가. `DatabaseRef`는 `&self`이므로 `Arc` 래핑 가능. 내부 가변성(interior mutability)은 `RwLock` 또는 `DashMap`으로 처리한다.

---

### D3 — Epoch 처리

**결정**: (A) MVP: `epoch = 0` 고정

**근거**: Epoch 전환 로직(검증자 집합 교체, 체크포인트 등)은 구현 복잡도가 높다. 허가형 환경에서 MVP 단계에서는 epoch 전환이 불필요하다. Phase 5 이후 확장 대상으로 남긴다.

**Phase 5+ 확장 계획**:

Epoch = 고정된 검증자 집합이 유지되는 기간. 아래 4단계로 확장한다.

1. **타입 도입**: `pub type Epoch = u64;` + `Committee { epoch, validators: Vec<AuthorityIndex> }` 추가 (`crates/shared`)

2. **EpochManager trait** (`crates/consensus`):
   ```
   fn current_epoch() → Epoch
   fn committee(epoch: Epoch) → Committee
   fn on_epoch_end() → Future<Committee>  // 다음 검증자 집합 반환
   ```

3. **Checkpoint at epoch boundary** (`crates/shadow_state`):
   - Epoch 종료 시 모든 speculative diff를 canonical DB에 완전 병합
   - `ShadowDb::flush_epoch()` 호출로 레이어 초기화
   - 체크포인트 해시를 새 epoch의 genesis block에 포함

4. **Consensus 재시작** (`crates/consensus`):
   - `ConsensusHandle::restart(new_committee: Committee)` 추가
   - Mysticeti는 epoch 단위로 fresh start (extern/sui의 기존 epoch 로직 활용 가능)
   - 새 epoch의 라운드 번호는 0부터 재시작 OR 전체 누적 라운드 유지 (결정 필요)

**접근 전략**: extern/sui의 `EpochManager`, `CommitteeStore` 구현을 먼저 분석한 뒤 우리 wrapper에 맞게 조정한다. SUI의 stake-weighted epoch 로직 중 stake 부분만 제거하면 node-based(D4)와 호환된다.

---

### D4 — Quorum 방식

**결정**: (A) Node-based (1 node = 1 vote)

**근거**: 허가형(Permissioned) 환경 + 소규모 노드 수 조건에서 stake-weighted quorum은 불필요한 복잡도다. 모든 노드 동등 투표권으로 구현 단순화. Stake-weighted는 Phase 5 이후 확장 대상.

---

### D5 — Conflict 감지 단위

**결정**: (A) Storage slot 단위

**근거**: `revm::EvmState`는 `HashMap<Address, AccountInfo + HashMap<U256, StorageSlot>>` 구조로 슬롯 단위를 이미 노출한다. 슬롯 단위 R/W set 추적은 REVM이 제공하는 정보를 그대로 활용하므로 구현 복잡도 추가가 없다. 계정 단위를 선택하면 ERC-20처럼 동일 컨트랙트에 접근하는 TX들이 모두 conflict로 분류되어 병렬화율이 급감한다. `ReadSet` = `HashSet<(Address, U256)>`, `WriteSet` = `HashMap<(Address, U256), SlotValue>`.

---

## §2 핵심 타입 (Core Types)

> 모든 crate 경계를 가로지르는 공유 타입. `crates/shared/src/lib.rs`에 위치.

### Round / BlockRef

```rust
pub type Round = u64;

/// Identifies a block within the DAG.
pub struct BlockRef {
    pub round: Round,
    pub author: AuthorityIndex,
    pub digest: BlockDigest,
}
```

---

### ConsensusEvent

```rust
/// Events emitted by the consensus module to downstream consumers.
pub enum ConsensusEvent {
    /// Optimistic pre-commit: 2Δ wave leader detected.
    /// Transactions may be executed speculatively.
    SoftCommit {
        round: Round,
        leader: BlockRef,
        txs: Vec<EthSignedTx>,
    },
    /// Final commit: 3Δ subDAG committed.
    /// Execution results must be finalized; conflicting speculative results discarded.
    HardCommit {
        subdag: OurCommittedSubDag,
    },
}
```

---

### OurCommittedSubDag

```rust
/// Slimmed-down version of SUI's CommittedSubDag — SUI-specific fields removed.
pub struct OurCommittedSubDag {
    pub leader: BlockRef,
    pub blocks: Vec<OurVerifiedBlock>,
    pub timestamp_ms: u64,
    pub commit_index: u64,
}

/// Wrapper around a DAG block carrying Ethereum transactions.
pub struct OurVerifiedBlock {
    pub block_ref: BlockRef,
    pub txs: Vec<EthSignedTx>,
}
```

---

### TxBatch

```rust
/// Unit of work passed from scheduler to executor.
pub struct TxBatch {
    pub round: Round,
    pub commit_index: u64,
    pub txs: Vec<EthSignedTx>,
    /// true  = SoftCommit-based (speculative)
    /// false = HardCommit-based (final)
    pub is_optimistic: bool,
}
```

---

### StateDiff

```rust
/// Storage changes produced by executing one round of transactions.
/// Passed from executor to shadow state for commit or discard.
pub struct StateDiff {
    pub round: Round,
    pub commit_index: u64,
    pub is_optimistic: bool,
    pub changes: HashMap<Address, AccountDiff>,
}

pub struct AccountDiff {
    pub balance: Option<U256>,
    pub nonce: Option<u64>,
    pub code: Option<Bytes>,
    /// Slot-level changes (D5: slot granularity).
    pub storage: HashMap<U256, U256>,
}
```

---

### ExecutionResult

```rust
/// Result of executing one TxBatch.
pub struct RoundExecutionResult {
    pub round: Round,
    pub commit_index: u64,
    pub is_optimistic: bool,
    pub results: Vec<TxExecutionResult>,
    pub state_diff: StateDiff,
    /// Indices into `results` of transactions with R/W conflicts detected.
    pub conflict_txs: Vec<usize>,
}

pub enum TxExecutionResult {
    Success {
        tx_hash: TxHash,
        gas_used: u64,
    },
    Revert {
        tx_hash: TxHash,
        gas_used: u64,
        reason: Bytes,
    },
    Invalid {
        tx_hash: TxHash,
        error: String,
    },
}
```

---

### BackpressureSignal

```rust
/// Signal sent from executor back to scheduler to regulate flow.
pub enum BackpressureSignal {
    /// Executor queue is filling up; scheduler should pause SoftCommit dispatch.
    SlowDown,
    /// Executor has capacity; scheduler may resume normal dispatch.
    Resume,
}
```

---

## §3 Trait 인터페이스 (Component Interfaces)

> 각 crate의 공개 API. 구현 없이 시그니처만.

### ConsensusHandle (`crates/consensus`)

```rust
/// External interface of the consensus module.
pub trait ConsensusHandle: Send + Sync {
    /// Subscribe to consensus events (SoftCommit / HardCommit).
    /// Uses broadcast so multiple consumers can subscribe independently.
    fn event_receiver(&self) -> broadcast::Receiver<ConsensusEvent>;

    /// Submit Ethereum transactions into the consensus pipeline.
    fn submit_transactions(&self, txs: Vec<EthSignedTx>) -> Result<(), ConsensusError>;

    /// Start the consensus engine (spawns internal tasks).
    fn start(&self) -> impl Future<Output = Result<(), ConsensusError>> + Send;

    /// Gracefully shut down the consensus engine.
    fn stop(&self) -> impl Future<Output = ()> + Send;
}
```

---

### SchedulerHandle (`crates/scheduler`)

```rust
/// Scheduler mediates ConsensusEvent → TxBatch dispatch.
/// Not a trait — a concrete struct holding channel endpoints.
pub struct SchedulerHandle {
    /// Receives consensus events.
    consensus_rx: broadcast::Receiver<ConsensusEvent>,
    /// Sends transaction batches to executor.
    executor_tx: mpsc::Sender<TxBatch>,
    /// Receives backpressure signals from executor.
    backpressure_rx: mpsc::Receiver<BackpressureSignal>,
}

impl SchedulerHandle {
    pub fn new(
        consensus_rx: broadcast::Receiver<ConsensusEvent>,
        executor_tx: mpsc::Sender<TxBatch>,
        backpressure_rx: mpsc::Receiver<BackpressureSignal>,
    ) -> Self;

    /// Run the scheduler event loop (consumes self, meant to be spawned).
    pub async fn run(self);
}
```

---

### ParallelExecutor (`crates/executor`)

```rust
/// Interface for the parallel EVM execution engine.
pub trait ParallelExecutor: Send + Sync {
    /// Execute a batch of transactions against the shadow state.
    /// Returns execution results including conflict detection.
    fn execute(
        &self,
        batch: TxBatch,
        db: Arc<ShadowDb>,
    ) -> impl Future<Output = Result<RoundExecutionResult, ExecutorError>> + Send;
}
```

---

### ShadowDb (`crates/shadow_state`)

```rust
/// Multi-version shadow database layered on top of the canonical ledger.
/// Implements DatabaseRef for Arc-sharing across parallel executor threads.
pub struct ShadowDb {
    /// Canonical (committed) state — read-only.
    canonical: Arc<dyn DatabaseRef<Error = DbError> + Send + Sync>,
    /// Per-round speculative write sets, keyed by commit_index.
    speculative: RwLock<BTreeMap<u64, StateDiff>>,
    /// Per-TX read tracking for conflict detection (D5: slot granularity).
    read_sets: DashMap<TxHash, ReadSet>,
}

impl DatabaseRef for ShadowDb {
    type Error = DbError;
    // basic_ref, code_by_hash_ref, storage_ref, block_hash_ref
}

impl ShadowDb {
    /// Apply a finalized StateDiff to canonical state. Drops all speculative
    /// diffs with commit_index <= the applied index.
    pub fn commit(&self, diff: StateDiff) -> Result<(), DbError>;

    /// Discard a speculative StateDiff (HardCommit conflict resolution).
    pub fn discard(&self, commit_index: u64);

    /// Detect R/W conflicts between a new diff and existing speculative diffs.
    pub fn detect_conflicts(&self, diff: &StateDiff) -> Vec<TxHash>;
}

pub type ReadSet  = HashSet<(Address, U256)>;
pub type WriteSet = HashMap<(Address, U256), U256>;
```

---

### CommitWrapper (`crates/executor`)

```rust
/// Wraps execution output: routes StateDiff to commit or discard path.
pub trait CommitWrapper: Send + Sync {
    fn on_hard_commit(
        &self,
        result: RoundExecutionResult,
        db: Arc<ShadowDb>,
    ) -> impl Future<Output = Result<(), CommitError>> + Send;

    fn on_conflict_discard(
        &self,
        commit_index: u64,
        db: Arc<ShadowDb>,
    ) -> impl Future<Output = ()> + Send;
}
```

---

### LatencyModel + InMemoryNetworkClient (`crates/consensus` / test)

```rust
/// Pluggable network latency for deterministic simulation.
pub trait LatencyModel: Send + Sync {
    fn delay(&self) -> Duration;
}

/// Zero latency (default for unit tests).
pub struct ZeroLatency;
impl LatencyModel for ZeroLatency {
    fn delay(&self) -> Duration { Duration::ZERO }
}

/// Uniform random latency within [min, max].
pub struct UniformLatency { pub min: Duration, pub max: Duration }

/// In-process implementation of ValidatorNetworkClient for simulation.
pub struct InMemoryNetworkClient {
    /// node_id → channel sender
    peers: HashMap<AuthorityIndex, mpsc::Sender<NetworkMessage>>,
    latency: Box<dyn LatencyModel>,
}
```

---

## §4 채널 배선 (Channel Wiring)

### 데이터 흐름 다이어그램

```
┌─────────────┐  broadcast<ConsensusEvent>  ┌─────────────┐
│  consensus  │ ─────────────────────────►  │  scheduler  │
└─────────────┘       cap: 128              └──────┬──────┘
                                                   │ mpsc<TxBatch>
                                                   │ cap: 32
                                                   ▼
                                           ┌───────────────┐
                                           │   executor    │
                                           └──────┬────────┘
                              mpsc<BackpressureSignal>  │  mpsc<RoundExecutionResult>
                                   cap: 8  ◄────────────┘  cap: 32
                              ┌─────────────┐              │
                              │  scheduler  │              ▼
                              └─────────────┘   ┌──────────────────┐
                                                 │  shadow_state    │
                                                 └──────────────────┘
                                                          │ oneshot<CommitResult>
                                                          ▼
                                                     [executor callback]
```

### 채널 명세 테이블

| 채널 | 방향 | 타입 | 용량 | 비고 |
|------|------|------|------|------|
| consensus → scheduler | → | `broadcast::Sender<ConsensusEvent>` | 128 | 다수 구독자 가능성 대비 broadcast |
| scheduler → executor | → | `mpsc::Sender<TxBatch>` | 32 | 라운드 단위 배치; 32 라운드 미처리 시 스케줄러 block |
| executor → shadow_state | → | `mpsc::Sender<RoundExecutionResult>` | 32 | StateDiff 포함 결과 전달 |
| executor → scheduler | ← | `mpsc::Sender<BackpressureSignal>` | 8 | 역방향; 큐 포화 시 SlowDown 신호 |
| shadow_state → executor | ← | `oneshot::Sender<CommitResult>` | 1 (oneshot) | commit/discard 완료 응답 |

### 백프레셔 정책

- executor가 `TxBatch` 큐 사용률 ≥ 75% 감지 시 `BackpressureSignal::SlowDown` 전송
- scheduler는 `SlowDown` 수신 시 `SoftCommit` 기반 투기적 배치 전송 일시 중단 (HardCommit 배치는 계속 전송)
- `BackpressureSignal::Resume` 수신 시 정상 재개

---

## §5 테스트 하네스 인터페이스 (Test Harness)

> Phase 3에서 구현. 여기서는 인터페이스만 확정.

### 결정론적 시뮬레이터 (정확성 검증용)

```rust
/// In-process multi-node simulation network.
/// Uses tokio::time::pause() for deterministic time control.
pub struct SimulatedNetwork {
    nodes: Vec<SimulatedNode>,
    latency_model: Box<dyn LatencyModel>,
    /// Partition table: if partitions[a][b] == true, node a cannot reach node b.
    partitions: Vec<Vec<bool>>,
}

impl SimulatedNetwork {
    /// Create a network with `n` nodes using the given latency model.
    pub fn new(n: usize, latency: impl LatencyModel + 'static) -> Self;

    /// Access a node by index.
    pub fn node(&self, idx: usize) -> &SimulatedNode;

    /// Inject a network partition: nodes in group_a cannot reach nodes in group_b.
    pub fn partition(&mut self, group_a: &[usize], group_b: &[usize]);

    /// Heal all partitions.
    pub fn heal_partitions(&mut self);

    /// Advance simulation until the given round is committed on all nodes.
    pub async fn run_until_commit(&mut self, target_round: Round);

    /// Advance simulation for a fixed number of simulated rounds.
    pub async fn run_rounds(&mut self, n: u64);
}

/// A single in-process node within the simulation.
pub struct SimulatedNode {
    pub index: usize,
    pub consensus: Box<dyn ConsensusHandle>,
    /// Direct access to this node's consensus event stream for assertions.
    pub event_rx: broadcast::Receiver<ConsensusEvent>,
}

impl SimulatedNode {
    /// Collect all ConsensusEvents emitted so far (non-blocking drain).
    pub fn drain_events(&mut self) -> Vec<ConsensusEvent>;

    /// Assert that a SoftCommit was emitted at the given round.
    pub fn assert_soft_commit(&mut self, round: Round);

    /// Assert that a HardCommit was emitted at the given round.
    pub fn assert_hard_commit(&mut self, round: Round);
}
```

### 멀티스레드 벤치마크 환경 (Δ 측정용)

```rust
/// Real-time benchmark harness for measuring pipeline latency.
/// Does NOT use tokio::time::pause() — wall-clock time is authoritative.
pub struct BenchmarkHarness {
    pub network: SimulatedNetwork,
    /// Records wall-clock timestamps of SoftCommit and HardCommit events.
    pub timeline: Arc<Mutex<CommitTimeline>>,
}

pub struct CommitTimeline {
    /// round → (soft_commit_ts, hard_commit_ts)
    pub entries: BTreeMap<Round, CommitTimestamps>,
}

pub struct CommitTimestamps {
    pub soft_commit_at: Option<Instant>,
    pub hard_commit_at: Option<Instant>,
    pub execution_done_at: Option<Instant>,
}

impl BenchmarkHarness {
    /// Compute the observed Δ (average inter-round commit interval).
    pub fn measure_delta(&self) -> Duration;

    /// Compute the observed pipeline benefit: hard_commit - soft_commit per round.
    pub fn measure_pipeline_gain(&self) -> Duration;
}
```

---

## §6 Crate 구현 책임 매트릭스

| Trait / Struct | 위치 | 의존 crate |
|---------------|------|-----------|
| `ConsensusEvent`, `TxBatch`, `StateDiff`, `RoundExecutionResult`, `BackpressureSignal` | `crates/shared` | (없음 — leaf crate) |
| `ConsensusHandle` trait + `InMemoryNetworkClient` | `crates/consensus` | `shared` |
| `SchedulerHandle` | `crates/scheduler` | `shared`, `consensus` |
| `ShadowDb`, `CommitWrapper` | `crates/shadow_state` | `shared`, `revm` |
| `ParallelExecutor` | `crates/executor` | `shared`, `shadow_state` |
| `SimulatedNetwork`, `BenchmarkHarness` | `crates/testkit` | `shared`, `consensus` |
| 채널 배선 (§4) + 노드 바이너리 | `crates/node` | 전체 |

**의존성 방향 (순환 없음)**:

```
shared → consensus → scheduler → executor → shadow_state
                  ↘                        ↗
                   node ←─────────────────
testkit → consensus (테스트 전용, dev-dependency)
```

---

## §7 Docker 배포 설정 명세

### 환경변수 / 설정 파일 외부화

모든 노드 파라미터는 하드코딩 금지. 우선순위: 환경변수 > 설정 파일 > 기본값.

| 파라미터 | 환경변수 | 기본값 | 비고 |
|---------|---------|--------|------|
| 노드 인덱스 | `NODE_INDEX` | — | 필수 |
| 위원회 크기 | `COMMITTEE_SIZE` | 4 | |
| 합의 포트 | `CONSENSUS_PORT` | 8000 | 노드 간 DAG 통신 |
| RPC 포트 | `RPC_PORT` | 9000 | TX 제출 수신 |
| 헬스체크 포트 | `HEALTH_PORT` | 9001 | HTTP GET /health |
| 피어 주소 목록 | `PEERS` | — | 쉼표 구분, `host:port` 형식 |
| 로그 레벨 | `RUST_LOG` | `info` | |

### 포트 및 프로토콜 명세

| 포트 | 프로토콜 | 방향 | 용도 |
|------|---------|------|------|
| 8000 | TCP (gRPC 또는 raw bincode) | 양방향 | Mysticeti DAG 블록 교환 |
| 9000 | TCP (HTTP/JSON-RPC) | 인바운드 | TX 제출 (eth_sendRawTransaction 호환) |
| 9001 | TCP (HTTP) | 인바운드 | 헬스체크 / 메트릭 |

### 헬스체크 엔드포인트

```
GET /health
→ 200 OK  { "status": "ok", "round": <u64>, "commit_index": <u64> }
→ 503     { "status": "syncing" }
```

---
