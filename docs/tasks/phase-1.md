# Phase 1: 인터페이스 설계

**상태**: ⏳ 대기
**목표**: 코드를 작성하기 전에 모든 컴포넌트 간의 경계면(API/타입)을 문서로 확정한다. 구현은 하지 않는다.

---

## 작업 목록

### 핵심 타입 정의
- [ ] `ConsensusEvent` 열거형 설계 (`SoftCommit { round, block }`, `HardCommit { round, block }`)
- [ ] `TxBatch` 타입 설계 (스케줄러 → 실행 엔진 전달 단위)
- [ ] `StateDiff` 타입 설계 (Shadow Memory → 메인 원장 머지 단위)
- [ ] `ExecutionResult` 타입 설계 (성공/실패, 가스 사용량 등)

### 컴포넌트 인터페이스 설계
- [ ] `ConsensusModule` trait (합의 모듈이 외부에 노출하는 인터페이스)
- [ ] `PipelineScheduler` trait (스케줄러 인터페이스)
- [ ] `ParallelExecutor` trait (병렬 실행 엔진 인터페이스)
- [ ] `ShadowDatabase` trait (REVM `Database` 확장 인터페이스)
- [ ] `CommitWrapper` trait (확정/롤백 래퍼 인터페이스)

### 비동기 채널 설계
- [ ] 합의 → 스케줄러 채널 타입 결정 (tokio mpsc / broadcast)
- [ ] 스케줄러 → 실행 엔진 채널 타입 결정
- [ ] 실행 엔진 → 래퍼 채널 타입 결정
- [ ] Backpressure 신호 채널 방향 및 타입 결정

### 테스트 하네스 인터페이스 설계
- [ ] `SimulatedNetwork` trait 설계 — 노드 간 메시지 전달 추상화 (지연 주입 포함)
- [ ] `SimulatedNode` trait 설계 — in-process 노드 추상화
- [ ] 결정론적 시뮬레이터용 인터페이스 확정 (Phase 0 전략 문서 기반)
- [ ] 벤치마크용 멀티스레드 환경 인터페이스 확정 (Δ 실제 측정 방법 포함)

### 문서화
- [ ] `docs/interfaces.md` — 모든 trait 및 타입 명세 작성 (테스트 하네스 포함)
- [ ] 컴포넌트 상호작용 다이어그램 (ASCII)

---

## 실행 계획 (Execution Plan)

> 이 섹션은 Phase 시작 전 사용자와 함께 수립하고 승인받은 후 채운다.

---

## 완료 기준 (Done Criteria)

1. 5개 컴포넌트 간의 모든 인터페이스가 Rust trait/struct 수준으로 명세되어 있다.
2. `crates/` 의 각 크레이트가 어떤 trait을 구현하는지 명확하다.
3. 채널 방향과 타입이 확정되어 있다.
4. 테스트 하네스 인터페이스가 설계되어 있다 — 결정론적 시뮬레이터(정확성)와 멀티스레드 환경(벤치마크) 양쪽 모두.
5. 이 설계를 기반으로 Phase 2~4를 독립적으로 병렬 진행할 수 있다.

---

## 테스트 기준

- [ ] `docs/interfaces.md`에 정의된 모든 trait을 `crates/` 각 `lib.rs`에 stub으로 선언했을 때 `cargo check` 통과
- [ ] 순환 의존성 없음 (`consensus` → `scheduler` → `executor` 단방향)
