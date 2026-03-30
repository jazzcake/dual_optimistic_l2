---
name: project_arch
description: 이중 낙관적 파이프라인 프로젝트의 핵심 아키텍처, 목표, 구성요소 요약
type: project
---

Mysticeti DAG 합의(SUI fork)와 REVM 병렬 실행을 결합한 이중 낙관적 L2 파이프라인 구현 프로젝트.

**핵심 성능 보장**: 체감 완료 시간 `max(3Δ, 2Δ+E)` — 기존 `3Δ+E` 대비 항상 `min(Δ, E)` 단축.

**4대 컴포넌트**:
1. `consensus` — Mysticeti DAG, 2Δ SoftCommit / 3Δ HardCommit 신호 발생 (Phase 3 완료, 8/8 테스트)
2. `scheduler` — 비동기 신호 중재, 라운드 순서 보장, Backpressure 제어 (Phase 4 완료, 6/6 테스트)
3. `shadow-state` — Multi-Version Shadow Memory, REVM `Database` trait 구현 (Phase 2 완료)
4. `parallel-evm` + `node` + `testkit` — E2E 통합, 벤치마크 검증 (Phase 5 완료)

**Phase 5 완료 내용**:
- `dag_state::get_causal_blocks()` — read-only causal DFS로 SoftCommit tx 수집
- `check_soft_commit()` / `to_shared_subdag()` — 실제 tx 페이로드 배선 완료
- `MockExecutor` — TxBatch 즉시 수신, delay/backpressure 테스트 지원
- E2E 통합 테스트 6개 (`node` crate): single/multi-round, tx-match, out-of-order, backpressure, byzantine-f1
- 벤치마크 3개 (`testkit` crate): bench_baseline, bench_optimistic, bench_conflict_sweep
- `docs/benchmark-results.md` 작성 완료
- `tokio::time::Instant` 사용으로 `tokio::time::pause()` 기반 결정론적 벤치마크 구현

**외부 의존성**:
- `extern/sui` → `jazzcake/sui` fork (submodule) — Mysticeti 추출 대상
- `extern/revm` → `jazzcake/revm` fork (submodule) — EVM 실행 엔진

**Why:** 허가형(Permissioned) 환경에서 비잔틴 노드 확률 ≈ 0이므로, 이론적 최악 케이스 없이 최대 성능 달성 가능.

**How to apply:** 아키텍처 관련 결정 시 항상 `min(Δ, E)` 이득 보존 여부를 기준으로 판단. 상세 이론은 `docs/architecture.md` 참조.
