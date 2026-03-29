# SUI Mysticeti 합의 분석 문서

**분석 대상**: `extern/sui/consensus/` — DAG 핵심 모듈
**목적**: Mysticeti 추출 전략 도출 및 REVM 실행 파이프라인 연결점 파악
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

### 함수

| 함수 | 시그니처 | 설명 |
|------|---------|------|
| `new` | `(Epoch, Vec<Authority>) → Committee` | f, quorum, validity 계산 |
| `reached_quorum` | `(Stake) → bool` | `stake >= quorum_threshold` |
| `reached_validity` | `(Stake) → bool` | `stake >= validity_threshold` |
| `stake` | `(AuthorityIndex) → Stake` | 특정 authority의 stake 조회 |
| `size` | `() → usize` | authority 수 |
| `authorities` | `() → Iterator<(AuthorityIndex, &Authority)>` | 전체 순회 |

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
    ancestors: Vec<BlockRef>,      // DAG 간선 (부모 참조, ≥ quorum)
    transactions: Vec<Transaction>,
    commit_votes: Vec<CommitVote>,
    misbehavior_reports: Vec<MisbehaviorReport>,
    // V2 = V1 + transaction_votes (SUI fast-path용, 우리는 미사용)
}

// 서명 첨부
struct SignedBlock { inner: Block, signature: Bytes }  // Ed25519 서명

// 검증 완료 (최종 형태, Arc로 공유 소유권)
struct VerifiedBlock {
    block: Arc<SignedBlock>,
    digest: BlockDigest,   // 캐시
    serialized: Bytes,     // 캐시 (재직렬화 불필요)
}
```

### trait BlockAPI — 블록 버전 무관 접근

| 메서드 | 반환 | 설명 |
|--------|------|------|
| `epoch()` | `Epoch` | |
| `round()` | `Round` | |
| `author()` | `AuthorityIndex` | |
| `slot()` | `Slot` | `(round, author)` 쌍 |
| `timestamp_ms()` | `u64` | |
| `ancestors()` | `&[BlockRef]` | **DAG 간선** — 부모 블록 참조 |
| `transactions()` | `&[Transaction]` | |
| `commit_votes()` | `&[CommitVote]` | 커밋 투표 (커밋 동기화용) |

### SignedBlock 서명/검증 과정

```
1. inner_digest = SHA3-256(BCS(Block))        // Block 내용의 해시
2. intent_msg = IntentMessage(ConsensusBlock, inner_digest)
3. signature = keypair.sign(BCS(intent_msg))  // 서명
4. verification: pubkey.verify(BCS(intent_msg), signature)
```

### VerifiedBlock 함수

| 함수 | 설명 |
|------|------|
| `new_verified(SignedBlock, Bytes)` | 검증 완료 후 생성. digest 계산 + 캐시 |
| `reference()` | `→ BlockRef { round, author, digest }` |
| `digest()` | 캐시된 digest 반환 |
| `serialized()` | 캐시된 직렬화 바이트 반환 |
| `compute_digest(serialized)` | `SHA3-256(BCS(SignedBlock))` — **서명 포함** |

---

## 5. StakeAggregator

**소스**: `core/src/stake_aggregator.rs`

```rust
trait CommitteeThreshold {
    fn is_threshold(committee: &Committee, amount: Stake) → bool;
}

struct QuorumThreshold;    // 2f+1
struct ValidityThreshold;  // f+1

struct StakeAggregator<T: CommitteeThreshold> {
    votes: BTreeSet<AuthorityIndex>,  // 중복 투표 방지
    stake: Stake,
}
```

### 함수

| 함수 | 시그니처 | 설명 |
|------|---------|------|
| `new()` | `→ Self` | 빈 aggregator |
| `add` | `(AuthorityIndex, &Committee) → bool` | 투표 추가. **threshold 도달 시 true 반환** |
| `add_unique` | `(AuthorityIndex, &Committee) → bool` | 새 authority면 true (threshold 무관) |
| `stake()` | `→ Stake` | 현재 누적 stake |
| `reached_threshold` | `(&Committee) → bool` | threshold 도달 여부 |
| `clear()` | | 초기화 |

**핵심 동작**: `add()`는 같은 authority를 여러 번 넣어도 **한 번만 카운트** (`BTreeSet`). threshold 도달 시 즉시 true 반환.

---

## 6. ThresholdClock

**소스**: `core/src/threshold_clock.rs`

같은 라운드의 블록이 2f+1 이상 수신되면 다음 라운드로 진행.

```rust
struct ThresholdClock {
    context: Arc<Context>,
    aggregator: StakeAggregator<QuorumThreshold>,
    round: Round,
    quorum_ts: Instant,    // 마지막 quorum 달성 시각 (지연 측정용)
}
```

### 함수

| 함수 | 시그니처 | 설명 |
|------|---------|------|
| `new` | `(Round, Arc<Context>) → Self` | 복구 시 특정 라운드부터 시작 |
| `add_block` | `(BlockRef) → bool` | **핵심 함수**. 라운드 진행 시 true |
| `get_round()` | `→ Round` | 현재 라운드 |
| `get_quorum_ts()` | `→ Instant` | 마지막 quorum 시각 |

### `add_block` 알고리즘

```
add_block(block_ref):
    match block.round vs self.round:

    case block.round < self.round:
        → return false                    // 과거 블록, 무시

    case block.round == self.round:
        → aggregator.add(block.author)
        → if quorum 도달:
            round += 1
            aggregator.clear()
            return true                   // 라운드 진행!
        → return false

    case block.round > self.round:
        → aggregator.clear()
        → aggregator.add(block.author)
        → if quorum 도달:                  // 이 블록 하나로도 quorum이면
            round = block.round + 1        // (committee size = 1 같은 경우)
        → else:
            round = block.round            // jump 하되 아직 quorum 아님
        → return true                     // 어쨌든 라운드가 바뀜
```

**논문 §3**: "A validator advances to round r+1 when it has received 2f+1 blocks at round r."

---

## 7. DagState

**소스**: `core/src/dag_state.rs`

DAG 전체 상태 관리. 메모리 캐시 + 디스크 영속화.

```rust
struct DagState {
    context: Arc<Context>,

    // === DAG 데이터 ===
    genesis: BTreeMap<BlockRef, VerifiedBlock>,           // 제네시스 블록 (상수)
    recent_blocks: BTreeMap<BlockRef, BlockInfo>,          // 최근 블록 캐시
    recent_refs_by_authority: Vec<BTreeSet<BlockRef>>,     // authority별 인덱스

    // === 라운드 관리 ===
    threshold_clock: ThresholdClock,                       // 라운드 진행
    evicted_rounds: Vec<Round>,                            // GC watermark (authority별)
    highest_accepted_round: Round,                         // 최고 수신 라운드

    // === 커밋 상태 ===
    last_commit: Option<TrustedCommit>,
    last_committed_rounds: Vec<Round>,                     // authority별 커밋 라운드

    // === 버퍼 (flush 전) ===
    blocks_to_write: Vec<VerifiedBlock>,
    commits_to_write: Vec<TrustedCommit>,

    // === 저장소 ===
    store: Arc<dyn Store>,
    cached_rounds: Round,                                  // 캐시 유지 라운드 수 (~50)
}
```

### 핵심 함수 — 블록 수락/조회

| 함수 | 시그니처 | 설명 |
|------|---------|------|
| `accept_block` | `(&mut self, VerifiedBlock)` | 블록을 DAG에 수락 |
| `accept_blocks` | `(&mut self, Vec<VerifiedBlock>)` | 여러 블록 수락 |
| `get_block` | `(&self, &BlockRef) → Option<VerifiedBlock>` | 단일 블록 조회 |
| `get_blocks` | `(&self, &[BlockRef]) → Vec<Option<VerifiedBlock>>` | 다중 블록 조회 |
| `contains_block` | `(&self, &BlockRef) → bool` | 존재 여부 |

### `accept_block` 알고리즘

```
accept_block(block):
    assert block.round != 0           // genesis는 별도 관리
    if already contains: return        // 중복 무시

    // equivocation 체크 (자기 자신 블록만)
    if block.author == own_index:
        assert slot에 기존 블록 없음

    update_block_metadata(block):
        recent_blocks[block_ref] = BlockInfo(block)
        recent_refs_by_authority[author].insert(block_ref)
        threshold_clock.add_block(block_ref)      // ← 라운드 진행 트리거
        highest_accepted_round = max(current, block.round)

    blocks_to_write.push(block)       // flush 대기열
```

### `get_blocks` — 2단계 조회

```
get_blocks(block_refs):
    blocks = [None; len]
    missing = []

    // 1단계: 메모리 캐시 조회
    for (i, ref) in block_refs:
        if ref.round == 0:
            blocks[i] = genesis[ref]
        else if recent_blocks.contains(ref):
            blocks[i] = recent_blocks[ref]
        else:
            missing.push((i, ref))

    // 2단계: 디스크 조회 (캐시 미스)
    if missing not empty:
        store_results = store.read_blocks(missing_refs)
        for (i, result) in zip(missing, store_results):
            blocks[i] = result

    return blocks
```

### 핵심 함수 — 커밋 규칙에서 사용

| 함수 | 시그니처 | 설명 |
|------|---------|------|
| `get_uncommitted_blocks_at_slot` | `(Slot) → Vec<VerifiedBlock>` | 특정 (round, author)의 미커밋 블록들 |
| `get_uncommitted_blocks_at_round` | `(Round) → Vec<VerifiedBlock>` | 특정 라운드의 미커밋 블록 전체 |
| `ancestors_at_round` | `(&VerifiedBlock, Round) → Vec<VerifiedBlock>` | 블록의 조상 중 특정 라운드에 있는 것들 |
| `get_last_block_for_authority` | `(AuthorityIndex) → VerifiedBlock` | authority의 최신 블록 |

### 핵심 함수 — 커밋 상태

| 함수 | 시그니처 | 설명 |
|------|---------|------|
| `set_committed` | `(&mut self, &BlockRef) → bool` | 블록을 "커밋됨"으로 표시. 이미 커밋이면 false |
| `is_committed` | `(&self, &BlockRef) → bool` | 커밋 여부 |
| `last_commit_round` | `() → Round` | 마지막 커밋 라운드 |
| `last_committed_rounds` | `() → Vec<Round>` | authority별 마지막 커밋 라운드 |
| `gc_round` | `() → Round` | GC 라운드 = last_commit_round - gc_depth |
| `add_commit` | `(TrustedCommit)` | 커밋을 버퍼에 추가 |

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

### GC (Garbage Collection)

```
gc_round = last_commit_leader_round - gc_depth
eviction_round(authority) = max(gc_round, highest_round[authority] - cached_rounds)

// eviction_round 이하의 블록은 메모리에서 제거, 디스크에만 존재
```

---

## 8. BlockManager

**소스**: `core/src/block_manager.rs`

조상이 미도착한 블록을 보류(suspend)하고, 조상 도착 시 재귀적으로 해제(unsuspend).

```rust
struct SuspendedBlock {
    block: VerifiedBlock,
    missing_ancestors: BTreeSet<BlockRef>,
    timestamp: Instant,
}

struct BlockManager {
    context: Arc<Context>,
    dag_state: Arc<RwLock<DagState>>,

    suspended_blocks: BTreeMap<BlockRef, SuspendedBlock>,
    // missing ancestor → 그것을 기다리는 블록들
    missing_ancestors: BTreeMap<BlockRef, BTreeSet<BlockRef>>,
    // 아직 fetch하지 못한 블록
    missing_blocks: BTreeSet<BlockRef>,
}
```

### 핵심 함수

| 함수 | 시그니처 | 설명 |
|------|---------|------|
| `try_accept_blocks` | `(Vec<VerifiedBlock>) → (Vec<VerifiedBlock>, BTreeSet<BlockRef>)` | 블록 수락 시도. 수락된 블록 + 누락된 조상 반환 |
| `try_accept_one_block` | `(VerifiedBlock) → TryAcceptResult` | 단일 블록 수락/보류/스킵 |
| `try_unsuspend_children_blocks` | `(BlockRef) → Vec<VerifiedBlock>` | 조상 도착 시 자식 블록 해제 |
| `missing_blocks()` | `→ &BTreeSet<BlockRef>` | 동기화 필요한 블록 목록 |

### `try_accept_blocks` 알고리즘

```
try_accept_blocks(blocks):
    blocks.sort_by(round)          // 낮은 라운드부터 처리
    accepted = []
    missing = Set()

    for block in blocks:
        result = try_accept_one_block(block)

        match result:
            Accepted(block):
                // 이 블록의 자식 중 보류 상태인 것 해제
                unsuspended = try_unsuspend_children_blocks(block.ref)
                all = [block] + unsuspended
                dag_state.accept_blocks(all)
                accepted.extend(all)

            Suspended(missing_ancestors):
                missing.extend(missing_ancestors)

            Processed | Skipped:
                continue

    return (accepted, missing)
```

### `try_accept_one_block` — 수락/보류 결정

```
try_accept_one_block(block):
    if dag_state.contains(block.ref):
        return Processed                    // 이미 있음

    if block is already suspended:
        return Processed                    // 이미 보류됨

    // 조상 확인
    missing = Set()
    for ancestor in block.ancestors:
        if ancestor.round == 0: continue    // genesis는 항상 존재
        if dag_state.contains(ancestor): continue
        if suspended_blocks.contains(ancestor): continue  // fetch됐지만 아직 보류

        missing.insert(ancestor)

    if missing.empty():
        return Accepted(block)
    else:
        // 보류 등록
        suspended_blocks[block.ref] = SuspendedBlock(block, missing)
        for ancestor in missing:
            missing_ancestors[ancestor].insert(block.ref)
            if ancestor not in suspended_blocks:
                missing_blocks.insert(ancestor)   // fetch 요청 대상
        return Suspended(missing_blocks_to_fetch)
```

### `try_unsuspend_children_blocks` — 재귀적 해제

```
try_unsuspend_children_blocks(accepted_ref):
    unsuspended = []

    if let Some(children) = missing_ancestors.remove(accepted_ref):
        missing_blocks.remove(accepted_ref)

        for child_ref in children:
            suspended = suspended_blocks[child_ref]
            suspended.missing_ancestors.remove(accepted_ref)

            if suspended.missing_ancestors.empty():
                // 모든 조상 도착! 해제
                block = suspended_blocks.remove(child_ref).block
                unsuspended.push(block)
                // 재귀: 이 블록의 자식도 해제 시도
                unsuspended.extend(
                    try_unsuspend_children_blocks(child_ref)
                )

    return unsuspended
```

---

## 9. BaseCommitter — 커밋 규칙

**소스**: `core/src/base_committer.rs` (477줄)
**논문**: §4.2 Direct Decision Rule, §4.3 Indirect Decision Rule

### Wave 구조

```rust
struct BaseCommitterOptions {
    wave_length: u32,      // 기본 3 (leader → voting → decision)
    leader_offset: u32,    // multi-leader 시 offset
    round_offset: u32,     // pipelining 시 offset
}

struct BaseCommitter {
    context: Arc<Context>,
    leader_schedule: Arc<LeaderSchedule>,   // ← 우리는 다른 방식으로 교체
    dag_state: Arc<RwLock<DagState>>,
    options: BaseCommitterOptions,
}
```

### Wave 계산 함수

| 함수 | 공식 | 설명 |
|------|------|------|
| `wave_number(round)` | `(round - round_offset) / wave_length` | 라운드 → wave 번호 |
| `leader_round(wave)` | `wave * wave_length + round_offset` | wave의 첫 라운드 (리더 라운드) |
| `decision_round(wave)` | `wave * wave_length + wave_length - 1 + round_offset` | wave의 마지막 라운드 |

**예시** (wave_length=3, offset=0):
```
Wave W:
  leader_round(W)   = W * 3 + offset   ← 리더 제안
  voting_round      = leader_round + 1  ← 투표 (참조 포함)
  decision_round(W) = W * 3 + 2 + offset ← 커밋 결정

Wave 0: leader=R0, voting=R1, decision=R2
Wave 1: leader=R3, voting=R4, decision=R5
```

### `elect_leader` — 리더 선출

```
elect_leader(round):
    wave = wave_number(round)
    if leader_round(wave) != round:
        return None                    // 이 라운드는 리더 라운드가 아님

    authority = leader_schedule.elect_leader(round, leader_offset)
    return Slot(round, authority)
```

**우리 프로젝트**: `leader_schedule.elect_leader()`를 라운드로빈 또는 자체 방식으로 교체.

### `try_direct_decide` — 직접 커밋 규칙 (논문 Algo 2)

```
try_direct_decide(leader_slot):
    voting_round   = leader.round + 1
    decision_round = decision_round(wave_number(leader.round))

    // ① Skip check: 2f+1이 리더를 무시했는가?
    if enough_leader_blame(voting_round, leader.authority):
        return Skip(leader_slot)

    // ② Commit check: decision_round에서 2f+1이 리더를 certify했는가?
    leader_blocks = dag_state.get_uncommitted_blocks_at_slot(leader_slot)
    supported = leader_blocks
        .filter(|b| enough_leader_support(decision_round, b))
        .map(Commit)

    // BFT 가정 하에 최대 1개만 support 가능
    assert supported.len() <= 1
    return supported.pop() or Undecided(leader_slot)
```

### `enough_leader_blame` — Skip 판정

```
enough_leader_blame(voting_round, leader_authority):
    voting_blocks = dag_state.get_uncommitted_blocks_at_round(voting_round)
    blame_aggregator = StakeAggregator::new()

    for block in voting_blocks:
        // 이 블록의 ancestors에 leader authority의 블록이 없으면 → blame
        if block.ancestors.all(|a| a.author != leader_authority):
            if blame_aggregator.add(block.author): return true

    return false
```

**논문 §3.4**: "skip(B, round_r+1_blocks) = |{B' : ¬supports(B', B)}| ≥ 2f+1"

### `enough_leader_support` — Commit 판정

```
enough_leader_support(decision_round, leader_block):
    decision_blocks = dag_state.get_uncommitted_blocks_at_round(decision_round)

    // 빠른 거부: 전체 stake가 quorum 미달이면 불가능
    total = sum(committee.stake(b.author) for b in decision_blocks)
    if total < quorum: return false

    cert_aggregator = StakeAggregator::new()
    all_votes = HashMap()     // 캐시 (같은 블록을 반복 체크 방지)

    for block in decision_blocks:
        if is_certificate(block, leader_block, &mut all_votes):
            if cert_aggregator.add(block.author): return true

    return false
```

### `is_certificate` — 인증 패턴 확인

```
is_certificate(potential_certificate, leader_block, all_votes):
    // potential_certificate의 ancestors 중 leader에 투표한 것이 2f+1인지 확인
    vote_aggregator = StakeAggregator::new()

    for ancestor_ref in potential_certificate.ancestors:
        is_vote = all_votes.get(ancestor_ref) or {
            potential_vote = dag_state.get_block(ancestor_ref)
            result = is_vote(potential_vote, leader_block)
            all_votes[ancestor_ref] = result   // 캐싱으로 반복 탐색 방지
            result
        }

        if is_vote:
            if vote_aggregator.add(ancestor_ref.author): return true

    return false
```

### `is_vote` — 투표 확인

```
is_vote(potential_vote, leader_block):
    leader_slot = Slot(leader_block.reference)
    supported = find_supported_block(leader_slot, potential_vote)
    return supported == Some(leader_block.reference)
```

### `find_supported_block` — 재귀 탐색

```
find_supported_block(leader_slot, from_block):
    if from_block.round < leader_slot.round:
        return None

    for ancestor in from_block.ancestors:
        if Slot(ancestor) == leader_slot:
            return Some(ancestor)          // 직접 참조

        if ancestor.round <= leader_slot.round:
            continue                       // 더 낮은 라운드는 스킵

        // 재귀: ancestor를 통해 간접 참조 확인 (weak link 처리)
        ancestor_block = dag_state.get_block(ancestor)
        if let Some(support) = find_supported_block(leader_slot, ancestor_block):
            return Some(support)

    return None
```

**논문 §3.2**: "supports(B', B) = B.hash ∈ B'.parents". SUI 구현에서는 **간접 support**도 재귀로 탐색 (weak link 대응).

### `try_indirect_decide` — 간접 커밋 규칙 (논문 Algo 3)

```
try_indirect_decide(leader_slot, already_decided_leaders):
    // anchor = leader_slot 이후 wave_length 이상 뒤의 committed leader
    anchors = already_decided_leaders
        .filter(|l| l.round() >= leader_slot.round + wave_length)

    for anchor in anchors:
        match anchor:
            Commit(anchor_block):
                return decide_leader_from_anchor(anchor_block, leader_slot)
            Skip(_): continue
            Undecided(_): break       // undecided 만나면 중단

    return Undecided(leader_slot)
```

### `decide_leader_from_anchor` — anchor 기반 결정

```
decide_leader_from_anchor(anchor, leader_slot):
    leader_blocks = dag_state.get_uncommitted_blocks_at_slot(leader_slot)

    // anchor에서 leader_slot의 decision_round까지 역추적
    wave = wave_number(leader_slot.round)
    decision_round = decision_round(wave)
    potential_certs = dag_state.ancestors_at_round(anchor, decision_round)

    // potential_certs 중 leader_block을 certify하는 것이 있는지
    certified = leader_blocks.filter(|leader_block| {
        potential_certs.any(|cert| is_certificate(cert, leader_block, &mut cache))
    })

    assert certified.len() <= 1    // BFT 가정
    return certified.pop().map(Commit) or Skip(leader_slot)
```

**핵심 차이**: direct는 "해당 라운드의 모든 블록"에서 찾지만, indirect는 "anchor의 causal history에 있는 블록"에서만 찾는다.

---

## 10. UniversalCommitter

**소스**: `core/src/universal_committer.rs`

여러 BaseCommitter를 조합하여 매 라운드 커밋 가능 (pipelining).

```rust
struct UniversalCommitter {
    context: Arc<Context>,
    dag_state: Arc<RwLock<DagState>>,
    committers: Vec<BaseCommitter>,     // 여러 개의 BaseCommitter
}
```

### Builder — Pipelining & Multi-leader

```
// pipeline_stages = wave_length if pipeline enabled, else 1
// number_of_leaders = configurable (default 1)
for round_offset in 0..pipeline_stages:
    for leader_offset in 0..number_of_leaders:
        committers.push(BaseCommitter { wave_length, round_offset, leader_offset })

// Pipelining(wave_length=3): 3개의 BaseCommitter가 각각 offset 0, 1, 2로 동작.
// → 매 라운드마다 다른 committer가 리더를 선출 → 사실상 매 라운드 커밋 가능.
```

### `try_decide` — 전체 커밋 결정

```
try_decide(last_decided):
    highest = dag_state.highest_accepted_round()
    leaders = VecDeque()

    // 높은 라운드부터 역순 탐색
    // R+2에서 R의 리더를 결정하므로, highest-2까지만
    for round in (last_decided.round+1 ..= highest-2).rev():
        for committer in committers.rev():
            slot = committer.elect_leader(round)    // ← 우리 리더 선출로 교체
            if slot is None: continue
            if slot == last_decided: break outer

            // 1차: direct rule
            status = committer.try_direct_decide(slot)

            // 2차: indirect rule (이미 결정된 리더를 anchor로)
            if status is not decided:
                status = committer.try_indirect_decide(slot, leaders.iter())

            leaders.push_front((status, decision_type))

    // decided prefix만 추출 (첫 Undecided에서 중단)
    decided = []
    for (leader, _) in leaders:
        if leader is decided:
            decided.push(leader.into_decided())
        else:
            break
    return decided
```

**핵심 포인트**:
1. **역순 탐색**: 높은 라운드부터 → 최신 직접 커밋을 먼저 찾고, 그것을 anchor로 이전 리더를 간접 결정
2. **decided prefix**: 연속된 decided 리더만 반환. undecided가 나오면 중단.

---

## 11. Linearizer

**소스**: `core/src/linearizer.rs`

커밋된 리더에서 실행할 블록 집합(SubDAG)을 결정론적으로 추출.

```rust
struct Linearizer {
    context: Arc<Context>,
    dag_state: Arc<RwLock<DagState>>,
}
```

### 핵심 함수

| 함수 | 시그니처 | 설명 |
|------|---------|------|
| `handle_commit` | `(Vec<VerifiedBlock>) → Vec<CommittedSubDag>` | 커밋된 리더들 → SubDAG 목록 |
| `collect_sub_dag_and_commit` | `(VerifiedBlock) → (CommittedSubDag, TrustedCommit)` | 단일 리더 → SubDAG + Commit |
| `linearize_sub_dag` | `(VerifiedBlock, &mut DagState) → Vec<VerifiedBlock>` | **핵심**: DAG 탐색 + 정렬 |
| `calculate_commit_timestamp` | `(...) → BlockTimestampMs` | 커밋 타임스탬프 (stake weighted median) |

### `linearize_sub_dag` — DAG 플래튼 알고리즘

```
linearize_sub_dag(leader_block, dag_state):
    gc_round = dag_state.gc_round()
    buffer = [leader_block]
    to_commit = []

    dag_state.set_committed(leader_block.ref)

    // BFS/DFS 탐색
    while buffer not empty:
        x = buffer.pop()
        to_commit.push(x)

        for ancestor_ref in x.ancestors:
            if ancestor_ref.round <= gc_round: continue      // GC 이하 스킵
            if dag_state.is_committed(ancestor_ref): continue // 이미 커밋 스킵

            ancestor = dag_state.get_block(ancestor_ref)
            buffer.push(ancestor)
            dag_state.set_committed(ancestor_ref)             // 중복 방지

    // 결정론적 정렬
    sort by (round ASC, authority ASC)
    return to_commit
```

**논문 §4.4**: 커밋된 리더의 causal history → 이미 커밋된 것 제외 → 결정론적 정렬.

### `calculate_commit_timestamp` — 타임스탬프

```
calculate_commit_timestamp(leader_block, last_commit_ts):
    // leader의 부모(round-1) 블록들의 타임스탬프를 stake 가중 median으로 계산
    parents = leader.ancestors.filter(|a| a.round == leader.round - 1)
    parent_blocks = dag_state.get_blocks(parents)
    median_ts = median_timestamp_by_stake(parent_blocks)

    // 단조 증가 보장
    return max(median_ts, last_commit_ts)
```

---

## 12. 실행 계층 전달 파이프라인

**소스**: `core/src/core.rs`, `commit_observer.rs`, `commit_consumer.rs`

### 전체 파이프라인

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

### Core.`try_commit` — 오케스트레이터

```
try_commit(certified_commits):
    committed_sub_dags = []

    loop:
        // ① 리더 스케줄 갱신 체크 (우리는 다른 방식으로 교체)
        commits_until_update = leader_schedule.commits_until_update()
        if commits_until_update == 0:
            leader_schedule.update()

        // ② 커밋 규칙 실행
        decided_leaders = committer.try_decide(last_decided_leader)
        decided_leaders.truncate(commits_until_update)

        if decided_leaders.empty(): break

        // ③ last_decided_leader 갱신
        last_decided_leader = decided_leaders.last().slot()

        // ④ Skip 제거, Commit만 추출
        sequenced_leaders = decided_leaders.filter_map(|d| d.into_committed_block())
        if sequenced_leaders.empty(): break

        // ⑤ CommitObserver로 전달 → Linearizer → 영속화 → 실행 계층
        subdags = commit_observer.handle_commit(sequenced_leaders, local=true)
        committed_sub_dags.extend(subdags)

    // ⑥ 자기 블록 커밋 알림 (TX consumer에게)
    notify_own_blocks_status(committed_block_refs)

    return committed_sub_dags
```

**핵심 포인트**: `try_commit`은 **loop** — 리더 스케줄 업데이트 경계를 넘을 수 있으므로 반복 실행.

### CommitObserver.`handle_commit`

```
handle_commit(committed_leaders, local):
    // ① Linearizer로 SubDAG 추출
    committed_sub_dags = linearizer.handle_commit(committed_leaders)

    // ② local 플래그 설정
    for subdag in committed_sub_dags:
        subdag.decided_with_local_blocks = local

    // ③ CommitFinalizer로 전송 (unbounded channel)
    for commit in committed_sub_dags:
        commit_finalizer_handle.send(commit)

    return committed_sub_dags
```

### CommittedSubDag — 실행 계층에 전달되는 구조체

```rust
struct CommittedSubDag {
    // === Linearizer가 설정 ===
    leader: BlockRef,                   // 커밋된 리더
    blocks: Vec<VerifiedBlock>,         // SubDAG 내 모든 블록 (결정론적 순서)
    timestamp_ms: u64,                  // stake-weighted median 타임스탬프
    commit_ref: CommitRef,              // (index, digest) — 진행 추적용

    // === CommitObserver가 설정 ===
    decided_with_local_blocks: bool,    // 로컬 DAG에서 결정? vs 커밋 동기화?

    // === SUI 전용 (우리 불필요) ===
    rejected_transactions_by_block: BTreeMap<BlockRef, Vec<TransactionIndex>>,
    always_accept_system_transactions: bool,
    reputation_scores_desc: Vec<(AuthorityIndex, u64)>,
}
```

### CommitConsumerArgs — 실행 계층과의 인터페이스

```rust
struct CommitConsumerArgs {
    replay_after_commit_index: CommitIndex,
    consumer_last_processed_commit_index: CommitIndex,
    commit_sender: UnboundedSender<CommittedSubDag>,  // ← 핵심 채널
}
```

### 영속화 순서 (Crash Safety)

```
1. DagState.accept_block()     → blocks_to_write 버퍼
2. Linearizer → DagState.add_commit() → commits_to_write 버퍼
3. DagState.flush()            → Store.write(WriteBatch { blocks, commits })  // ATOMIC
4. commit_sender.send()        → 실행 계층 전달
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
| `AncestorStateManager` | ❌ 제거 또는 미정 | leader scoring 의존 |
| `sui-simulator` 기반 테스트 | ❌ 대체 | 자체 시뮬레이터 구현 |
| **SoftCommit (2Δ) 감지** | ❌ 없음 | **신규 추가 필요** |

### 교체 필요한 구체적 지점

| SUI 구현 | 우리 프로젝트 | 이유 |
|---------|------------|------|
| `leader_schedule.elect_leader()` | 별도 설계 | 리더 선출 방식 상이 |
| `TransactionCertifier` | 제거 | SUI fast-path 불필요 |
| `CommitFinalizer` | 제거 | SUI fast-path 불필요 |
| `ValidatorNetworkClient` (gRPC) | Mock(테스트) + gRPC(프로덕션) 구현 | Phase 3 mock, Phase 5 gRPC |
| `TransactionClient` | Ethereum TX 수신용으로 교체 | JSON-RPC / P2P 게이트웨이 |
| `AncestorStateManager` | 제거 또는 단순화 | 평판 기반 선출 미사용 |
| `CommitSyncer` | 제거 가능 | 기본 합의에 불필요, 복구 전용 |
| `RoundProber` | 제거 또는 단순화 | 최소 구현에서 선택적 |
| `RocksDBStore` | MemStore(테스트) + 향후 교체 | Phase 3에서 결정 |
| `BCS serialization` | 유지 또는 교체 | Rust에서는 재사용 가능 |
| `Arc<RwLock<>>` 패턴 | 유지 | Rust 동시성 모델 동일 |
| Stake-weighted quorum | Node-based 또는 stake | Phase 1에서 결정 필요 |
| Epoch 관리 | 단순화(고정 epoch 0) 또는 완전 지원 | Phase 1에서 결정 필요 |

---

## 18. Validator (Authority) 구조 및 역할

**소스**: `config/src/committee.rs`, `core/src/core.rs`

### Authority 구조체

```rust
pub struct Authority {
    pub stake: Stake,                    // 투표 가중치 (스테이킹 양에 비례)
    pub address: Multiaddr,              // 네트워크 주소
    pub hostname: String,                // 메트릭/로깅용
    pub authority_name: AuthorityName,   // SUI 측 권한 이름과 매핑
    pub protocol_key: ProtocolPublicKey, // 블록 서명/검증용
    pub network_key: NetworkPublicKey,   // TLS 및 네트워크 신원
}
```

### AuthorityIndex

```rust
// 단순 u32 래퍼 — Committee.authorities Vec에서의 0-indexed 위치
struct AuthorityIndex(u32);
// Vec, 배열 인덱싱 트레이트 구현 → committee[idx]로 직접 접근
// 상수: ZERO (항상 유효), MIN, MAX (BTreeMap range scan용)
```

### 밸리데이터의 이중 역할

| 역할 | 동작 |
|------|------|
| **Block Proposer** | 매 라운드마다 TX를 포함한 블록 1개 제안, 이전 라운드 블록들을 ancestors로 참조, protocol_key로 서명 |
| **Voter** | 수신한 블록을 ancestors에 포함시킴으로써 투표 (암묵적 투표 방식), 명시적 거절 시 commit_votes에 포함 |

**핵심**: SUI Mysticeti는 별도 투표 메시지가 없다. 다음 라운드 블록의 ancestors에 리더를 포함하는 것이 곧 투표.

### Core — 밸리데이터 오케스트레이터

**소스**: `core/src/core.rs`

Core 구조체가 밸리데이터의 모든 동작을 조율한다.

```rust
struct Core {
    context: Arc<Context>,
    transaction_consumer: TransactionConsumer,   // TX 풀링
    transaction_certifier: Box<dyn TransactionCertifier>,  // TX 수락/거절 (SUI전용)
    block_manager: BlockManager,                 // DAG 의존성 관리
    dag_state: Arc<RwLock<DagState>>,           // DAG 상태
    committer: UniversalCommitter,               // 커밋 결정
    commit_observer: CommitObserver,             // 실행 계층 전달
    leader_schedule: Arc<LeaderSchedule>,        // 리더 선출
    ancestor_state_manager: AncestorStateManager, // 블록 배포 품질 추적
    last_included_ancestors: LastIncludedAncestors, // 중복 ancestors 방지
    // ...signals, round tracking 등
}
```

**`Core::recover()`**: 재시작 후 마지막 커밋 상태에서 블록 생산 및 커밋 재개.

---

## 19. Committee 구축 파이프라인 & Context

**소스**: `config/src/committee.rs`, `core/src/context.rs`, `core/src/authority_node.rs`

### Context — epoch별 공유 설정

```rust
pub struct Context {
    pub epoch_start_timestamp_ms: u64,
    pub own_index: AuthorityIndex,         // 이 밸리데이터의 index
    pub committee: Committee,              // epoch별 Committee
    pub parameters: Parameters,           // 블록 크기 제한 등
    pub protocol_config: ConsensusProtocolConfig,
    pub metrics: Arc<Metrics>,
    pub clock: Arc<Clock>,
}
```

모든 consensus 컴포넌트가 `Arc<Context>`를 공유 소유.

### AuthorityNode 초기화 파이프라인

```
SUI 프로토콜 (외부)
    │
    ↓ ConsensusAuthority::start(committee, own_keypair, ...)
AuthorityNode::start()
    ├─ Context::new(committee, own_index, parameters)   ← Committee 주입
    ├─ DagState::new(store)                             ← 마지막 커밋 복구
    ├─ TransactionClient::new(context)                  ← TX 수신 채널 생성
    ├─ Core::new(context, dag_state, ...)               ← 합의 엔진 생성
    ├─ Synchronizer::new(context, ...)                  ← 블록 동기화 시작
    ├─ CommitSyncer::new(...)                           ← 히스토리 동기화
    └─ NetworkManager::start()                          ← gRPC 서버 시작
```

**`Committee`는 SUI 프로토콜에서 완성된 형태로 주입된다.** consensus 모듈이 Committee를 직접 구축하지 않는다.

### 우리 프로젝트에서의 Committee 주입

```rust
// Phase 1에서 설계할 인터페이스 개념
fn start_consensus_node(
    committee: Committee,       // 외부에서 주입 (우리가 직접 구성)
    own_index: AuthorityIndex,
    keypair: ProtocolKeyPair,
    // ...
) -> ConsensusHandle
```

---

## 20. Epoch 개념 및 전환

**소스**: `config/src/committee.rs`, `core/src/authority_node.rs`

### Epoch 타입

```rust
pub type Epoch = u64;   // SUI 프로토콜이 단조 증가시킴
```

- Committee에 `epoch: Epoch` 태그
- 모든 Block에 `epoch: Epoch` 포함
- `BlockVerifier`: `block.epoch != context.committee.epoch()` → 검증 거부

### Epoch Change 트리거

**Epoch 변경은 consensus 모듈 외부(SUI 프로토콜)에서 결정한다.**

```
SUI 프로토콜 측:
    주기적 체크 (시간 or 블록 수 기반)
        → epoch boundary 도달
        → 새 Committee 결정 (새 validator 집합, stake 변경 등)
        → ConsensusAuthority::stop() 호출
        → 새 Committee로 ConsensusAuthority::start() 재호출
```

### Epoch 전환 시 consensus 동작

```
1. AuthorityNode::stop() (순차적 정지)
   - Synchronizer, CommitSyncer 정지 (Core 호출하는 것들 먼저)
   - LeaderTimeoutTask 정지
   - CoreThread 정지 (블록 생산 중단)
   - NetworkManager 정지

2. 영속화 상태 유지
   - 이전 epoch의 DagState, Store는 그대로 보존
   - 마지막 커밋 정보(CommitRef, 평판 점수) 저장

3. AuthorityNode::start() with new Committee
   - 새 Context(새 Committee, 동일 own_index or 변경된 index)
   - 새 DagState::new() — 마지막 커밋 복구
   - 새 epoch의 genesis 블록 생성
   - 새 LeaderSchedule (복구된 평판 점수 기반)

4. 이전 epoch 블록 수락 불가 (epoch 불일치 → 검증 거부)
```

### 우리 프로젝트에서의 Epoch

우리 L2에서 epoch 개념이 필요한지 여부는 Phase 1에서 결정:
- **단순화 옵션**: epoch = 0으로 고정, validator 집합 변경 없이 운용
- **완전 지원 옵션**: SUI와 동일한 방식으로 외부에서 epoch 관리

---

## 21. 블록 동기화 (Synchronizer)

**소스**: `core/src/synchronizer.rs`, `core/src/network/mod.rs`

### Synchronizer 두 가지 모드

```
1. Live Sync (즉각적 조상 조회)
   - 트리거: BlockManager에서 missing_ancestors 발생
   - 동작: 즉시 peer 2곳에 fetch 요청
   - 타임아웃: 2~4초
   - 동시 요청: 동일 블록에 최대 2개 동시 진행

2. Periodic Sync (주기적 누락 보정)
   - 트리거: 타이머 또는 Live Sync 완료 직후
   - 동작: 아직 받지 못한 블록을 최대 3개 peer에서 배치 조회
   - 목적: 브로드캐스트 누락 보정
```

### ValidatorNetworkClient trait

**소스**: `core/src/network/mod.rs`

```rust
pub trait ValidatorNetworkClient: Send + Sync + Sized + 'static {
    // 새 블록 스트리밍 구독
    async fn subscribe_blocks(
        &self,
        peer: AuthorityIndex,
        last_received: Round,
        timeout: Duration,
    ) -> ConsensusResult<BlockStream>;

    // 특정 블록들을 명시적으로 요청
    async fn fetch_blocks(
        &self,
        peer: AuthorityIndex,
        block_refs: Vec<BlockRef>,
        highest_accepted_rounds: Vec<Round>,
        breadth_first: bool,
        timeout: Duration,
    ) -> ConsensusResult<Vec<Bytes>>;

    // 과거 커밋 범위 조회 (CommitSyncer 용)
    async fn fetch_commits(
        &self,
        peer: AuthorityIndex,
        commit_range: CommitRange,
        timeout: Duration,
    ) -> ConsensusResult<(Vec<Bytes>, Vec<Bytes>)>;

    // 최신 블록 조회 (Synchronizer 용)
    async fn fetch_latest_blocks(
        &self,
        peer: AuthorityIndex,
        authorities: Vec<AuthorityIndex>,
        timeout: Duration,
    ) -> ConsensusResult<Vec<Bytes>>;

    // 최신 라운드 정보 조회 (RoundProber 용)
    async fn get_latest_rounds(
        &self,
        peer: AuthorityIndex,
        timeout: Duration,
    ) -> ConsensusResult<(Vec<Round>, Vec<Round>)>;
}
```

### missing_blocks 충족 흐름

```
블록 수신 → BlockManager.try_accept_blocks()
                ↓ missing_ancestors 발견
           Synchronizer.fetch_blocks(missing_refs)
                ↓ lock (중복 요청 방지)
           ValidatorNetworkClient.fetch_blocks(peer, refs)  ← RPC
                ↓ Vec<Bytes> (직렬화된 블록)
           BlockVerifier.verify()
                ↓ VerifiedBlock
           BlockManager에 재삽입 → try_unsuspend_children_blocks()
```

### 우리 프로젝트에서의 네트워크 추상화

`ValidatorNetworkClient`는 **우리가 구현할 핵심 인터페이스**다.

| 구현 옵션 | 용도 |
|---------|------|
| **테스트용 mock** (`InMemoryNetworkClient`) | 계층 1 시뮬레이터에서 채널로 구현 |
| **실제 gRPC** (`tonic`) | 프로덕션, Docker 멀티노드 환경 |

Phase 3에서 in-process mock 구현, Phase 5 Docker에서 gRPC 구현.

---

## 22. 트랜잭션 소싱 — Mempool 없음

**소스**: `core/src/transaction.rs`, `core/src/authority_node.rs`

### 핵심 발견: Mysticeti에는 별도 Mempool이 없다

전통적인 mempool 대신 **bounded channel** 방식을 사용한다.

```rust
const MAX_PENDING_TRANSACTIONS: usize = 2_000;

struct TransactionConsumer {
    tx_receiver: Receiver<TransactionsGuard>,         // 채널 수신 끝
    max_transactions_in_block_bytes: u64,             // 블록 크기 제한
    max_num_transactions_in_block: u64,               // 블록 내 TX 수 제한
    pending_transactions: Option<TransactionsGuard>,  // 이전 배치 잔량
    block_status_subscribers: ...,                    // TX 상태 구독자
}
```

### 트랜잭션 흐름

```
외부 (SUI 프로토콜 / 우리는 RPC)
    ↓ TransactionClient::submit(txs)
bounded_channel (MAX 2000 TX)
    ↓ Core가 블록 제안할 때 폴링
TransactionConsumer::next()
    ├─ 최대 max_bytes, max_count까지 TX 추출
    └─ 이전 배치 잔량(pending) 우선 소진
    ↓
Block { transactions: Vec<Transaction> }
    ↓ 블록이 커밋되면
TransactionsGuard → included_in_block_ack.send(BlockRef, BlockStatus)
    ↓
외부 (TX 제출자에게 확정 알림)
```

### TransactionClient 인터페이스

```rust
pub struct TransactionClient {
    // SUI(또는 우리 RPC 레이어)가 TX를 제출하는 창구
}

// Context를 받아 (client, consumer_receiver) 쌍 생성
TransactionClient::new(context) → (TransactionClient, Receiver<TransactionsGuard>)
```

### 우리 프로젝트에서의 트랜잭션 소싱

| 항목 | SUI | 우리 프로젝트 |
|------|-----|-------------|
| TX 입력 | `TransactionClient` (SUI 내부) | JSON-RPC / P2P로 수신한 Ethereum TX |
| TX 형식 | `Transaction { data: Bytes }` | EIP-2718 인코딩된 Ethereum TX |
| 수용 한도 | 2000 TX 채널 | 동일하게 bounded channel 사용 |
| TX 확정 알림 | `BlockStatus` 채널 | `CommittedSubDag` 수신 후 영수증 발행 |

**mempool 부재의 의미**: Mysticeti는 TX 중복 제거, 수수료 기반 우선순위, TX 만료 처리를 하지 않는다. 이는 상위 레이어(우리의 RPC 게이트웨이)가 담당해야 한다.

---

## 23. 블록 제안 흐름 (Block Proposal)

**소스**: `core/src/core.rs`, `core/src/leader_timeout.rs`, `core/src/broadcaster.rs`

### 이중 트리거 메커니즘

```
트리거 A: ThresholdClock 라운드 진행 (주 경로)
    블록 수신 → Core::add_blocks()
        → DagState.accept_block() → ThresholdClock.add_block()
        → 라운드 증가 시 Core::try_signal_new_round()
        → CoreSignals::new_round(round)  ← watch 채널 업데이트
        → LeaderTimeoutTask 타이머 리셋

트리거 B: LeaderTimeout (활성 유지 / 강제 제안)
    min_round_delay 만료 → dispatcher.new_block(round, force=false)
    leader_timeout 만료  → dispatcher.new_block(round, force=true)
```

### LeaderTimeoutTask

**소스**: `core/src/leader_timeout.rs`

```rust
struct LeaderTimeoutTask<D: CoreThreadDispatcher> {
    dispatcher: Arc<D>,
    new_round_receiver: watch::Receiver<Round>,  // ThresholdClock 신호 수신
    leader_timeout: Duration,      // 최대 타임아웃 (예: 500ms)
    min_round_delay: Duration,     // 최소 대기 (예: 50ms)
    stop: Receiver<()>,
}
```

| 타이머 | 동작 | force |
|--------|------|-------|
| `min_round_delay` 만료 | 블록 제안 시도 (ancestors 부족 시 스킵 가능) | false |
| `leader_timeout` 만료 | 블록 제안 강제 (ancestors 부족해도 제안) | true |
| 새 라운드 신호 | 두 타이머 모두 리셋 | — |

**역할**: 네트워크 지연이 있어도 라운드가 멈추지 않도록 보장하는 활성 유지(liveness) 메커니즘.

### 블록 빌드 호출 체인

```
Core::new_block(round, force)
    → Core::try_propose(force)
        → Core::try_new_block(force)    ← 실제 블록 구성
```

### `try_new_block` — 블록 구성 로직

```
try_new_block(force):
    clock_round = dag_state.current_round()

    // 1. Ancestors 선택
    (ancestors, excluded) = smart_ancestors_to_propose(clock_round, !force)
    // !force: quality 기준 미달 시 제안 보류 가능
    // force: 무조건 제안 (leader_timeout 경우)

    // 2. TX 포함
    (transactions, ack_cb, _) = transaction_consumer.next()
    // max_bytes, max_count 한도 내에서 최대한 포함
    // 이전 배치 잔량(pending) 먼저 소진

    // 3. CommitVotes 포함
    commit_votes = dag_state.take_commit_votes(MAX=100)

    // 4. 블록 생성
    block = Block::V1(BlockV1 {
        epoch, round: clock_round, author: own_index,
        timestamp_ms: clock.now(),
        ancestors,
        transactions,
        commit_votes,
    })

    // 5. 서명 → VerifiedBlock
    signed = SignedBlock::new(block, keypair)
    verified = VerifiedBlock::new_verified(signed, serialized)

    // 6. DAG에 자기 블록 즉시 수락
    dag_state.accept_block(verified.clone())

    // 7. 브로드캐스트 신호
    signals.new_block(ExtendedBlock { block: verified, excluded })
```

### Ancestors 선택 전략 (`smart_ancestors_to_propose`)

```
// 기본: 마지막으로 포함한 것 이후의 블록들만 포함 (last_included_ancestors 워터마크)
// 품질 점수(ancestor quality) 기반 선택:
//   - 충분한 수의 이전 라운드 블록을 참조한 블록만 포함
//   - AncestorStateManager가 품질 점수 추적
// force=true 시: 품질 기준 무시, 가능한 모든 ancestors 포함
```

### 브로드캐스트 흐름

```
CoreSignals::new_block(extended_block)
    → tx_block_broadcast (broadcast 채널, capacity 비차단)
        → Broadcaster (peer마다 태스크 1개)
            → ValidatorNetworkClient::send_block(peer, block, timeout)
            → 실패 시 지수 백오프로 재시도
```

**ExtendedBlock**: `VerifiedBlock` + `excluded_ancestors` (품질 미달로 제외된 ancestors 정보). 메트릭용.

---

## 24. Store Trait

**소스**: `core/src/storage/mod.rs`

### Store trait 전체 메서드

```rust
pub trait Store: Send + Sync {
    // --- 쓰기 ---
    fn write(&self, write_batch: WriteBatch) -> ConsensusResult<()>;

    // --- 블록 조회 ---
    fn read_blocks(&self, refs: &[BlockRef])
        -> ConsensusResult<Vec<Option<VerifiedBlock>>>;
    fn contains_blocks(&self, refs: &[BlockRef])
        -> ConsensusResult<Vec<bool>>;
    fn scan_blocks_by_author(
        &self, authority: AuthorityIndex, start_round: Round,
    ) -> ConsensusResult<Vec<VerifiedBlock>>;
    fn scan_last_blocks_by_author(
        &self, author: AuthorityIndex, num_of_rounds: u64, before_round: Option<Round>,
    ) -> ConsensusResult<Vec<VerifiedBlock>>;

    // --- 커밋 조회 ---
    fn read_last_commit(&self) -> ConsensusResult<Option<TrustedCommit>>;
    fn scan_commits(&self, range: CommitRange)
        -> ConsensusResult<Vec<TrustedCommit>>;

    // --- SUI 전용 (우리 불필요) ---
    fn read_commit_votes(&self, commit_index: CommitIndex)
        -> ConsensusResult<Vec<BlockRef>>;
    fn read_last_commit_info(&self)
        -> ConsensusResult<Option<(CommitRef, CommitInfo)>>;
    fn read_last_finalized_commit(&self)
        -> ConsensusResult<Option<CommitRef>>;
    fn read_rejected_transactions(&self, commit_ref: CommitRef)
        -> ConsensusResult<Option<BTreeMap<BlockRef, Vec<TransactionIndex>>>>;
}
```

### WriteBatch

```rust
pub struct WriteBatch {
    pub blocks:    Vec<VerifiedBlock>,                                     // 필수
    pub commits:   Vec<TrustedCommit>,                                     // 필수
    pub commit_info: Vec<(CommitRef, CommitInfo)>,                         // SUI 전용
    pub finalized_commits: Vec<(CommitRef, BTreeMap<BlockRef, Vec<TransactionIndex>>)>, // SUI 전용
}
```

### 구현체

| 구현 | 위치 | 용도 |
|------|------|------|
| `MemStore` | `storage/mem_store.rs` | 테스트용 in-memory (BTreeMap 기반) |
| `RocksDBStore` | `storage/rocksdb_store.rs` | 프로덕션 영속화 |

### 우리 프로젝트에서의 Store 구현 계획

**Phase 3 (합의 추출)**: `MemStore` 그대로 재사용 — 테스트에 충분.

**Phase 5 이후 (프로덕션)**:
- SUI-전용 메서드 제거 (`read_commit_votes`, `read_rejected_transactions`, etc.)
- `WriteBatch`에서 SUI 전용 필드 제거
- 우리 사용에 맞는 경량 `WriteBatch` 재정의

```
우리의 최소 WriteBatch:
    blocks:  Vec<VerifiedBlock>   // 필수
    commits: Vec<TrustedCommit>   // 필수
```

---

## 25. 필수 vs 선택 컴포넌트 (최소 추출 기준)

**소스**: 분석 전체 종합

### 필수 컴포넌트 (최소 합의 동작)

| 컴포넌트 | 역할 | 비고 |
|---------|------|------|
| `Core` | 합의 오케스트레이터 | 핵심 |
| `DagState` | DAG + 커밋 상태 | 핵심 |
| `Store` (MemStore) | 영속화 | Phase 3는 MemStore 충분 |
| `BlockManager` | 조상 의존성 관리 | 핵심 |
| `LeaderTimeoutTask` | 활성 유지(liveness) | 없으면 네트워크 멈춤 |
| `TransactionConsumer` | 블록 TX 포함 | 핵심 |
| `CoreSignals` + `Broadcaster` | 블록 브로드캐스트 | 핵심 |
| `RoundTracker` | ancestors 품질 추적 | Core 내부 의존 |
| `ValidatorNetworkClient` (mock) | 네트워크 추상화 | Phase 3: mock |

### 선택/제거 가능 컴포넌트

| 컴포넌트 | 제거 여부 | 이유 |
|---------|---------|------|
| `CommitSyncer` | **제거 가능** | 기본 합의 불필요. 노드 낙오 복구용. Phase 3 이후 고려 |
| `RoundProber` | **단순화 가능** | 전파 지연 측정용. 없어도 합의 동작. 백프레셔 기능만 남길 수 있음 |
| `RocksDBStore` | **나중에** | Phase 3: MemStore, Phase 5+: 교체 |
| `TransactionCertifier` | **제거** | SUI fast-path 전용 |
| `CommitFinalizer` | **제거** | SUI fast-path 전용 |
| `AncestorStateManager` | **단순화** | 평판 기반 리더 선출 미사용 시 불필요 |
| `LeaderSchedule` (점수 기반) | **교체** | 라운드로빈 또는 자체 방식으로 대체 |

### 전체 아키텍처 (AuthorityNode 기준)

```
ConsensusAuthority::start(committee, keypair, ...)
    │
    └─ AuthorityNode::start()
           ├─ Context ──────────────────── epoch, own_index, committee, metrics
           ├─ DagState + Store ──────────── DAG + 영속화
           ├─ TransactionClient ──────────── TX 입력 채널
           ├─ Core (CoreThread) ─────────── 합의 엔진
           │    ├─ BlockManager
           │    ├─ UniversalCommitter
           │    ├─ CommitObserver → Linearizer
           │    ├─ LeaderSchedule    [교체 필요]
           │    ├─ RoundTracker
           │    └─ AncestorStateManager [단순화]
           ├─ LeaderTimeoutTask ─────────── liveness 타이머
           ├─ Broadcaster ───────────────── 블록 브로드캐스트
           ├─ Synchronizer ─────────────── 누락 블록 동기화
           ├─ CommitSyncer ─────────────── [제거 가능] 낙오 복구
           ├─ RoundProber ──────────────── [단순화] 전파 지연 측정
           └─ NetworkManager (gRPC) ──────── [테스트: mock]
```

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
