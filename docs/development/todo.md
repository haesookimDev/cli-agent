# TODO

기존 `TODO.md` 내용 및 향후 개발 계획.

---

## ~~우선순위 높음~~

### ~~2. 코더 에이전트 병렬 실행~~ ✓ 완료 (2026-03-20)

- ~~Claude Code / Codex를 사용하여 실제 코드 작성~~ — ClaudeCodeBackend / CodexBackend 이미 구현됨
- ~~병렬 실행 시 여러 세션 생성~~ — 각 Coder 노드가 개별 세션으로 실행됨
- ~~Claude Code / Codex 출력을 채팅에서 실시간으로 표시~~ — CoderOutputChunk 이벤트로 스트리밍
- **핵심 버그 수정**: Coder 노드가 `req.task` 대신 `node.instructions`를 실행하도록 수정 — 병렬 Coder 서브태스크가 각자 고유한 작업을 실행

---

## 우선순위 중간

### ~~3. 역할 기반 멀티 워크플로우 오케스트레이터~~ ✓ 완료 (2026-03-20)

현재 단일 워크플로우 실행에서 확장하여 역할 기반 워크플로우가 오케스트레이터 관리 하에 유기적으로 상호작용:

- ✓ **in-graph Reviewer → Planner 피드백**: Reviewer 노드가 INCOMPLETE 출력 시 즉시 replan Planner 주입
- ✓ **`skills/multi_role_dev.yaml`**: plan → review_plan → code → validate_lint → summarize 파이프라인
- **잔여**: 스스로 개선하는 에이전트 (멀티 오케스트레이터 확장 — 우선순위 낮음 #5로 이동)

### ~~4. 업데이트 알림~~ ✓ 완료 (2026-03-20)

- ~~`UPDATE.md` / `RELEASE.md` 형식으로 업데이트 이력 관리~~ — `RELEASE.md` 추가
- ~~Slack / Discord 웹훅으로 업데이트 알림 발송~~ — `src/notifier.rs` + `agent notify-release` CLI 커맨드 (`SLACK_NOTIFY_WEBHOOK_URL`, `DISCORD_NOTIFY_WEBHOOK_URL`)

---

## 우선순위 낮음

### 5. 멀티 오케스트레이터 확장

여러 오케스트레이터 인스턴스로 대형 프로젝트 관리:
- 각 오케스트레이터가 서브 프로젝트 담당
- 오케스트레이터 간 통신 & 결과 집계

---

## 기술 부채

- ~~메모리 유사도 검색을 키워드 기반에서 임베딩 벡터 기반으로 전환~~ ✓ 완료 (2026-03-20) — `src/memory/embedding.rs` 추가, SQLite `embedding BLOB` 컬럼, OpenAI-compatible API 연동 (`EMBEDDING_API_URL`, `OPENAI_API_KEY`, `EMBEDDING_MODEL`)
- ~~테스트 커버리지 확대~~ ✓ 완료 (2026-03-20) — Windows 테스트 `#[cfg(windows)]` 게이팅 수정 + `gateway::parse_gateway_text` 7개 테스트 추가 (총 81개)
- ~~WebSocket 실시간 이벤트 지원 (현재 SSE 폴링)~~ ✓ 완료 (2026-03-20) — `/v1/runs/:run_id/ws` 엔드포인트 추가
- ~~프론트엔드 E2E 테스트 추가~~ ✓ 완료 (2026-03-20) — Playwright 설정 + `e2e/navigation.spec.ts`, `e2e/runner.spec.ts` (API 모킹 포함)

---

## 에이전트 실행 로직 분석에서 도출된 개선 항목

> 2026-03-20 분석 완료.
> 2026-03-20 MEDIUM 항목 M-2 ~ M-4, M-6 및 TODO #1 수정 완료.
> 2026-03-20 M-1(부분), M-5, TODO #2(코더 병렬), 기술부채(WebSocket) 수정 완료.

### MEDIUM — 에이전트 간 통신 개선

#### ~~M-1. 에이전트 출력 구조화 (JSONSchema 검증)~~ ✓ 완료 (2026-03-20)
- **파일**: `src/agents/mod.rs:27-30`, `src/runtime/mod.rs:15-24`
- **문제**: `NodeExecutionResult.output`이 비구조적 텍스트라 파싱 오류 빈발
- ✓ **완료**: `extract_json_object()` 헬퍼 추가 — Planner 출력에서 markdown fence / 산문 제거 후 SubtaskPlan JSON 추출
- ✓ **잔여 완료**: Reviewer 모호 출력 시 `warn!()` 추가; ToolCaller/Reviewer 파싱 실패 에러 명시 완료

#### ~~M-2. 서킷 브레이커를 노드 단위로 세분화~~ ✓ 완료 (2026-03-20)
- `role_failures` → `node_failures` (node ID 기반)로 교체
- `mark_dependents_pending_as_skipped` 추가 — 실패 노드의 직접 의존 노드만 Skip

#### ~~M-3. 동적 노드 생성 상한선 추가~~ ✓ 완료 (2026-03-20)
- `MAX_DYNAMIC_SUBTASKS_PER_PLAN = 50` 상수 추가
- Planner 출력 파싱 후 `.take(50)` 적용

#### ~~M-4. 폴백 타임아웃 주석/코드 불일치 수정~~ ✓ 완료 (2026-03-20)
- `FALLBACK_CHAIN_DEADLINE_SECS = 300` named constant 추출
- 체크 조건(`> 100`)과 에러 메시지(`"25s"`) 불일치 해소

#### ~~M-5. 실행 중 태스크 재분류 로직 추가~~ ✓ 완료 (2026-03-20)
- `OnNodeCompletedFn` 반환 타입을 `(Vec<AgentNode>, Vec<String>)`으로 변경
- Planner가 SubtaskPlan 생성 시 정적 그래프 노드(`extract`, `code` 등)를 Skipped 처리
- `static_node_ids: Arc<Mutex<Vec<String>>>`로 각 실행 단계별 정적 노드 목록 관리

### MEDIUM — 라우팅 개선

#### ~~M-6. 요청 수준 HTTP 타임아웃 추가~~ ✓ 완료 (2026-03-20)
- `INFER_TOTAL_TIMEOUT_SECS = 600` 상수 추가
- `infer()` / `infer_stream()` 공개 API에 `tokio::time::timeout` 래핑

### LOW — 장기 아키텍처 개선

#### L-1. 중간 상태 체크포인팅
- 크래시 시 인플라이트 노드 상태 손실 방지
- SQLite에 노드 상태 주기적으로 flush

#### L-2. 비용 한도 강제 (Cost Budgeting)
- 라우터가 비용 기준으로 모델을 선택하나 한도 초과 시 경고/중단 없음
- 런당 최대 비용 설정 + 초과 시 저비용 모델로 강제 스위칭

#### L-3. 크로스-런 의존성 지원
- 각 실행이 완전히 독립적으로, "실행 Y는 실행 X 성공 후 시작" 표현 불가
- 런 간 의존성 DAG 지원

#### L-4. 모델 성능 피드백 루프
- 실제 성공/실패 결과가 라우팅 점수에 반영되지 않음
- 모델별 성공률 추적 → 라우팅 가중치 자동 조정

#### L-5. 태스크 선점(Preemption) 지원
- 현재 개별 노드 중단 불가, 전체 실행 취소만 가능
- 노드 단위 pause/cancel 신호 추가
