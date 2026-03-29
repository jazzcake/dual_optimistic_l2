# Phase 0: 환경 구성 & 코드베이스 분석

**상태**: 🔄 진행중
**목표**: 구현을 시작하기 전에 SUI Mysticeti와 REVM의 내부 구조를 충분히 이해하고, 추출/통합 전략을 확정한다.

---

## 작업 목록

### 환경 구성
- [x] Rust 설치 (cargo 1.94.1)
- [x] 프로젝트 디렉토리 구조 생성
- [x] SUI fork (`jazzcake/sui`) → `extern/sui` submodule
- [x] REVM fork (`jazzcake/revm`) → `extern/revm` submodule
- [x] Cargo workspace 설정 (`crates/` 5개 skeleton)
- [x] 문서 구조 확립 (CLAUDE.md, TASKS.md, phase 파일들)

### SUI Mysticeti 분석
- [ ] `extern/sui` 에서 합의 관련 crate 위치 파악
- [ ] Mysticeti의 핵심 타입 목록 작성 (Block, DAG, Round, Quorum 등)
- [ ] SUI 내부 의존성 그래프 분석 (어디까지가 Mysticeti 전용인가)
- [ ] 2Δ SoftCommit / 3Δ HardCommit 신호를 발생시키는 코드 위치 파악
- [ ] 분리 시 제거해야 할 SUI 전용 의존성 목록 작성

### REVM 분석
- [ ] `extern/revm` crate 구조 파악 (revm, revm-interpreter, revm-primitives 등)
- [ ] `Database` trait 인터페이스 파악 (Shadow State 구현 진입점)
- [ ] `Evm` builder 패턴 및 트랜잭션 실행 흐름 파악
- [ ] 병렬 실행을 위한 상태 격리 방법 파악

### 분석 문서 작성
- [ ] `docs/mysticeti-analysis.md` — Mysticeti crate 맵 + 의존성 그래프
- [ ] `docs/revm-analysis.md` — REVM Database trait + 실행 흐름

---

## 완료 기준 (Done Criteria)

1. SUI에서 Mysticeti 관련 crate를 정확히 식별하고, 분리에 필요한 최소 의존성 목록이 문서화되어 있다.
2. REVM의 `Database` trait을 이해하고, Shadow Memory로 구현하는 방법이 문서화되어 있다.
3. Phase 1(인터페이스 설계)에서 결정해야 할 모든 경계면(boundary)이 명확히 정의되어 있다.

---

## 테스트 기준

이 Phase는 구현이 없으므로 코드 테스트 대신 **문서 검증**으로 대체한다.

- [ ] Mysticeti 분석 문서에 crate 의존성 다이어그램이 포함되어 있다
- [ ] REVM 분석 문서에 `Database` trait 메서드 목록과 역할이 설명되어 있다
- [ ] 두 문서가 Phase 1 인터페이스 설계의 입력 자료로 충분하다 (상호 검토)
