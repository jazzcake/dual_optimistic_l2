# Phase 5: 통합 & 벤치마크

**상태**: ⏳ 대기
**목표**: tx 페이로드 배선 완성 → Mock 실행기로 E2E 통합 테스트 → 실제 REVM 연결 후 `min(Δ, E)` 이득을 수치로 검증한다.

Docker 멀티노드 환경 구축 및 Foundry 컨트랙트 테스트는 **Phase 6**으로 분리.

---

## Phase 4 이월 과제 (선결 조건)

> 아래 항목들은 Phase 4 완료 후 발견된 미완성 사항이다.
> **5-A에서 가장 먼저 해결한다.**

### 과제 1 — `to_shared_subdag()` tx 채우기

`VerifiedBlock.transactions()` API는 이미 존재한다 (`&[Transaction]`).
`Transaction(Vec<u8>)` → `EthSignedTx(Vec<u8>)` 변환 1줄만 추가하면 된다.

```rust
// crates/consensus/src/node.rs — to_shared_subdag()
// 현재
OurVerifiedBlock { txs: vec![], .. }
// 수정
OurVerifiedBlock { txs: b.transactions().iter().map(|t| EthSignedTx(t.0.clone())).collect(), .. }
```

### 과제 2 — `check_soft_commit()` causal subDAG tx 수집

리더의 전체 causal subDAG tx를 2Δ 시점에 수집해야 한다.
`Linearizer::linearize_sub_dag()`와 동일한 DFS이되, `set_committed()` 호출이 없는 **read-only** 버전이 필요하다.
`ConsensusNode.dag_state`가 `#[allow(dead_code)]`로 보유된 것이 이 구현을 위한 준비 흔적이다.

```rust
// crates/consensus/src/node.rs — check_soft_commit()
// 현재
ConsensusEvent::SoftCommit { txs: vec![], .. }
// 수정: dag_state read lock으로 causal cone 순회
let txs = collect_causal_txs(&self.dag_state.read(), leader_ref, dag_state.last_committed_round());
ConsensusEvent::SoftCommit { txs, .. }
```

### 과제 3 — `TestBlock`에 tx 주입 메서드 추가

통합 테스트에서 tx 페이로드를 담은 블록을 만들 수 있어야 한다.

```rust
// crates/consensus/src/types.rs
impl TestBlock {
    pub fn set_transactions(mut self, txs: Vec<Transaction>) -> Self { .. }
}
```

---

## 서브페이즈 구성

```
5-A  tx 페이로드 배선     ── 이월 과제 1·2·3 해결
5-B  실행 레이어 Mock     ── MockExecutor + node 배관
5-C  E2E 통합 테스트      ── 결정론적 시뮬레이터 기반
5-D  실제 REVM + 벤치마크 ── ParallelExecutor 구현 + 수치 검증
```

---

## 5-A: tx 페이로드 배선

| 순번 | 파일 | 작업 |
|------|------|------|
| 1 | `consensus/src/node.rs` | `to_shared_subdag()` — `b.transactions()` → `EthSignedTx` 변환 |
| 2 | `consensus/src/dag_state.rs` | `get_causal_blocks(leader_ref, gc_round)` — read-only causal DFS 추가 |
| 3 | `consensus/src/node.rs` | `check_soft_commit()` — causal tx 수집 후 `SoftCommit.txs` 채우기 |
| 4 | `consensus/src/types.rs` | `TestBlock::set_transactions()` 추가 |
| 5 | `consensus/src/node.rs` | 기존 7개 테스트에서 tx 페이로드 흐름 검증 추가 |

**완료 기준**: `cargo test -p consensus` 7/7 유지, `SoftCommit.txs`와 `HardCommit subdag.blocks[*].txs` 모두 비어있지 않음.

---

## 5-B: 실행 레이어 Mock + node 배관

| 순번 | 파일 | 작업 |
|------|------|------|
| 1 | `parallel-evm/src/lib.rs` | `MockExecutor` — `TxBatch` 수신 즉시 `RoundExecutionResult` 반환 (실제 REVM 없음) |
| 2 | `parallel-evm/src/lib.rs` | `MockCommitWrapper` — commit/discard 이벤트 로그만 기록 |
| 3 | `node/src/lib.rs` | `Node::start()` — `ConsensusNode` + `PipelineScheduler` + `MockExecutor` 채널 배선 |
| 4 | `node/src/lib.rs` | `NodeConfig::from_env()` — committee_size, node_index 최소 구현 |

**채널 배선 확인:**
```
ConsensusNode ──broadcast──▶ PipelineScheduler ──mpsc──▶ MockExecutor
                                    ▲ CommitDecision           │ RoundExecutionResult
                                    └──────────────────────────┘
                                    ◀── BackpressureSignal ────┘
```

---

## 5-C: E2E 통합 테스트

`crates/testkit` + `crates/node` 기반. `tokio::time::pause()` 활용 결정론적 실행.

| 테스트 | 시나리오 | 검증 항목 |
|--------|---------|----------|
| `test_e2e_single_round` | 4노드, R3 wave 1회 | `SoftCommit` → `TxBatch(is_optimistic=true)` → `HardCommit` → `CommitDecision::Commit` |
| `test_e2e_multi_round` | 연속 3 wave (R3·R6·R9) | `commit_index` 1·2·3 오름차순, `TxBatch` 누락 없음 |
| `test_e2e_soft_hard_tx_match` | tx 페이로드 있는 블록 | `SoftCommit.txs` == `HardCommit subdag` 전체 tx |
| `test_e2e_out_of_order` | SoftCommit R6 → R3 역순 도착 | executor 수신 순서 R3 → R6 |
| `test_e2e_backpressure` | MockExecutor 응답 지연 주입 | `SlowDown` 발송 → 큐 감소 후 `Resume` |
| `test_e2e_byzantine_f1` | 4노드 f=1, node-0 블록 없음 | HardCommit 정상 도달 |

---

## 5-D: 실제 REVM + 벤치마크

| 순번 | 작업 | 내용 |
|------|------|------|
| 1 | `ParallelExecutor` 구현 | `ShadowDb` + REVM 연결, read/write set 추적 |
| 2 | `BenchmarkHarness` 구현 | `CommitTimestamps` 기록, `measure_pipeline_gain()` 완성 |
| 3 | 기준 모델 구현 | `3Δ` 대기 후 직렬 실행 경로 |
| 4 | 지연 주입 | `tokio::time::sleep` 기반 Δ 시뮬레이션 |
| 5 | `docs/benchmark-results.md` | 측정 결과 기록 |

**벤치마크 검증 기준:**

| 측정 항목 | 기대 결과 |
|----------|----------|
| `optimistic_latency` vs `baseline_latency` | optimistic < baseline (모든 충돌률 구간) |
| 충돌률 0% | 이득 = `min(Δ, E)` |
| 충돌률 100% | 이득 > 0 (수학적 하한 유지) |

---

## 실행 계획 (Execution Plan)

> 이 섹션은 Phase 시작 전 사용자와 함께 수립하고 승인받은 후 채운다.

---

## 완료 기준 (Done Criteria)

1. `cargo test` (전체) 통과 — E2E 통합 테스트 6개 포함
2. `SoftCommit.txs`와 `HardCommit subdag.blocks[*].txs`가 일치함을 테스트로 확인
3. 벤치마크에서 제안 모델이 기준 모델 대비 `min(Δ, E)` 이상 지연 단축 측정
4. 충돌률 증가 시에도 제안 모델의 상대적 우위가 수치로 유지됨
5. `docs/benchmark-results.md` 작성 완료

---

## 테스트 목록

```
cargo test
cargo bench
```

**5-A (tx 페이로드)**
- [ ] `cargo test -p consensus` 7/7 유지 + tx 흐름 검증

**5-C (E2E 통합)**
- [ ] `test_e2e_single_round`
- [ ] `test_e2e_multi_round`
- [ ] `test_e2e_soft_hard_tx_match`
- [ ] `test_e2e_out_of_order`
- [ ] `test_e2e_backpressure`
- [ ] `test_e2e_byzantine_f1`

**5-D (벤치마크)**
- [ ] `bench_baseline`
- [ ] `bench_optimistic`
- [ ] `bench_conflict_sweep`
