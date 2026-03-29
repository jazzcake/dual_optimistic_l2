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

### 통합 테스트 (결정론적 시뮬레이터 기반)
- [ ] End-to-end: 트랜잭션 제출 → 합의 → 실행 → 확정 전체 흐름
- [ ] 결정론적 시뮬레이터에서 N노드 (3~4) 전체 파이프라인 검증
- [ ] 비잔틴 노드 주입 (f=1) 환경에서 정상 동작
- [ ] 순서 역전(out-of-order) 메시지 주입 시 스케줄러 재정렬 검증
- [ ] Backpressure 발동 및 해제 시나리오 검증

### 벤치마크 (멀티스레드 실환경 기반)
- [ ] 벤치마크 환경 구성: 멀티스레드 tokio 런타임, 실제 타이밍 측정
- [ ] Δ 주입 방법 구현: 채널 전송에 `tokio::time::sleep` 기반 지연 삽입
- [ ] 기준 모델 구현: 3Δ 대기 후 직렬 실행
- [ ] 제안 모델: 2Δ 낙관적 병렬 실행
- [ ] 측정 항목:
  - 체감 완료 시간 (Latency) — `max(3Δ, 2Δ+E)` vs `3Δ+E`
  - 최대 처리량 (Max TPS)
  - 충돌률별 성능 곡선 (`E_retry` 증가에 따른 상대적 우위 유지 확인)

### 문서화
- [ ] `docs/benchmark-results.md` — 벤치마크 결과 기록
- [ ] `README.md` 업데이트 — 설치/실행 가이드 완성

---

## 실행 계획 (Execution Plan)

> 이 섹션은 Phase 시작 전 사용자와 함께 수립하고 승인받은 후 채운다.

---

## 완료 기준 (Done Criteria)

1. `cargo test` (전체) 통과 — 결정론적 시뮬레이터 기반 통합 테스트 포함
2. End-to-end 통합 테스트 통과 (비잔틴, 순서 역전, Backpressure 시나리오 포함)
3. 벤치마크(멀티스레드 실환경)에서 제안 모델이 기준 모델 대비 `min(Δ, E)` 이상의 지연 시간 단축 측정
4. 충돌률 증가 시에도 제안 모델의 상대적 우위가 유지됨을 수치로 확인
5. README.md에 실행 가이드가 완성되어 있다

---

## 테스트 기준

```
cargo test
cargo bench
```

**통합 테스트 (결정론적 시뮬레이터)**
- [ ] `test_e2e_single_round` — 단일 라운드 전체 파이프라인 정상 동작
- [ ] `test_e2e_multi_round` — 연속 라운드 처리 (큐 연속성)
- [ ] `test_e2e_conflict_recovery` — 충돌 발생 후 재실행 및 정상 확정
- [ ] `test_e2e_byzantine_f1` — f=1 비잔틴 노드 주입 후 정상 확정
- [ ] `test_e2e_out_of_order` — 순서 역전 메시지 시나리오
- [ ] `test_e2e_backpressure` — 큐 과부하 시 Backpressure 발동 및 해제

**벤치마크 (멀티스레드 실환경)**
- [ ] `bench_baseline` — 기준 모델(3Δ 직렬) Latency / TPS 측정
- [ ] `bench_optimistic` — 제안 모델(2Δ 병렬) Latency / TPS 측정
- [ ] `bench_conflict_sweep` — 충돌률 0%~100% 구간별 양 모델 Latency 비교
- [ ] 결과 검증: `optimistic_latency < baseline_latency` (모든 충돌률 구간에서)
