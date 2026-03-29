# 전체 업무 목록 (Master Task List)

## 현재 Phase: **Phase 2 완료 → Phase 3 대기**

| Phase | 제목 | 상태 | 문서 |
|-------|------|------|------|
| 0 | 환경 구성 & 코드베이스 분석 | ✅ 완료 | [phase-0.md](phase-0.md) |
| 1 | 인터페이스 설계 | ✅ 완료 | [phase-1.md](phase-1.md) |
| 2 | Shadow State 구현 | ✅ 완료 | [phase-2.md](phase-2.md) |
| 3 | Mysticeti 합의 추출 | ⏳ 대기 | [phase-3.md](phase-3.md) |
| 4 | 낙관적 파이프라인 스케줄러 구현 | ⏳ 대기 | [phase-4.md](phase-4.md) |
| 5 | 통합 & 벤치마크 | ⏳ 대기 | [phase-5.md](phase-5.md) |

---

## 최종 목표

SUI의 Mysticeti DAG 합의 엔진과 REVM 기반 병렬 EVM 실행 엔진을 결합한
**이중 낙관적 파이프라인 (Dual Optimistic Pipeline)** 을 구현한다.

- 합의 레이어: Mysticeti 3단계 파이프라인 (1Δ → 2Δ → 3Δ)
- 실행 레이어: Multi-Version Shadow Memory 기반 병렬 REVM
- 스케줄러: 2Δ SoftCommit → 선행 실행, 3Δ HardCommit → 확정/폐기
- 성능 보장: 체감 완료 시간 `max(3Δ, 2Δ+E)` < 기존 `3Δ+E`

---

## 아키텍처 참조

전체 이론 및 설계 근거: [`docs/architecture.md`](../architecture.md)
