# Phase 3: Mysticeti 합의 추출

**상태**: ✅ 3-A 완료 / 3-B 완료
**목표**: SUI 저장소에서 Mysticeti DAG 합의에 필요한 최소한의 코드를 추출하여 `crates/consensus`에 이식한다. SUI 전용 의존성 없이 독립 컴파일되어야 한다.

---

## Phase 3-A / 3-B 분할 근거

Phase 3는 성격이 다른 두 작업으로 구성된다:

| 단계 | 성격 | 목표 |
|------|------|------|
| **3-A** | 정적 작업 — 복사·수정·빌드 | `cargo build -p consensus` 통과 |
| **3-B** | 동적 작업 — 신규 설계·시뮬레이터·TDD | `cargo test -p consensus` 5/5 통과 |

---

## Phase 3-A: SUI 코드 추출 + 독립 빌드

### 이식 전략: 복사 + Apache 2.0 출처 표기

SUI는 Apache 2.0 라이선스이므로 코드를 복사하고 출처를 명시하는 방식을 사용한다.
각 파일/함수 상단에 아래 형식으로 표기한다:

```rust
// Adapted from: sui/consensus/core/src/base_committer.rs (lines N-M)
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: <수정 내용>
```

### 복사 대상 파일 (9개)

| SUI 원본 | 우리 파일 | 주요 수정 사항 |
|---------|----------|--------------|
| `types/src/block.rs` | `types.rs` | SUI 전용 필드 제거 |
| `config/src/committee.rs` | `committee.rs` | `fastcrypto` → `sha3` |
| `core/src/stake_aggregator.rs` | `stake_aggregator.rs` | 거의 그대로 |
| `core/src/threshold_clock.rs` | `threshold_clock.rs` | `prometheus` 메트릭 제거 |
| `core/src/dag_state.rs` | `dag_state.rs` | `Store` trait → in-memory |
| `core/src/block_manager.rs` | `block_manager.rs` | 거의 그대로 |
| `core/src/base_committer.rs` | `base_committer.rs` | `LeaderSchedule` → round-robin |
| `core/src/universal_committer.rs` | `universal_committer.rs` | 거의 그대로 |
| `core/src/linearizer.rs` | `linearizer.rs` | SUI fast-path 필드 제거 |

### 제거할 SUI 전용 의존성

| 의존성 | 제거 방법 |
|--------|---------|
| `fastcrypto` | SHA3 → `sha3` crate 직접 사용 |
| `sui-macros` (`fail_point!`, `sim_test`) | no-op 매크로로 대체 또는 제거 |
| `sui-tls`, `sui-http` | 제거 |
| `CommitFinalizer`, `TransactionCertifier` | 제거 (SUI fast-path, 우리 불필요) |
| `prometheus` 메트릭 | 제거 (Phase 5에서 필요 시 재추가) |
| `LeaderSchedule` (점수 기반) | `round % committee.size()` 라운드로빈으로 교체 |

### 추가할 의존성 (Cargo.toml)

```toml
[dependencies]
shared       = { workspace = true }
tokio        = { workspace = true }
parking_lot  = "0.12"
sha3         = "0.10"
rand         = "0.8"
```

### Phase 3-A 작업 목록

- [x] `types.rs` 이식 + 출처 표기 + DbC assert 추가
- [x] `committee.rs` 이식 + `fastcrypto` → `sha3` 교체
- [x] `stake_aggregator.rs` 이식 + DbC assert 추가
- [x] `threshold_clock.rs` 이식 + 메트릭 제거
- [x] `dag_state.rs` 이식 + Store → in-memory
- [x] `block_manager.rs` 이식
- [x] `base_committer.rs` 이식 + LeaderSchedule → round-robin
- [x] `universal_committer.rs` 이식
- [x] `linearizer.rs` 이식 + fast-path 필드 제거
- [x] `context.rs` 신규 작성 (경량 Context: own_index + committee)
- [x] `commit.rs` 이식 (CommittedSubDag, LeaderStatus, DecidedLeader)
- [x] `Cargo.toml` 의존성 정리
- [x] `cargo build -p consensus` 통과 확인

### Phase 3-A 완료 기준

1. `cargo build -p consensus` 가 `extern/sui` 없이 통과한다.
2. 모든 이식 파일에 Apache 2.0 출처 표기가 있다.
3. 모든 `pub` 함수에 `debug_assert!` 사전조건이 있다.

---

## Phase 3-B: SoftCommit + 시뮬레이터 + 테스트

### 신규 파일 (4개)

| 파일 | 내용 |
|------|------|
| `soft_commit.rs` | SoftCommitTracker — SUI에 없는 신규 코드. voting round(R+1)에서 2f+1 감지 |
| `node.rs` | ConsensusNode — 전체 조합 + `ConsensusEvent` 채널 발송 |
| `sim/network.rs` | SimulatedNetwork — 채널 기반 메시지 라우팅 + LatencyModel + PartitionModel |
| `sim/node.rs` | SimulatedNode + FakeClock (`tokio::time::pause`) + Seeded RNG (ChaCha8) |

### SoftCommit 설계 (SUI에 없는 신규 로직)

SUI의 `try_direct_decide()`는 R+2(decision round)에서만 실행된다.
우리의 2Δ SoftCommit은 R+1(voting round)에서 감지해야 한다.

```
accept_block(block at voting_round R+1):
    for ancestor in block.ancestors where ancestor.round == R:
        soft_commit_tracker.add_vote(ancestor_ref, block.author, committee)
        if soft_commit_tracker.reached_quorum(ancestor_ref):
            event_sender.send(ConsensusEvent::SoftCommit { round: R, leader: ancestor_ref })
```

### Phase 3-B 작업 목록

- [x] `soft_commit.rs` 구현 (SoftCommitTracker)
- [x] `node.rs` 구현 (ConsensusNode)
- [x] `sim/network.rs` 구현 (SimulatedNetwork + PartitionModel)
- [x] `sim/node.rs` 구현 (SimulatedNode + FakeClock + Seeded RNG)
- [x] `test_soft_commit_triggered` 구현 및 통과
- [x] `test_hard_commit_triggered` 구현 및 통과
- [x] `test_dag_causal_order` 구현 및 통과
- [x] `test_byzantine_node_tolerance` 구현 및 통과
- [x] `test_deterministic_replay` 구현 및 통과

### Phase 3-B 완료 기준

1. `cargo test -p consensus` → 5/5 통과
2. 결정론적 시뮬레이터가 동일 시드로 항상 동일한 이벤트 순서를 재현한다.

---

## 실행 계획 (Execution Plan)

### Phase 3-A 실행 계획 (완료)

| 순번 | 파일 | 상태 |
|------|------|------|
| 1 | `committee.rs` | ✅ SUI-dep 제거, stake/quorum 순수 구현 |
| 2 | `types.rs` | ✅ sha3 digest, 단일 Block 구조, VerifiedBlock |
| 3 | `commit.rs` | ✅ CommittedSubDag, LeaderStatus, DecidedLeader |
| 4 | `context.rs` | ✅ 신규 작성 (own_index + Arc<Committee>) |
| 5 | `stake_aggregator.rs` | ✅ 그대로, DbC assert 추가 |
| 6 | `threshold_clock.rs` | ✅ prometheus 메트릭 제거 |
| 7 | `dag_state.rs` | ✅ Store trait → BTreeMap in-memory |
| 8 | `block_manager.rs` | ✅ monitored_scope/metrics 제거 |
| 9 | `base_committer.rs` | ✅ LeaderSchedule → round % size() |
| 10 | `universal_committer.rs` | ✅ protocol_config 제거, 단순 빌더 |
| 11 | `linearizer.rs` | ✅ TrustedCommit/bcs 제거, 단순 CommittedSubDag |

| 1 | `soft_commit.rs` | ✅ SoftCommitTracker — R+1 voting round 2f+1 쿼럼 감지 |
| 2 | `node.rs` | ✅ ConsensusNode — 전체 파이프라인 조합 + 이벤트 발송 |
| 3 | `sim/mod.rs` | ✅ sim 서브모듈 진입점 |
| 4 | `sim/network.rs` | ✅ SimulatedNetwork + PartitionModel |
| 5 | `sim/node.rs` | ✅ SimulatedNode + FakeClock + StdRng(seeded) |
| 6 | tests (node.rs) | ✅ 5개 테스트 7/7 통과 |

---

## 테스트 기준

```
cargo test -p consensus
```

모든 테스트는 **결정론적 in-process 시뮬레이터** 위에서 실행한다.

- [x] `test_soft_commit_triggered` — N노드 시뮬레이터에서 2f+1 쿼럼 형성 시 SoftCommit 이벤트 발생
- [x] `test_hard_commit_triggered` — 라운드 앵커 확정 시 HardCommit 이벤트 발생
- [x] `test_dag_causal_order` — DAG에서 인과 순서가 보존됨
- [x] `test_byzantine_node_tolerance` — f개 비잔틴 노드 주입 시에도 쿼럼 정상 형성
- [x] `test_deterministic_replay` — 동일 시드로 2회 실행 시 동일한 이벤트 순서 재현
