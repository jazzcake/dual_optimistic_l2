# CLAUDE.md — 프로젝트 규칙 및 문서 관리

## 문서 읽기 정책

### 항상 읽을 문서 (매 대화 시작 시)
| 문서 | 목적 |
|------|------|
| `CLAUDE.md` | 이 문서. 규칙 및 문서 인덱스 |
| `docs/tasks/TASKS.md` | 전체 Phase 목록 및 현재 진행 상황 |
| `.claude/memory/MEMORY.md` | 프로젝트 메모리 인덱스 |

### 필요시 읽을 문서
| 문서 | 언제 읽나 |
|------|-----------|
| `docs/tasks/phase-N.md` | 해당 Phase 작업 시작 전 |
| `docs/architecture.md` | 아키텍처 관련 결정이 필요할 때 |
| `.claude/memory/*.md` | MEMORY.md 인덱스에서 관련 항목이 있을 때 |
| `crates/*/src/` | 해당 크레이트 구현 작업 시 |

### 읽지 않아도 되는 문서
| 문서 | 이유 |
|------|------|
| `extern/sui/**` | 방대한 외부 저장소. 필요 시 Grep/Glob으로 검색 접근 |
| `extern/revm/**` | 방대한 외부 저장소. 필요 시 Grep/Glob으로 검색 접근 |
| `Cargo.lock` | 자동 생성 파일 |

---

## 메모리 관리 규칙

- 모든 프로젝트 메모리는 **`.claude/memory/`** 에 저장 (git으로 관리됨)
- 전역 메모리 위치(`~/.claude/projects/.../memory/`)는 사용하지 않음
- 새 메모리 저장 시 `.claude/memory/MEMORY.md` 인덱스도 함께 업데이트
- 메모리 파일 형식은 기존 auto-memory 규칙을 따름 (frontmatter 포함)

---

## 커뮤니케이션 규칙

- 대화는 **한국어**로
- 코드 주석은 **영어**로
- 커밋 메시지는 **영어**로

---

## 코딩 규칙

- Rust edition 2021, workspace resolver = "2"
- 각 crate는 독립적으로 테스트 가능해야 함 (`cargo test -p <crate>`)
- `unsafe` 코드는 `// SAFETY:` 주석 필수
- 외부 저장소(`extern/`) 코드는 직접 수정하지 않음. 필요 시 해당 fork에서 수정 후 submodule 업데이트

### 외부 코드 재사용 규칙 (Apache 2.0 출처 표기)

`extern/` 의 Apache 2.0 코드를 `crates/` 로 이식할 때는 반드시 출처를 명시한다.

```rust
// Adapted from: sui/consensus/core/src/base_committer.rs (lines 123-200)
// Original copyright: Copyright (c) Mysten Labs, Inc.
// License: Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
// Changes: removed score-based LeaderSchedule, replaced with round-robin
```

- 파일 단위로 이식하는 경우: 파일 최상단에 표기
- 함수/구조체 단위인 경우: 해당 항목 바로 위에 표기
- 수정 내용이 있으면 `Changes:` 항목에 구체적으로 기술
- 재구현(재작성)이 아닌 한 반드시 이 규칙을 따른다

### Design by Contract (사전조건 assert)

모든 공개(pub) 함수의 진입부에 인자 및 객체 상태에 대한 사전조건을 명시한다.

```rust
pub fn add_block(&mut self, block_ref: BlockRef, committee: &Committee) -> bool {
    debug_assert!((block_ref.author as usize) < committee.size(),
        "author index {} out of committee size {}", block_ref.author, committee.size());
    debug_assert!(block_ref.round > 0,
        "round must be positive (use genesis API for round 0)");
    // ...
}
```

| 종류 | 사용 시점 |
|------|---------|
| `debug_assert!` | 함수 인자·객체 상태의 사전조건 — debug 빌드에서만 검사, release에서 제거 |
| `assert!` | 절대 위반 불가 불변식 (invariant) — 항상 검사 |

- TDD 테스트: "올바른 경로"가 기대 결과를 내는지 검증
- DbC assert: "잘못된 입력으로 호출하는 버그"를 즉시 탐지
- 두 가지는 상호 보완 관계이며 함께 사용한다

---

## Phase 진행 규칙

Phase는 아래 5단계를 반드시 순서대로 거친다.

### 1단계: 문서 숙지
- Phase 시작 전 해당 `docs/tasks/phase-N.md`를 읽을 것

### 2단계: 계획 수립 (필수)
- Phase 작업 시작 전 **반드시** 실행 계획을 수립한다
- 계획 수립 방식은 사용자에게 먼저 질문한다:
  **"계획을 직접 잡으시겠습니까, 아니면 제가 초안을 드릴까요?"**
- 확정된 계획은 해당 phase 문서의 `## 실행 계획` 섹션에 기록한다

### 3단계: 계획 승인 (필수)
- 수립된 계획을 사용자에게 제시하고 **명시적 승인**을 받은 후에만 실행을 시작한다
- 승인 없이 코드 작성이나 파일 수정을 시작하지 않는다

### 4단계: 실행
- Phase 문서의 체크리스트를 실시간으로 업데이트하며 진행
- `TASKS.md`의 현재 Phase 상태를 항상 최신으로 유지

### 5단계: 완료 승인 (필수)
- 완료 기준(Done Criteria)을 충족했을 때 사용자에게 보고하고 **명시적 승인**을 받는다
- 승인 없이 다음 Phase로 진행하지 않는다
