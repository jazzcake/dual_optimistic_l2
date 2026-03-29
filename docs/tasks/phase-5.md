# Phase 5: 통합 & 벤치마크

**상태**: ⏳ 대기
**목표**: 모든 컴포넌트를 `crates/node`에서 통합하여 완전한 파이프라인을 구성하고, 이론적 성능 이득(`min(Δ, E)`)을 실측으로 검증한다.

---

## 작업 목록

### 통합 (`crates/node`)
- [ ] 전체 컴포넌트 초기화 및 연결
- [ ] 채널 배선 (consensus → scheduler → executor → wrapper)
- [ ] 노드 시작/종료 라이프사이클 구현
- [ ] 설정 파일 구조 설계 (밸리데이터 수, 네트워크 지연 등)

### 통합 테스트
- [ ] End-to-end: 트랜잭션 제출 → 합의 → 실행 → 확정 전체 흐름
- [ ] 멀티 노드 시뮬레이션 (로컬 3~4 노드)
- [ ] 비잔틴 노드 시뮬레이션 (f=1 환경에서 정상 동작)

### 벤치마크
- [ ] 기준 모델 구현: 3Δ 대기 후 직렬 실행
- [ ] 제안 모델: 2Δ 낙관적 병렬 실행
- [ ] 측정 항목:
  - 체감 완료 시간 (Latency)
  - 최대 처리량 (Max TPS)
  - 충돌률별 성능 곡선
- [ ] 결과: `max(3Δ, 2Δ+E)` vs `3Δ+E` 수치 비교

### 문서화
- [ ] `docs/benchmark-results.md` — 벤치마크 결과 기록
- [ ] `README.md` 업데이트 — 설치/실행 가이드 완성

---

## 실행 계획 (Execution Plan)

> 이 섹션은 Phase 시작 전 사용자와 함께 수립하고 승인받은 후 채운다.

---

## 완료 기준 (Done Criteria)

1. `cargo test` (전체) 통과
2. End-to-end 통합 테스트 통과
3. 벤치마크에서 제안 모델이 기준 모델 대비 `min(Δ, E)` 이상의 지연 시간 단축 측정
4. README.md에 실행 가이드가 완성되어 있다

---

## 테스트 기준

```
cargo test
cargo bench
```

- [ ] `test_e2e_single_round` — 단일 라운드 전체 파이프라인 정상 동작
- [ ] `test_e2e_multi_round` — 연속 라운드 처리 (큐 연속성)
- [ ] `test_e2e_conflict_recovery` — 충돌 발생 후 재실행 및 정상 확정
- [ ] `bench_baseline` — 기준 모델(3Δ 직렬) TPS 측정
- [ ] `bench_optimistic` — 제안 모델(2Δ 병렬) TPS 측정
- [ ] 결과 검증: `optimistic_latency < baseline_latency`
