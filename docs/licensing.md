# 라이선스 의존성 관리

이 프로젝트가 사용하는 외부 코드의 라이선스 현황 및 주의사항을 기록한다.

---

## 1. REVM (`extern/revm`) — MIT

**라이선스**: MIT
**출처**: bluealloy/revm (submodule)

### GMP feature flag 주의

REVM의 `gmp` feature를 활성화하면 `libgmp`(GNU Multiple Precision Arithmetic Library)가
동적 링크되며, GPL 라이선스의 영향을 받는다.

| 상태 | 라이선스 |
|------|---------|
| `gmp` feature OFF (기본값) | MIT 유지 |
| `gmp` feature ON | GPL 오염 |

`gmp`는 modexp 프리컴파일(주소 0x05)의 가속에만 사용된다.
순수 Rust 구현으로도 동작하므로 활성화할 이유가 없다.

**현재 상태**: `crates/` 내 어떤 `Cargo.toml`에도 `features = ["gmp"]` 없음. 안전.

**규칙**: `revm`, `revm-precompile` 의존성에 `gmp` feature를 추가하지 않는다.

---

## 2. SUI / Mysticeti (`extern/sui`) — Apache 2.0

**라이선스**: Apache 2.0
**저작권**: Copyright 2022 Mysten Labs, Inc.
**출처**: MystenLabs/sui (submodule)

Apache 2.0의 주요 의무:
- 배포 시 원본 라이선스 및 저작권 고지 유지 → `THIRD_PARTY_LICENSES` 참조
- **수정된 파일에 변경 주석 필수** (§4(b)): 우리가 `extern/sui` 내 파일을 직접 수정하면
  해당 파일 상단에 `// Modified by jazzcake: <변경 내용 요약>` 주석을 추가해야 한다.
- 현재 상태: submodule 원본 그대로 사용. 수정 파일 없음.

---

## 3. BlockSTM 알고리즘 — 소스코드 미사용

**출처**: Aptos Labs (aptos-core, Apache 2.0)
**우리 관계**: `crates/shadow-state`는 BlockSTM 논문을 참고한 **독립 구현**이다.
aptos-core 소스코드를 복사하거나 링크하지 않는다.

**라이선스 의무 없음**. 알고리즘 자체는 저작권 보호 대상이 아니다.
(논문 출처: "Block-STM: Scaling Blockchain Execution by Turning Ordering Curse
to a Performance Blessing", Aptos Labs, 2022)

설계 차이점은 `docs/revm-analysis.md` §8 참조.

---

## 4. Apache 2.0 수정 파일 관리 규칙

`extern/sui` 파일을 수정하는 경우:

```rust
// Modified by jazzcake (dual_optimistic_l2): <변경 내용 한 줄 요약>
// Original: Copyright 2022 Mysten Labs, Inc. — Apache License 2.0
```

이 주석을 수정된 파일 상단(기존 저작권 헤더 바로 아래)에 추가한다.
