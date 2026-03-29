# Phase 4: 낙관적 파이프라인 스케줄러 구현

**상태**: ⏳ 대기
**목표**: 합의 레이어(`ConsensusEvent`)와 병렬 실행 레이어(`TxBatch`) 사이의
비동기 중재자 이벤트 루프를 구현한다.
라운드 순서 보장, 2Δ↔3Δ 조정, Backpressure 제어가 핵심이다.

---

## 역할 한 줄 요약

```
Consensus ──SoftCommit(R)──▶ [Scheduler] ──TxBatch(optimistic)──▶ ParallelEVM
           ──HardCommit(R)──▶ [Scheduler] ──Commit / Discard  ──▶ ShadowState
                                            ◀──BackpressureSignal──
```

스케줄러 없이 직접 연결하면 세 가지 문제가 발생한다:

| 문제 | 설명 |
|------|------|
| **순서 역전** | 네트워크 지연으로 SoftCommit이 round 6 → round 3 순서로 도착할 수 있음. 실행은 반드시 commit_index 오름차순으로 해야 함 |
| **2Δ↔3Δ 조정** | SoftCommit으로 투기 실행한 결과를 HardCommit 시점에 확정(commit)하거나 폐기(discard)해야 함 |
| **배압 제어** | REVM이 느릴 때 합의 엔진이 계속 밀어넣으면 큐가 무한 적체됨 |

---

## 파일 구성

```
crates/scheduler/src/
├── lib.rs              ← re-exports + 공개 API (PipelineScheduler::new)
├── pipeline.rs         ← 이벤트 루프 run(), handle_soft_commit(), handle_hard_commit()
├── pending_queue.rs    ← PendingQueue (BTreeMap<Round, PendingEntry>)
└── backpressure.rs     ← BackpressureController (threshold + 히스테리시스)
```

---

## 상태 머신

```
SoftCommit(round R) 도착
    → pending_queue.insert(R, txs)
    → flush_pending(): next_dispatch_round 순서로 대기열에서 꺼내
                       executor_tx.send(TxBatch { is_optimistic: true })

HardCommit(subdag.leader.round = R, commit_index = C) 도착
    → 해당 R이 dispatched 상태인가?
        YES → CommitDecision::Commit { commit_index: C }
        NO  → CommitDecision::FreshBatch(TxBatch { is_optimistic: false })

BackpressureSignal::SlowDown → flush_pending() 일시정지
BackpressureSignal::Resume   → flush_pending() 재개
```

---

## 모듈 상세 설계

### `pending_queue.rs`

```rust
enum EntryStatus { Queued, Dispatched }

struct PendingEntry {
    round: Round,
    txs: Vec<EthSignedTx>,
    status: EntryStatus,
}

struct PendingQueue {
    entries: BTreeMap<Round, PendingEntry>, // round → 버퍼
    next_dispatch_round: Round,             // 다음에 executor로 보낼 라운드 (3→6→9…)
    pub threshold: usize,                   // backpressure 기준 depth
}
```

공개 메서드:
- `insert(round, txs)` — SoftCommit 도착 시 버퍼에 추가
- `drain_dispatchable()` — next_dispatch_round 부터 연속된 항목 순서대로 꺼내기
- `on_hard_commit(round, commit_index) → HardCommitDecision`
- `depth()` — queued + dispatched 합계 (backpressure 판단용)

```rust
enum HardCommitDecision {
    Commit { round: Round, commit_index: u64 },      // SoftCommit 있었음 → 확정
    FreshExecution { round: Round, commit_index: u64 }, // SoftCommit 없었음 → 재실행
}
```

### `backpressure.rs`

```rust
struct BackpressureController {
    threshold: usize,       // 기본값: 16
    paused: bool,
    signal_tx: mpsc::Sender<BackpressureSignal>,
}
```

- `check(depth)`: `depth > threshold` → `SlowDown` 발송, `depth < threshold / 2` → `Resume` 발송
- 히스테리시스 구간(`threshold/2 ~ threshold`)으로 진동 방지

### `pipeline.rs`

```rust
pub enum CommitDecision {
    Commit { commit_index: u64, round: Round },
    Discard { commit_index: u64 },
    FreshBatch(TxBatch),
}

pub struct PipelineScheduler {
    consensus_rx: broadcast::Receiver<ConsensusEvent>,
    executor_tx:  mpsc::Sender<TxBatch>,
    backpressure_rx: mpsc::Receiver<BackpressureSignal>,
    backpressure: BackpressureController,
    pending: PendingQueue,
    commit_tx: mpsc::Sender<CommitDecision>,
}
```

`run()` 루프:
```rust
loop {
    tokio::select! {
        event  = consensus_rx.recv()     => { /* handle */ }
        signal = backpressure_rx.recv()  => { backpressure.update(signal) }
    }
}
```

---

## Cargo.toml 변경

```toml
[dependencies]
shared   = { workspace = true }
tokio    = { workspace = true }
tracing  = "0.1"
```

---

## 실행 계획 (Execution Plan)

| 순번 | 파일 | 상태 |
|------|------|------|
| 1 | `pending_queue.rs` | ⏳ |
| 2 | `backpressure.rs` | ⏳ |
| 3 | `pipeline.rs` | ⏳ |
| 4 | `lib.rs` 업데이트 | ⏳ |
| 5 | 테스트 6개 | ⏳ |

---

## 완료 기준 (Done Criteria)

1. `cargo test -p scheduler` → 6/6 통과
2. out-of-order SoftCommit이 들어와도 executor 전달 순서가 round 오름차순 보장
3. `depth > threshold` 시 `SlowDown`, `depth < threshold/2` 시 `Resume` 신호 발송

---

## 테스트 목록

```
cargo test -p scheduler
```

| 테스트 | 시나리오 | 검증 |
|--------|----------|------|
| `test_in_order_processing` | SoftCommit(R3) → SoftCommit(R6) 정순 | executor에 R3 → R6 순서로 도착 |
| `test_out_of_order_reorder` | SoftCommit(R6) → SoftCommit(R3) 역순 | executor에 R3 먼저 도착 (버퍼링 확인) |
| `test_backpressure_triggered` | pending depth > threshold | `SlowDown` 신호 발송 확인 |
| `test_backpressure_release` | depth < threshold/2 로 감소 | `Resume` 신호 발송 확인 |
| `test_hard_commit_match` | SoftCommit(R3) dispatch → HardCommit(R3, idx=1) | `CommitDecision::Commit { commit_index: 1 }` |
| `test_hard_commit_mismatch` | HardCommit(R3) 도착 (SoftCommit 없음) | `CommitDecision::FreshBatch { is_optimistic: false }` |
