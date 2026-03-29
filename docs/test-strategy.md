# 테스트 전략 문서

**목적**: Phase 0 분석을 바탕으로 테스트/검증 환경 전략을 확정한다.
**결정 일자**: 2026-03-29
**관련 문서**: `docs/mysticeti-analysis.md §15`, `docs/tasks/phase-5.md`

---

## 1. 요구사항 정리

테스트 환경이 충족해야 할 목적:

| 목적 | 요구사항 |
|------|---------|
| **정확성 검증** | 결정론적 재현 (동일 시드 → 동일 결과), 인프라 불필요 |
| **성능 측정** | 실제 타이밍, 멀티스레드, `min(Δ, E)` 이득 측정 |
| **통합 검증** | 실제 네트워크 토폴로지, 노드 장애 시나리오 |
| **컨트랙트 테스트** | EVM 실행, Solidity 컨트랙트 배포/호출 |

---

## 2. 3계층 테스트 전략

### 계층 1: 결정론적 인-프로세스 시뮬레이터 (정확성)

**목적**: 합의 정확성 및 Shadow State R/W 충돌 로직 검증
**사용 단계**: Phase 3 (합의 추출), Phase 4 (스케줄러), Phase 5 (통합)
**대상 테스트**: 모든 `#[test]` 및 `#[tokio::test]`

#### 구현 방법

msim은 SUI 전용으로 직접 재사용이 불가하다 (§15 참조). 자체 in-process 시뮬레이터를 구현한다.

```
구성 요소:
  SimulatedNetwork   — 채널 기반 메시지 라우팅 (tokio mpsc/broadcast)
  SimulatedNode      — 단일 프로세스 내 노드 인스턴스
  FakeClock          — tokio::time::pause() 기반 시간 제어
  LatencyModel       — 메시지 지연 주입 (uniform/bimodal)
  PartitionModel     — 네트워크 파티션 시뮬레이션
```

#### 핵심 설계 결정

- **단일 프로세스**: 모든 노드가 같은 프로세스에서 실행 → 공유 메모리로 메시지 전달
- **tokio::time::pause()**: 가상 시간으로 Δ 주입 → `sleep(Δ)` = 즉시 리턴 가능
- **결정론적 스케줄링**: tokio는 deterministic이 아니지만, 메시지 순서를 큐로 제어하면 충분
- **`turmoil` 검토**: tokio의 결정론적 네트워크 시뮬레이션 라이브러리. Phase 3 시작 전 평가.

```toml
# crates/consensus/Cargo.toml (dev-dependencies)
[dev-dependencies]
tokio = { version = "1", features = ["full", "test-util"] }
# turmoil = "0.6"   ← 평가 후 결정
```

#### msim 대체 결론

| 옵션 | 장점 | 단점 | 결정 |
|------|------|------|------|
| msim 직접 재사용 | SUI 호환성 | SUI 전체 의존성 필요 | ❌ |
| turmoil | 결정론적 보장, 활발한 유지보수 | API 학습 필요 | 🔍 Phase 3 평가 |
| 자체 구현 (tokio channels) | 완전 제어, 의존성 없음 | 구현 비용 | ✅ 1차 선택 |

**확정**: tokio test-util 기반 자체 in-process 시뮬레이터를 먼저 구현하고, 결정론성이 부족할 경우 turmoil로 전환한다.

---

### 계층 2: 멀티스레드 실환경 벤치마크 (성능)

**목적**: `min(Δ, E)` 이득 실측, TPS 측정, 충돌률별 성능 곡선
**사용 단계**: Phase 5
**도구**: `criterion`, `tokio::time::sleep` 기반 Δ 주입

#### 구성

```
tokio::Runtime (multi-thread)
    ├── N개 노드 (tokio 태스크로 격리)
    ├── 채널: tokio::sync::mpsc (실제 큐잉)
    ├── Δ 주입: tokio::time::sleep(Δ)  ← 실제 시간 소비
    └── 측정: Instant::now() 기반 E2E 지연 측정
```

#### 측정 모델

| 모델 | 동작 | 예상 지연 |
|------|------|-----------|
| 기준 모델 (baseline) | 3Δ 대기 후 직렬 실행 | `3Δ + E` |
| 제안 모델 (optimistic) | 2Δ에 낙관적 병렬 실행 | `max(3Δ, 2Δ + E) = min(3Δ, 2Δ + E) = 2Δ + E` |
| 충돌 시 | 재실행 포함 | `2Δ + E + E_retry` |

**이득 조건**: `2Δ + E < 3Δ` → `E < Δ`. 즉 EVM 실행 시간이 Δ보다 짧을 때.

#### 벤치마크 시나리오

```
bench_baseline         — 기준 모델 Latency/TPS
bench_optimistic       — 제안 모델 Latency/TPS
bench_conflict_0pct    — 충돌률 0%
bench_conflict_25pct   — 충돌률 25%
bench_conflict_50pct   — 충돌률 50%
bench_conflict_100pct  — 충돌률 100% (모든 TX가 같은 슬롯 쓰기)
```

---

### 계층 3: Docker 멀티노드 검증 (인수 테스트)

**목적**: 실제 네트워크 환경에서 4노드 클러스터 동작 검증, 장애 내성 확인
**사용 단계**: Phase 5
**도구**: Docker Compose, toxiproxy (네트워크 지연/파티션), Foundry (컨트랙트)

#### 클러스터 구성

```yaml
# docker/compose.yml 기본 구조
services:
  node0: { image: dual-optimistic-l2, ports: [...] }
  node1: { image: dual-optimistic-l2 }
  node2: { image: dual-optimistic-l2 }
  node3: { image: dual-optimistic-l2 }
  # f=1: 노드 1개 중단 시에도 합의 지속 (quorum = 3/4)
```

#### 장애 시나리오

| 시나리오 | 방법 | 기대 결과 |
|---------|------|-----------|
| 노드 1개 중단 | `docker stop node3` | 나머지 3노드에서 합의 지속 |
| 네트워크 지연 | `toxiproxy latency` | 지연 증가하나 합의 완료 |
| 네트워크 파티션 | `toxiproxy timeout` | 파티션 해소 후 재동기화 |
| 비잔틴 메시지 | 테스트 노드 코드 주입 | 정상 노드 쿼럼으로 차단 |

---

### 계층 4: Foundry 컨트랙트 테스트

**목적**: Docker 테스트넷에서 EVM 컨트랙트 배포/실행 검증
**사용 단계**: Phase 5
**도구**: Foundry (`forge`, `cast`, `anvil`)

```
contracts/
├── src/
│   ├── ConflictTest.sol   — 동일 슬롯 쓰기 (충돌 감지 검증)
│   └── BasicState.sol     — 단순 상태 읽기/쓰기
└── test/
    └── BasicState.t.sol   — forge test 단위 테스트
```

**검증 흐름**:
```
forge test             → 단위 테스트 (로컬 anvil)
cast send              → Docker 테스트넷에 실제 TX 전송
cast call              → 상태 확인
```

---

## 3. 테스트 계층 요약

| 계층 | 환경 | 결정론 | 실시간 | 인프라 | 사용 목적 |
|------|------|--------|--------|--------|-----------|
| 1 (시뮬레이터) | 단일 프로세스 | ✅ | ❌ | 불필요 | 정확성 |
| 2 (멀티스레드) | tokio runtime | ❌ | ✅ | 불필요 | 성능 측정 |
| 3 (Docker) | 실제 컨테이너 | ❌ | ✅ | Docker | 인수 테스트 |
| 4 (Foundry) | EVM + Docker | ❌ | ✅ | Docker | 컨트랙트 |

**`cargo test`는 계층 1만 실행**. 계층 2는 `cargo bench`. 계층 3/4는 별도 스크립트.

---

## 4. Phase별 테스트 적용 계획

| Phase | 계층 1 | 계층 2 | 계층 3/4 |
|-------|--------|--------|---------|
| Phase 1 (인터페이스) | ❌ (설계만) | ❌ | ❌ |
| Phase 2 (Shadow State) | ✅ ShadowDb 단위 테스트 | ❌ | ❌ |
| Phase 3 (합의 추출) | ✅ N노드 합의 시뮬레이션 | ❌ | ❌ |
| Phase 4 (스케줄러) | ✅ SoftCommit/HardCommit 처리 | ❌ | ❌ |
| Phase 5 (통합) | ✅ E2E 통합 테스트 | ✅ 벤치마크 | ✅ Docker + Foundry |

---

## 5. 완료 기준

- [x] msim 재사용 불가 확인 — `docs/mysticeti-analysis.md §15`
- [x] 대안 결정: tokio test-util 자체 구현 (turmoil 평가 예정)
- [x] 3계층 구조 확정 (시뮬레이터 / 멀티스레드 / Docker)
- [x] Foundry 컨트랙트 테스트 계획 포함
- [x] Phase별 테스트 계층 적용 계획 수립
