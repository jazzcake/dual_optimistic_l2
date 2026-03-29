# SUI Mysticeti 합의 분석 문서

**분석 대상**: `extern/sui/consensus/` — DAG 핵심 모듈
**목적**: Mysticeti 추출 전략 도출 및 REVM 실행 파이프라인 연결점 파악
**참조**: 기존 함수 단위 분석 문서 (Go 포팅용) + Explore 에이전트 탐색 결과
**논문**: arxiv:2310.14821v6

---

## 1. Crate 구조

```
sui/consensus/
├── types/        핵심 타입 (Round, BlockRef, BlockDigest) — 의존성 최소
├── config/       Committee, 쿼럼 계산, crypto — 경량
├── core/         합의 로직 전체 — 의존성 다수
└── simtests/     결정론적 시뮬레이션 테스트 (sui-simulator 필수)
```

---

## 2. 타입 계층

**소스**: `types/src/block.rs`

```rust
Round           = u32        // 라운드 번호
BlockTimestampMs = u64       // 블록 생성 시각 (ms)
TransactionIndex = u16       // 블록 내 TX 인덱스
BlockDigest     = [u8; 32]   // SHA3-256 해시 (MIN=[0x00;32], MAX=[0xFF;32] 상수 포함)
```

### BlockRef — 블록 고유 식별자

```rust
struct BlockRef {
    round:  Round,
    author: AuthorityIndex,  // u32
    digest: BlockDigest,
}
// Hash::hash: digest 앞 8바이트만 사용 (HashMap 성능 최적화)
// Ord: round → author → digest 순 (BTreeMap 키)
```

### BlockDigest 계산

```
BlockDigest = SHA3-256(BCS(SignedBlock))   // 서명까지 포함한 전체 바이트
```

서명 포함 → non-malleable → equivocation 방지.

### Slot — DAG 위치

```rust
struct Slot { round: Round, authority: AuthorityIndex }
// 하나의 Slot에 0, 1, 또는 다수의 블록이 있을 수 있음 (equivocation 시)
```

---

## 3. Committee & Quorum

**소스**: `config/src/committee.rs`

```rust
struct Committee {
    epoch: u64,
    total_stake: u64,
    quorum_threshold: u64,   // 2f+1
    validity_threshold: u64, // f+1
    authorities: Vec<Authority>,
}
```

### 쿼럼 계산 공식

```
f = (total_stake - 1) / 3
quorum_threshold  = total_stake - f    // ≈ 2/3 + 1
validity_threshold = f + 1

검증 성질: 두 쿼럼의 교집합 > f   (BFT 핵심 성질)
```

**우리 프로젝트 (node 기반)**: `f = (n-1)/3`, `quorum = n - f`. 수식은 동일.

---

## 4. Block 생명주기: Block → SignedBlock → VerifiedBlock

**소스**: `core/src/block.rs`

```rust
// 내용물
enum Block { V1(BlockV1), V2(BlockV2) }

struct BlockV1 {
    epoch, round, author, timestamp_ms,
    ancestors: Vec<BlockRef>,      // DAG 간선 (부모 참조)
    transactions: Vec<Transaction>,
    commit_votes: Vec<CommitVote>,
    // ...
}

// 서명 첨부
struct SignedBlock { inner: Block, signature: Bytes }

// 검증 완료 (최종 형태, Arc로 공유 소유권)
struct VerifiedBlock {
    block: Arc<SignedBlock>,
    digest: BlockDigest,   // 캐시
    serialized: Bytes,     // 캐시 (재직렬화 불필요)
}
```

`BlockAPI` trait으로 V1/V2 버전 무관하게 접근.

---

## 5. StakeAggregator

**소스**: `core/src/stake_aggregator.rs`

```rust
struct StakeAggregator<T: CommitteeThreshold> {
    votes: BTreeSet<AuthorityIndex>,  // 중복 투표 방지
    stake: Stake,
}

// add(author, &committee) → bool: threshold 도달 시 true
// 같은 authority를 여러 번 넣어도 한 번만 카운트
```

`QuorumThreshold` (2f+1), `ValidityThreshold` (f+1) 두 가지 구체 타입.

---

## 6. ThresholdClock

**소스**: `core/src/threshold_clock.rs`

같은 라운드의 블록이 2f+1 이상 수신되면 다음 라운드로 진행.

```
add_block(block_ref):
    if block.round == self.round:
        aggregator.add(block.author)
        if quorum: round += 1, clear aggregator → return true

    if block.round > self.round:
        round = block.round (+ 1 if single-node quorum)
        → return true (라운드 jump)
```

**논문 §3**: "A validator advances to round r+1 when it has received 2f+1 blocks at round r."

---

## 7. DagState

**소스**: `core/src/dag_state.rs`

DAG 전체 상태 관리. 메모리 캐시 + 디스크 영속화.

```rust
struct DagState {
    recent_blocks: BTreeMap<BlockRef, BlockInfo>,       // 최근 블록 캐시
    recent_refs_by_authority: Vec<BTreeSet<BlockRef>>,  // authority별 인덱스
    threshold_clock: ThresholdClock,                    // 라운드 진행
    last_committed_rounds: Vec<Round>,                  // authority별 커밋 라운드
    blocks_to_write: Vec<VerifiedBlock>,                // flush 대기열
    commits_to_write: Vec<TrustedCommit>,
    store: Arc<dyn Store>,
    gc_round: Round,
}
```

### 핵심 조회 함수

| 함수 | 설명 |
|------|------|
| `get_uncommitted_blocks_at_slot(Slot)` | 특정 (round, author)의 미커밋 블록들 |
| `get_uncommitted_blocks_at_round(Round)` | 특정 라운드의 미커밋 블록 전체 |
| `ancestors_at_round(block, round)` | 블록의 causal history 중 특정 라운드에 있는 블록들 |

### `ancestors_at_round` — 간접 커밋 규칙의 핵심

```
ancestors_at_round(later_block, target_round):
    linked = BTreeSet(later_block.ancestors)
    while linked not empty:
        block_ref = linked.pop_last()
        if block_ref.round <= target_round: break
        linked.extend(get_block(block_ref).ancestors)
    return linked.range(target_round ..).map(get_block)
```

### 영속화 (`flush`)

```
flush():
    store.write_batch({ blocks: blocks_to_write, commits: commits_to_write })
    // ATOMIC: 블록 + 커밋 동시 기록 → crash-safe
```

---

## 8. BlockManager

**소스**: `core/src/block_manager.rs`

조상이 미도착한 블록을 보류(suspend)하고, 조상 도착 시 재귀적으로 해제(unsuspend).

```
try_accept_blocks(blocks):
    sort by round (낮은 라운드 먼저)
    for block:
        missing_ancestors = block.ancestors.filter(not in dag_state)
        if missing_ancestors.empty():
            dag_state.accept_block(block)
            unsuspended = try_unsuspend_children(block)  // 재귀 해제
        else:
            suspend(block, missing_ancestors)
            missing_blocks.add(missing_ancestors)        // fetch 요청 대상
```

---

## 9. BaseCommitter — 커밋 규칙

**소스**: `core/src/base_committer.rs` (477줄)
**논문**: §4.2 Direct Decision Rule, §4.3 Indirect Decision Rule

### Wave 구조

```
wave_length = 3 (기본값)

Wave W:
  leader_round(W)   = W * 3 + offset   ← 리더 제안
  voting_round      = leader_round + 1  ← 투표 (참조 포함)
  decision_round(W) = W * 3 + 2 + offset ← 커밋 결정
```

### `try_direct_decide` — 직접 커밋 규칙 (논문 Algo 2)

```
try_direct_decide(leader_slot):
    voting_round   = leader.round + 1
    decision_round = leader.round + 2

    // ① Skip check: 2f+1이 리더를 무시했는가?
    if enough_leader_blame(voting_round, leader.authority):
        return Skip

    // ② Commit check: decision_round에서 2f+1이 리더를 certify했는가?
    leader_blocks = dag_state.get_uncommitted_blocks_at_slot(leader_slot)
    for leader_block in leader_blocks:
        if enough_leader_support(decision_round, leader_block):
            return Commit(leader_block)

    return Undecided
```

### `enough_leader_support` — Certificate 확인

```
is_certificate(potential_cert, leader_block):
    // potential_cert의 ancestors 중 leader에 투표한 것이 2f+1인지
    vote_aggregator = StakeAggregator::new()
    for ancestor in potential_cert.ancestors:
        if is_vote(ancestor, leader_block):
            if vote_aggregator.add(ancestor.author): return true
    return false

is_vote(potential_vote, leader_block):
    // potential_vote가 leader_block을 직접 또는 간접 참조하는가?
    return find_supported_block(leader_block.slot, potential_vote)
        == Some(leader_block.reference)

find_supported_block(leader_slot, from):
    // 재귀 탐색: from → ancestors에서 leader_slot 블록 찾기
```

### `try_indirect_decide` — 간접 커밋 규칙 (논문 Algo 3)

```
try_indirect_decide(leader_slot, already_decided):
    // anchor: 이미 커밋된 리더 중 leader.round + wave_length 이후의 것
    for anchor in already_decided.filter(round >= leader.round + 3):
        match anchor:
            Commit(anchor_block):
                // anchor의 causal history에서 leader가 certify되는지 확인
                potential_certs = dag_state.ancestors_at_round(anchor, decision_round)
                if any cert in potential_certs certifies leader:
                    return Commit
            Skip: continue
            Undecided: break  // undecided 만나면 중단

    return Undecided
```

---

## 10. UniversalCommitter

**소스**: `core/src/universal_committer.rs`

여러 BaseCommitter를 조합하여 매 라운드 커밋 가능 (pipelining).

```
try_decide(last_decided) → Vec<DecidedLeader>:
    // 높은 라운드부터 역순 탐색 (최신 직접 커밋을 먼저, anchor로 이전 것 간접 결정)
    for round in (last_decided.round+1 ..= highest-2).rev():
        status = try_direct_decide(slot)
        if not decided:
            status = try_indirect_decide(slot, already_found_leaders)

    // decided prefix만 반환 (첫 Undecided에서 중단)
    return decided_leaders
```

---

## 11. Linearizer

**소스**: `core/src/linearizer.rs`

커밋된 리더에서 실행할 블록 집합(SubDAG)을 결정론적으로 추출.

```
linearize_sub_dag(leader_block):
    buffer = [leader_block]
    to_commit = []

    while buffer not empty:
        block = buffer.pop()
        if is_committed(block): continue
        set_committed(block)
        to_commit.push(block)
        buffer.extend(block.ancestors.filter(not gc, not committed))

    sort by (round ASC, authority ASC)   // 결정론적 순서
    return to_commit
```

**논문 §4.4**: 커밋된 리더의 causal history → 이미 커밋된 것 제외 → 결정론적 정렬.

---

## 12. 실행 계층 전달 파이프라인

**소스**: `core/src/core.rs`, `commit_observer.rs`, `commit_consumer.rs`

```
블록 수신 → DagState.accept_block()
                ↓
           Core.try_commit()
                ↓
     UniversalCommitter.try_decide()        ← 커밋 규칙 적용
                ↓ Vec<DecidedLeader>
     filter: Commit만 추출 (Skip 제거)
                ↓ Vec<VerifiedBlock> (committed leaders)
     CommitObserver.handle_commit(leaders, local=true)
                ↓
     Linearizer.handle_commit()             ← SubDAG 추출 + 결정론적 정렬
                ↓
     DagState.flush()                       ← atomic write to Store
                ↓
     commit_sender (unbounded channel)      ← 실행 계층으로 전달
                ↓
     [Execution Layer]                       ← CommittedSubDag 수신 → TX 실행
```

### CommittedSubDag — 실행 계층에 전달되는 구조체

```rust
struct CommittedSubDag {
    leader: BlockRef,                   // 커밋된 리더
    blocks: Vec<VerifiedBlock>,         // SubDAG 내 모든 블록 (결정론적 순서)
    timestamp_ms: u64,                  // stake-weighted median 타임스탬프
    commit_ref: CommitRef,              // (index, digest) — 진행 추적용
    decided_with_local_blocks: bool,    // ← 핵심 플래그 (아래 설명)
    // ... (SUI 전용 필드 다수)
}
```

---

## 13. SoftCommit / HardCommit 신호 매핑 (핵심)

### 중요한 발견: SoftCommit 신호는 현재 SUI 코드에 없다

SUI의 `decided_with_local_blocks=true`는 우리의 2Δ SoftCommit이 **아니다**.

| 신호 | SUI 구현 의미 | 우리 아키텍처 의미 |
|------|--------------|------------------|
| `decided_with_local_blocks=true` | 로컬 DAG에서 결정 (vs commit syncer에서 수신) | ≠ 2Δ |
| `CommittedSubDag` channel 수신 | 전체 순서 확정 | ≈ 3Δ HardCommit |

### Wave별 타이밍 분석

```
Round R   (1Δ): 리더 L 제안 (브로드캐스트)
Round R+1 (2Δ): 다른 노드들이 L을 ancestors에 포함한 블록 생성
                ← 2f+1 블록이 L을 참조하는 시점이 우리의 SoftCommit
Round R+2 (3Δ): Decision round — enough_leader_support() 판정
                → Commit 반환 → CommittedSubDag 생성 → 실행 계층 전달
                ← 이것이 우리의 HardCommit
```

### 2Δ SoftCommit 구현 방법

SUI `try_direct_decide()`는 R+2에서만 실행된다. 우리의 2Δ는 R+1에서 감지해야 한다.
**현재 SUI 코드에 없으므로 추가 구현이 필요하다.**

구현 위치: `DagState.accept_block()` 또는 신규 모듈

```
// 새로 추가할 로직 (개념적)
accept_block(block):
    // 기존 로직...

    // 2Δ SoftCommit 감지 추가
    if is_voting_round(block.round):
        for ancestor in block.ancestors:
            if ancestor.round == block.round - 1:  // leader round
                soft_commit_tracker.add_reference(ancestor, block.author, committee)
                if soft_commit_tracker.reached_quorum(ancestor):
                    soft_commit_sender.send(SoftCommit { round: ancestor.round, leader: ancestor })
```

### 3Δ HardCommit = CommittedSubDag channel 수신

```
// 기존 SUI 코드 그대로 활용
commit_sender: UnboundedSender<CommittedSubDag>
→ 우리의 HardCommit 채널
→ CommittedSubDag.leader + CommittedSubDag.blocks → 실행 순서 확정
```

### 우리 파이프라인 신호 설계

```rust
enum ConsensusEvent {
    SoftCommit {
        round: Round,
        leader: BlockRef,
        tx_batch: Vec<Transaction>,   // SubDAG 아직 미확정, 예측 기반
    },
    HardCommit {
        subdag: CommittedSubDag,      // 최종 결정론적 순서의 TX들
    },
}
```

---

## 14. 의존성 분석 및 추출 전략

### Crate별 분류

| Crate | 분류 | 이유 |
|-------|------|------|
| `consensus-types` | ✅ 추출 용이 | fastcrypto + serde만 필요 |
| `consensus-config` | ✅ 추출 용이 | 경량 타입, crypto만 |
| `consensus-core` | ⚠️ 부분 추출 | 네트워크/메트릭 의존성 많음 |
| `consensus-simtests` | ❌ 직접 재사용 불가 | `sui-simulator` 필수 |

### consensus-core 의존성 분류

#### 제거 가능 (SUI 전용)
| 의존성 | 용도 | 대체 방법 |
|--------|------|-----------|
| `sui-macros` | `fail_point!`, `sim_test` 매크로 | no-op 매크로로 대체 |
| `sui-tls` | TLS 설정 | `rustls` 직접 사용 또는 제거 |
| `sui-http` | HTTP 유틸 | 제거 또는 대체 |
| `CommitFinalizer` | SUI fast-path TX 수락/거절 | 미사용 (우리는 불필요) |
| `TransactionCertifier` | SUI fast-path | 미사용 |
| `leader_schedule` (점수 기반) | 평판 기반 리더 선출 | 단순 라운드로빈으로 대체 |

#### 유지 (표준 Rust 생태계)
| 의존성 | 용도 |
|--------|------|
| `tokio` | async 런타임 |
| `parking_lot` | RwLock |
| `tonic` | gRPC (네트워크 레이어에만 사용) |
| `prometheus` | 메트릭 (선택적) |

#### 추상화된 인터페이스 (우리가 구현 가능)
| 인터페이스 | 설명 |
|-----------|------|
| `Store` trait | 저장소 추상화 — 우리 구현으로 교체 가능 |
| `ValidatorNetworkClient` trait | 네트워크 추상화 — gRPC 대신 우리 구현 |

### 추출 전략: 참조 방식 (path dependency)

완전 이식보다 `extern/sui`를 path dependency로 참조하면서 필요한 부분만 래핑 권장.

```toml
# crates/consensus/Cargo.toml
[dependencies]
consensus-core = { path = "../../extern/sui/consensus/core" }
consensus-types = { path = "../../extern/sui/consensus/types" }
consensus-config = { path = "../../extern/sui/consensus/config" }
```

**장점**: SUI 업스트림 변경 추적 용이, 추출/수정 부담 없음
**단점**: SUI 빌드 의존성 전체 포함

Phase 3에서 빌드 테스트 후 최종 결정.

---

## 15. 테스트 인프라: sui-simulator (msim)

**소스**: `consensus/simtests/`

```rust
// 사용 방식
#[sim_test(config = "test_config()")]
async fn test_committee_start_simple() { ... }

fn test_config() -> SimConfig {
    env_config(
        uniform_latency_ms(10..20),
        [("regional", bimodal_latency_ms(30..40, 300..800, 0.01))]
    )
}
```

**기능**:
- 가짜 시간(fake clock) — 실제 시간 불필요
- 결정론적 태스크 스케줄링 (동일 시드 → 동일 결과)
- 네트워크 지연 모델 (uniform, bimodal)
- 노드별 격리 런타임 (`sui_simulator::runtime::Handle`)

### msim 재사용 가능성

| 항목 | 판단 |
|------|------|
| `sui-simulator` 라이선스 | Apache 2.0 ✅ |
| 외부 의존성 분리 여부 | SUI 내부 매크로와 강결합 ⚠️ |
| `#[msim]` feature gate | consensus-core 전체에 산재 ⚠️ |
| 독립 사용 가능 여부 | 불가. SUI 전체 컨텍스트 필요 ❌ |

**결론**: msim을 직접 재사용하기 어렵다. 대안:
1. `tokio-test` + 커스텀 in-process 네트워크 채널로 결정론적 시뮬레이터 직접 구현
2. `turmoil` (오픈소스 결정론적 네트워크 시뮬레이터) 검토
3. 테스트용 `ValidatorNetworkClient` mock 구현 (가장 현실적)

→ `docs/test-strategy.md`에서 최종 결정.

---

## 16. REVM 연결 설계 (Phase 1 입력)

### CommittedSubDag → REVM 실행 흐름

```
[합의 계층]
CommittedSubDag {
    blocks: Vec<VerifiedBlock>,   // 결정론적 순서의 블록들
    commit_ref: CommitRef,
}
    ↓ HardCommit 채널
[스케줄러]
TxBatch {
    round: Round,
    txs: Vec<EthTx>,             // blocks에서 TX 추출
    commit_ref: CommitRef,
}
    ↓ 실행 큐
[Shadow State + REVM]
// 각 tx마다 독립된 Evm 인스턴스
// ShadowDb implements revm::Database
Evm::new(ShadowDb::for_round(round)).transact(tx)
    ↓
StateDiff (commit or discard on HardCommit)
```

### 2Δ SoftCommit에서 예측 실행

SoftCommit 시점의 TX는 CommittedSubDag의 최종 순서와 **다를 수 있다** (순서 역전 가능).
→ Shadow Memory의 R/W 충돌 감지 후 재실행으로 보정.

### 핵심 결정 사항 (Phase 1)

1. SoftCommit 감지 모듈을 `crates/consensus` 내부에 추가할지, `crates/scheduler`가 담당할지
2. 2Δ 시점의 TX 순서 예측 방법 (현재 라운드 블록 내 TX 순서 그대로 사용)
3. `ValidatorNetworkClient` 대체 구현 (gRPC vs 우리 자체 P2P)

---

## 17. 우리 프로젝트에서 유지/제거/추가할 것

| 항목 | SUI 구현 | 우리 프로젝트 |
|------|---------|--------------|
| `Committee`, 쿼럼 계산 | ✅ 재사용 | 동일 |
| `Block`, `BlockRef`, `BlockDigest` | ✅ 재사용 | 동일 |
| `StakeAggregator` | ✅ 재사용 | 동일 |
| `ThresholdClock` | ✅ 재사용 | 동일 |
| `DagState` | ✅ 재사용 (Store trait 교체) | |
| `BlockManager` | ✅ 재사용 | 동일 |
| `BaseCommitter` | ✅ 재사용 | leader 선출만 교체 |
| `UniversalCommitter` | ✅ 재사용 | |
| `Linearizer` | ✅ 재사용 | |
| `Core.try_commit()` | ✅ 재사용 | |
| `CommitObserver` | ✅ 재사용 | |
| `leader_schedule` (점수 기반) | ⚠️ 단순화 | 라운드로빈으로 대체 |
| `CommitFinalizer` | ❌ 제거 | SUI fast-path 불필요 |
| `TransactionCertifier` | ❌ 제거 | SUI fast-path 불필요 |
| `sui-simulator` 기반 테스트 | ❌ 대체 | 자체 시뮬레이터 구현 |
| **SoftCommit (2Δ) 감지** | ❌ 없음 | **신규 추가 필요** |

---

## 부록: 논문 vs 구현 대응표

| 논문 개념 | 논문 위치 | SUI 구현 |
|-----------|----------|---------|
| supports(B', B) | §3.2 | `is_vote()` + `find_supported_block()` |
| certificate(B) | §3.3 | `is_certificate()` + `enough_leader_support()` |
| skip(B) | §3.4 | `enough_leader_blame()` |
| Direct Decision | §4.2, Algo 2 | `try_direct_decide()` |
| Indirect Decision | §4.3, Algo 3 | `try_indirect_decide()` + `decide_leader_from_anchor()` |
| Commit Sequence | §4.4 | `linearize_sub_dag()` |
| Wave structure | §4.1 | `wave_number()`, `leader_round()`, `decision_round()` |
| Threshold clock | §3 | `ThresholdClock.add_block()` |
