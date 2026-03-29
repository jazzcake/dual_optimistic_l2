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

---

## Phase 진행 규칙

- Phase 시작 전 해당 `docs/tasks/phase-N.md`를 읽을 것
- 각 Phase의 **완료 기준(Done Criteria)**을 충족해야 다음 Phase로 진행
- Phase 문서의 체크리스트를 실시간으로 업데이트하며 진행
- `TASKS.md`의 현재 Phase 상태를 항상 최신으로 유지
