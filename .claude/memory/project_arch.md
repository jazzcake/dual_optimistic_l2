---
name: project_arch
description: 이중 낙관적 파이프라인 프로젝트의 핵심 아키텍처, 목표, 구성요소 요약
type: project
---

Mysticeti DAG 합의(SUI fork)와 REVM 병렬 실행을 결합한 이중 낙관적 L2 파이프라인 구현 프로젝트.

**핵심 성능 보장**: 체감 완료 시간 `max(3Δ, 2Δ+E)` — 기존 `3Δ+E` 대비 항상 `min(Δ, E)` 단축.

**4대 컴포넌트**:
1. `consensus` — Mysticeti DAG, 2Δ SoftCommit / 3Δ HardCommit 신호 발생
2. `scheduler` — 비동기 신호 중재, 라운드 순서 보장, Backpressure 제어
3. `shadow-state` — Multi-Version Shadow Memory, REVM `Database` trait 구현
4. `parallel-evm` + `node` — 병렬 실행 및 전체 통합

**외부 의존성**:
- `extern/sui` → `jazzcake/sui` fork (submodule) — Mysticeti 추출 대상
- `extern/revm` → `jazzcake/revm` fork (submodule) — EVM 실행 엔진

**Why:** 허가형(Permissioned) 환경에서 비잔틴 노드 확률 ≈ 0이므로, 이론적 최악 케이스 없이 최대 성능 달성 가능.

**How to apply:** 아키텍처 관련 결정 시 항상 `min(Δ, E)` 이득 보존 여부를 기준으로 판단. 상세 이론은 `docs/architecture.md` 참조.
