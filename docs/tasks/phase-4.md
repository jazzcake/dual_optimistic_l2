# Phase 4: 낙관적 파이프라인 스케줄러 구현

**상태**: ⏳ 대기
**목표**: 합의 모듈과 실행 모듈 사이의 비동기 신호 중재자를 구현한다. 라운드 순서 보장과 Backpressure 제어가 핵심이다.

---

## 작업 목록

### 핵심 구조체 구현 (`crates/scheduler`)
- [ ] `PipelineScheduler` 구조체
- [ ] 실행 대기열 (`BTreeMap<RoundIndex, TxBatch>`) — 라운드 순서 보장
- [ ] SoftCommit 수신 핸들러 → 대기열 적재
- [ ] HardCommit 수신 핸들러 → commit / discard 결정 후 래퍼에 전달

### Backpressure 제어
- [ ] 대기열 임계치(threshold) 설정
- [ ] 임계치 초과 시 합의 엔진으로 억제 신호 발송
- [ ] 억제 해제 조건 구현

### 순서 역전 처리
- [ ] 패킷 순서 역전(out-of-order) 발생 시 재정렬 로직
- [ ] 라운드 인덱스 기반 전체 순서 정렬 수학적 보장

---

## 실행 계획 (Execution Plan)

> 이 섹션은 Phase 시작 전 사용자와 함께 수립하고 승인받은 후 채운다.

---

## 완료 기준 (Done Criteria)

1. `cargo test -p scheduler` 가 모두 통과한다.
2. 순서가 역전된 SoftCommit이 들어와도 실행 순서가 라운드 인덱스 기준으로 보장된다.
3. 대기열이 임계치 초과 시 Backpressure 신호가 발송된다.

---

## 테스트 기준

```
cargo test -p scheduler
```

- [ ] `test_in_order_processing` — 순서대로 들어온 SoftCommit 정상 처리
- [ ] `test_out_of_order_reorder` — 역전된 SoftCommit이 올바른 순서로 실행됨
- [ ] `test_backpressure_triggered` — 대기열 임계치 초과 시 억제 신호 발송
- [ ] `test_backpressure_release` — 대기열 해소 시 억제 신호 해제
- [ ] `test_hard_commit_match` — HardCommit이 SoftCommit과 일치 시 commit 호출
- [ ] `test_hard_commit_mismatch` — HardCommit 불일치 시 discard 호출
