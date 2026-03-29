# Phase 3: Mysticeti 합의 추출

**상태**: ⏳ 대기
**목표**: SUI 저장소에서 Mysticeti DAG 합의에 필요한 최소한의 코드를 추출하여 `crates/consensus`에 이식한다. SUI 전용 의존성 없이 독립 컴파일되어야 한다.

---

## 작업 목록

### 코드 추출
- [ ] Phase 0 분석 결과를 기반으로 추출 대상 파일 목록 확정
- [ ] `crates/consensus`에 Mysticeti 핵심 코드 이식
- [ ] SUI 전용 타입(`sui-types` 등) → 프로젝트 내부 타입으로 교체
- [ ] SUI 전용 네트워크 레이어 제거 또는 추상화

### 신호 발생 구현
- [ ] 2Δ SoftCommit 이벤트 발생 로직 연결
- [ ] 3Δ HardCommit 이벤트 발생 로직 연결
- [ ] `ConsensusEvent` 채널 전송 구현

### 의존성 정리
- [ ] `crates/consensus/Cargo.toml` — 외부 의존성 최소화
- [ ] `extern/sui` path 의존성 사용 여부 결정 (이식 vs 참조)

---

## 완료 기준 (Done Criteria)

1. `cargo build -p consensus` 가 `extern/sui` 없이 통과한다. (이식 방식 선택 시)
2. 또는 `extern/sui` path 의존성만으로 빌드된다. (참조 방식 선택 시)
3. SoftCommit / HardCommit 이벤트가 테스트 환경에서 정상 발생한다.

---

## 테스트 기준

```
cargo test -p consensus
```

- [ ] `test_soft_commit_triggered` — 2f+1 쿼럼 형성 시 SoftCommit 이벤트 발생
- [ ] `test_hard_commit_triggered` — 라운드 앵커 확정 시 HardCommit 이벤트 발생
- [ ] `test_dag_causal_order` — DAG에서 인과 순서가 보존됨
- [ ] `test_byzantine_node_tolerance` — f개 비잔틴 노드에서도 쿼럼 형성
