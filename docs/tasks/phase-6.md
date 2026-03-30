# Phase 6: Docker 멀티노드 검증 & Foundry 컨트랙트 테스트

**상태**: ⏳ 대기
**목표**: Phase 5에서 검증된 단일 프로세스 파이프라인을 실제 네트워크 환경으로 확장하고,
Solidity 컨트랙트를 통해 EVM 실행 레이어의 end-to-end 정확성을 검증한다.

---

## 서브페이즈 구성

```
6-A  node 바이너리 완성   ── 네트워크 I/O + CLI entrypoint
6-B  Docker 멀티노드      ── 4노드 클러스터, 장애 주입
6-C  Foundry 컨트랙트    ── 배포 + 병렬 실행 충돌 검증
6-D  문서화              ── README + 실행 가이드
```

---

## 6-A: node 바이너리 완성

Phase 5까지의 `crates/node`는 in-process 테스트 전용이다.
실제 네트워크 통신을 위한 최소한의 I/O 레이어를 추가한다.

| 순번 | 파일 | 작업 |
|------|------|------|
| 1 | `crates/node/src/network.rs` | 노드 간 블록 브로드캐스트 (TCP or QUIC 최소 구현) |
| 2 | `crates/node/src/rpc.rs` | 트랜잭션 수신 RPC 엔드포인트 (JSON-RPC `eth_sendRawTransaction`) |
| 3 | `crates/node/src/main.rs` | CLI entrypoint — `NodeConfig::from_env()` → `Node::start()` |
| 4 | `crates/node/src/lib.rs` | `NodeConfig` 완성 (peers, ports 실제 파싱) |

---

## 6-B: Docker 멀티노드 검증

| 순번 | 파일 | 작업 |
|------|------|------|
| 1 | `Dockerfile` | `crates/node` 바이너리 멀티스테이지 빌드 |
| 2 | `docker/compose.yml` | 4노드 클러스터 (node-0 ~ node-3), bridge network |
| 3 | `docker/config/` | 각 노드 설정 파일 (node_index, committee, peers) |
| 4 | `docker/scripts/monitor.sh` | 라운드 진행·HardCommit 로그 모니터링 스크립트 |

**장애 주입 시나리오:**
- node-0 `docker stop` → 나머지 3노드 합의 지속 (f=1 내성)
- `tc netem delay 50ms` 또는 `toxiproxy` 지연 주입 → 합의 완료 시간 측정

**검증 체크리스트:**
- [ ] `docker compose up` 으로 4노드 클러스터 정상 기동
- [ ] 합의 라운드가 지속적으로 진행됨 (로그 확인)
- [ ] node-0 중단 후 나머지 3노드에서 합의 지속
- [ ] 네트워크 지연 주입 후 합의 완료까지 시간 측정

---

## 6-C: Foundry 컨트랙트 테스트

| 순번 | 파일/명령 | 작업 |
|------|----------|------|
| 1 | `contracts/` | Foundry 프로젝트 초기화 (`forge init`) |
| 2 | `foundry.toml` | Docker 테스트넷 RPC 엔드포인트 연결 |
| 3 | `contracts/src/Counter.sol` | 기본 상태 읽기/쓰기 + 이벤트 발생 컨트랙트 |
| 4 | `contracts/src/ConflictTest.sol` | 동일 슬롯을 쓰는 트랜잭션 다수 전송용 컨트랙트 |
| 5 | `contracts/test/` | `forge test` 단위 테스트 |
| 6 | `cast send` | 테스트넷에 트랜잭션 전송 및 실행 확인 |

**ConflictTest 시나리오 목적:**
- 동일 storage slot에 쓰는 tx N개를 동시 전송
- ShadowDb의 conflict detection이 정상 동작하는지 확인
- 최종 상태가 serial execution 결과와 일치하는지 검증

**검증 체크리스트:**
- [ ] `forge test` 전체 통과
- [ ] `cast send`로 Counter 컨트랙트 상태 변경 확인
- [ ] ConflictTest: N개 충돌 tx 처리 후 최종 상태 일관성 확인

---

## 6-D: 문서화

| 파일 | 내용 |
|------|------|
| `README.md` | 설치 가이드, Docker 실행 방법, Foundry 연결 방법 |
| `docker/README.md` | 멀티노드 환경 로컬 실행 절차, 장애 주입 방법 |

---

## 실행 계획 (Execution Plan)

> 이 섹션은 Phase 시작 전 사용자와 함께 수립하고 승인받은 후 채운다.

---

## 완료 기준 (Done Criteria)

1. `docker compose up` 으로 4노드 클러스터 정상 기동, 합의 라운드 지속 진행
2. node-0 중단 시에도 나머지 3노드 합의 지속 (f=1 내성)
3. `forge test` 전체 통과
4. `cast send`로 컨트랙트 트랜잭션 전송 → 실행 및 확정 확인
5. 충돌 tx 다수 전송 시 최종 상태가 serial 결과와 일치
6. `README.md` 실행 가이드 완성
