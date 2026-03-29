# Phase 2: Shadow State 구현

**상태**: ⏳ 대기
**목표**: REVM의 `Database` trait을 구현하는 Multi-Version Shadow Memory를 완성한다. 합의/스케줄러 없이 독립적으로 테스트 가능해야 한다.

---

## 작업 목록

### 핵심 구조체 구현 (`crates/shadow-state`)
- [ ] `ShadowDb` 구조체 — round별 격리된 shadow 저장소
- [ ] `VersionedSlot` — 계정/스토리지 슬롯의 다중 버전 관리
- [ ] `ReadSet` / `WriteSet` — 트랜잭션별 접근 추적
- [ ] REVM `Database` trait 구현 (`basic`, `code_by_hash`, `storage`, `block_hash`)

### 충돌 감지 로직
- [ ] 읽기-쓰기(RW) 충돌 감지 (`tx_A reads slot X, tx_B writes slot X`)
- [ ] 쓰기-쓰기(WW) 충돌 감지
- [ ] 충돌 발생 시 재실행 대상 트랜잭션 식별 로직

### 확정/폐기 로직
- [ ] `commit_round(round_id)` — Shadow Diff를 메인 DB에 병합
- [ ] `discard_round(round_id)` — Shadow 전체 폐기 (O(1) Drop)

---

## 완료 기준 (Done Criteria)

1. `cargo test -p shadow-state` 가 모두 통과한다.
2. 충돌 감지 테스트: 동일 슬롯에 쓰는 두 tx가 충돌로 식별된다.
3. 폐기 테스트: `discard_round` 후 메인 DB 상태가 변경되지 않는다.
4. 확정 테스트: `commit_round` 후 메인 DB에 Diff가 정확히 반영된다.

---

## 테스트 기준

```
cargo test -p shadow-state
```

- [ ] `test_rw_conflict_detection` — RW 충돌 정상 감지
- [ ] `test_ww_conflict_detection` — WW 충돌 정상 감지
- [ ] `test_commit_applies_diff` — commit 후 메인 DB 반영 확인
- [ ] `test_discard_no_side_effects` — discard 후 메인 DB 불변 확인
- [ ] `test_multi_round_isolation` — 서로 다른 round의 shadow가 격리됨
