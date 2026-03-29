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

### SUI 테스트 인프라 분석
- [ ] SUI의 합의 테스트 유틸리티 파악 (`sui-simulator`, `msim` 등)
- [ ] `msim` 결정론적 시뮬레이터 재사용 가능 여부 판단
- [ ] 재사용 불가 시 대안 시뮬레이터 후보 조사
- [ ] 테스트 전략 결정: 결정론적 시뮬레이션(정확성) vs 멀티스레드(벤치마크) 분리 방침 확정

### 분석 문서 작성
- [ ] `docs/mysticeti-analysis.md` — Mysticeti crate 맵 + 의존성 그래프
- [ ] `docs/revm-analysis.md` — REVM Database trait + 실행 흐름
- [ ] `docs/test-strategy.md` — 시뮬레이션 전략 및 선택 근거

---

## 실행 계획 (Execution Plan)

**작업 순서 원칙**: REVM → SUI Mysticeti → SUI 테스트 인프라 → Phase 1 입력 정리

REVM을 먼저 하는 이유: 독립 크레이트라 구조가 단순하고, `Database` trait 파악이
Shadow State(Phase 2) 설계의 직접 입력이 되므로 기준점을 먼저 확보한다.

---

### Step 1: REVM 분석

| 순서 | 작업 | 도구 |
|------|------|------|
| 1-1 | `extern/revm/` crate 디렉토리 구조 전체 파악 | Glob |
| 1-2 | `Database` trait 위치 및 메서드 시그니처 전체 파악 | Grep → Read |
| 1-3 | `Evm` builder 패턴 및 트랜잭션 실행 흐름 추적 | Read |
| 1-4 | 병렬 실행 시 상태 격리 가능 여부 파악 (thread-safe 여부 등) | Read |
| 1-5 | `docs/revm-analysis.md` 작성 | Write |

---

### Step 2: SUI Mysticeti 분석

| 순서 | 작업 | 도구 |
|------|------|------|
| 2-1 | `extern/sui/` 에서 consensus 관련 crate 디렉토리 탐색 | Glob |
| 2-2 | 핵심 타입 목록 작성 (Block, DAGVertex, Round, QuorumCertificate 등) | Grep → Read |
| 2-3 | 2Δ SoftCommit 발생 코드 위치 추적 | Grep → Read |
| 2-4 | 3Δ HardCommit 발생 코드 위치 추적 | Grep → Read |
| 2-5 | SUI 전용 의존성 (`sui-types`, `sui-storage` 등) 목록화 | Read Cargo.toml들 |
| 2-6 | 의존성을 "제거 가능 / 대체 필요 / 그대로 사용" 3분류 | 분석 |
| 2-7 | `docs/mysticeti-analysis.md` 작성 | Write |

---

### Step 3: SUI 테스트 인프라 분석

| 순서 | 작업 | 도구 |
|------|------|------|
| 3-1 | `sui-simulator`, `msim` 등 테스트 유틸리티 존재 여부 및 구조 파악 | Glob → Read |
| 3-2 | `msim` 재사용 가능 여부 판단 (라이선스, 외부 의존성, API 안정성) | Read |
| 3-3 | 재사용 불가 시 대안 후보 조사 (자체 구현, 타 오픈소스 등) | 분석 |
| 3-4 | 테스트 전략 확정: 결정론적 시뮬레이터(정확성) / 멀티스레드(벤치마크) / Docker(검증) 구성 방침 | 결정 |
| 3-5 | `docs/test-strategy.md` 작성 | Write |

---

### Step 4: Phase 1 입력 정리

| 순서 | 작업 |
|------|------|
| 4-1 | 세 분석 결과를 바탕으로 Phase 1에서 결정할 경계면(boundary) 목록 정리 |
| 4-2 | phase-0.md 완료 기준 전체 체크 후 사용자께 완료 보고 및 승인 요청 |

---

### 산출물
- `docs/revm-analysis.md`
- `docs/mysticeti-analysis.md`
- `docs/test-strategy.md`
- `phase-0.md` 체크리스트 완성

---

## 완료 기준 (Done Criteria)

1. SUI에서 Mysticeti 관련 crate를 정확히 식별하고, 분리에 필요한 최소 의존성 목록이 문서화되어 있다.
2. REVM의 `Database` trait을 이해하고, Shadow Memory로 구현하는 방법이 문서화되어 있다.
3. Phase 1(인터페이스 설계)에서 결정해야 할 모든 경계면(boundary)이 명확히 정의되어 있다.
4. 테스트 전략이 확정되어 있다: 정확성 검증용 시뮬레이터와 벤치마크용 멀티스레드 환경을 어떻게 구성할지 결정되어 있다.

---

## 테스트 기준

이 Phase는 구현이 없으므로 코드 테스트 대신 **문서 검증**으로 대체한다.

- [ ] Mysticeti 분석 문서에 crate 의존성 다이어그램이 포함되어 있다
- [ ] REVM 분석 문서에 `Database` trait 메서드 목록과 역할이 설명되어 있다
- [ ] `docs/test-strategy.md`에 시뮬레이션 전략과 선택 근거가 명시되어 있다
- [ ] 두 분석 문서와 테스트 전략 문서가 Phase 1 인터페이스 설계의 입력 자료로 충분하다 (상호 검토)
