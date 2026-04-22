# CLI-Agent 프로젝트 종합 분석 및 TODO

## Context

Rust + Next.js 기반 멀티에이전트 오케스트레이터 CLI/TUI 플랫폼의 전반적인 코드 품질, 에이전트 동작 결함, 리팩토링 필요사항을 종합 분석한 결과.

**현재 상태 (최신 커밋 `ada89e5` 기준)**: 빌드 정상, **111개 테스트 통과**, Phase 1/2/3 완료. 7,654줄 모놀리스였던 `orchestrator/mod.rs`는 10개 서브모듈로 분할(mod.rs 2,984줄)되었고, 2,796줄이었던 `interface/api.rs`는 12개 핸들러 모듈로 분할(api.rs 267줄)되었다. 에이전트 휴리스틱 오탐, reviewer 재시도, LLM 분류 fast-path/캐시 등 동작 정확성·성능 개선도 적용됨.

---

## 진행 상태 요약

| Phase | 범위 | 상태 | 커밋 수 |
|-------|------|------|--------|
| Phase 1 | Critical Fixes (TODO 1-1 ~ 1-7) | ✅ 완료 | 7 |
| Phase 2 | Core Refactoring (TODO 3-1, 3-2, 3-3) | ✅ 완료 | 21 |
| Phase 3 | Agent Behavior (TODO 2-1 ~ 2-7) | ✅ 완료 | 7 |
| Phase 4 | Remaining Refactoring (TODO 3-4 ~ 3-6) | ⏳ 대기 | — |
| Phase 5 | Test Coverage (TODO 5-1 ~ 5-6) | ⏳ 대기 | — |
| Phase 6 | Infrastructure (TODO 4-1 ~ 4-8) | ⏳ 대기 | — |
| Phase 7 | Performance (TODO 6-1 ~ 6-7) | ⏳ 대기 | — |
| 추가 제안 | Workflow / Tracing / Harness (7-1 ~ 9-6) | ⏳ 대기 | — |

**누적 35 commits** · 테스트 87 → 111 (+24)

---

## 1. Critical Fixes (안정성/정확성) — ✅ Phase 1 완료

### TODO 1-1: `serde_json::to_value().unwrap()` 제거 — API 서버 패닉 방지 — ✅ `523120f`
- **기존 위치**: `src/interface/api.rs` 21개소
- **현재 위치**: 각 `src/interface/handlers/*.rs` 서브모듈 (분할 완료)
- **적용**: 공통 헬퍼 `json_value()` 도입 (직렬화 실패 시 error 객체로 degrade + `tracing::error!` 로깅). 21개소 전부 치환.

### TODO 1-2: 토큰 추정 함수 버그 수정 — ✅ `83cd72e`
- **파일**: `src/context/mod.rs` `estimate_tokens()`
- **적용**: `max(whitespace * 1.3, char_count / 3.5)` — tiktoken cl100k_base 통계 기반 char-floor 적용. 한국어 과소평가 해결. 유닛테스트 3개 추가.

### TODO 1-3: Coder 백엔드 타임아웃 시 자식 프로세스 미종료 — ✅ `4ff69dd`
- **파일**: `src/orchestrator/coder_backend.rs` (ClaudeCodeBackend, CodexBackend)
- **적용**: `tokio::time::timeout` Elapsed 시 `child.kill().await` + `child.wait().await`로 좀비 회수. Codex는 임시 파일까지 정리.

### TODO 1-4: `std::sync::Mutex` async 컨텍스트 데드락 위험 — ✅ `b705543`
- **현재 위치**: `src/orchestrator/run_manager.rs`, `src/orchestrator/completion.rs` (mod.rs에서 분할됨)
- **적용**: `static_node_ids` lock scope 최소화 + `unwrap_or_else(|e| e.into_inner())` poisoning 복구. `tokio::sync::Mutex` 전환은 불필요 — lock이 await 경계를 넘지 않음 확인.

### TODO 1-5: `terminal/mod.rs` Mutex poisoning 패닉 — ✅ `2171e4e`
- **파일**: `src/terminal/mod.rs:143`, `src/interface/handlers/terminal.rs` (2511 → 분할됨)
- **적용**: `lock().unwrap_or_else(|e| e.into_inner())` 패턴으로 교체.

### TODO 1-6: `record_action_event` 에러 로깅 — ✅ `2ac2912`
- **파일**: `src/orchestrator/mod.rs` `record_action_event` 래퍼
- **적용**: `let _ =` 삭제 → `if let Err(e) = ... { tracing::warn!(...) }`. 래퍼 하나 수정으로 모든 호출 사이트 자동 이익.

### TODO 1-7: Coder 백엔드 파일 변경 감지 — ✅ `eee21fb`
- **파일**: `src/orchestrator/coder_backend.rs`
- **적용**: `snapshot_working_tree()` + `detect_changed_files()` 헬퍼 추가 (`git status --porcelain=v1` 기반). 3개 백엔드 모두에서 실행 전후 diff로 `files_changed` 채움. 비-git 디렉토리에서는 빈 벡터 fallback. 유닛테스트 2개 추가.

---

## 2. Agent Behavior Improvements (에이전트 동작 개선) — ✅ Phase 3 완료

### TODO 2-1: `verify_completion` — 모호한 리뷰어 응답 처리 개선 — ✅ `e155e7c`
- **파일**: `src/orchestrator/completion.rs`
- **적용**:
  - `VerificationVerdict` enum (Complete/Incomplete/Ambiguous) 도입.
  - `parse_reviewer_verdict()` 3단 파서: JSON(`status`+`reason`) → 접두어 `COMPLETE/INCOMPLETE:` → 키워드 스캔 (단어 경계 + 부정어 우선).
  - `run_reviewer(strict)` 헬퍼로 분리, Ambiguous 시 **strict JSON-only 프롬프트로 1회 재시도** 후 판단.
  - 유닛테스트 8개.

### TODO 2-2: `classify_task` — 매번 LLM 호출하는 비효율 제거 — ✅ `ada89e5`
- **파일**: `src/orchestrator/task_classifier.rs`, `graph_builder.rs`, `mod.rs` (Orchestrator.classify_cache)
- **적용**:
  - `classify_task_fast()` — URL/코딩 verb/config verb+noun/analysis verb 고신뢰 fast-path. `Some(TaskType)` 반환 시 LLM skip.
  - `classify_cache: Arc<DashMap<(Uuid, String), TaskType>>` — 세션×task 키. `build_graph`가 캐시 hit 시 LLM 호출 완전 생략.
  - 테스트 콜사이트는 `Uuid::nil()`로 opt-out. 유닛테스트 4개.

### TODO 2-3: `classify_task_fallback` — 오분류 문제 — ✅ `5be251b`
- **파일**: `src/orchestrator/task_classifier.rs`
- **적용**: 1st-match-wins → 5단계 precedence ladder로 재구성. 코딩 verb가 config noun을 이김 ("debugging a settings display bug" → CodeGeneration). Configuration은 verb+noun 동반 시만. 전체 키워드 스캔을 `contains_word`로 전환. 유닛테스트 4개.

### TODO 2-4: `auto_skill_route` — 스킬 ID 부분 문자열 매칭 — ✅ `a967eba`
- **파일**: `src/orchestrator/skill_router.rs`, `helpers.rs`
- **적용**: `contains_word()` helper 도입 (단어 경계 + 대소문자 무시 + `_` 포함). `skill.id`/`skill.name` 모두 `contains` → `contains_word`로 교체. "ci"가 "circle" 안에서 매칭되는 문제 해결. 유닛테스트 4개.

### TODO 2-5: `looks_like_follow_up_task` — 5토큰 이하 무조건 후속 판정 — ✅ `8213e3c`
- **파일**: `src/orchestrator/task_classifier.rs`
- **적용**: raw length 규칙 제거. 신규 로직:
  1. fresh task verb (fix/add/refactor/수정/…) 있으면 false (신규 작업).
  2. 그외 `contains_word`로 referent/confirmation 마커 검사.
  3. ≤2 토큰 무-verb는 follow-up.
- "Fix this bug" → false, "commit the changes"에서 "it" 오탐 해결. 유닛테스트 4개.

### TODO 2-6: ToolCaller 반복 루프 상한 — ✅ `7201f2b`
- **파일**: `src/orchestrator/node_executor.rs`
- **적용**: `MAX_TOOL_ITERATIONS=5`는 이미 존재. 추가 보완:
  - `MAX_TOOLS_PER_ITERATION=10` — 단일 LLM 턴에서 반환된 tool_calls를 truncate + warn 로그.
  - Exhaustion 감지용 `exited_via_break` 플래그 — 캡 도달 시 `tracing::warn!` 구조화 로깅.

### TODO 2-7: MCP 개별 툴 호출 타임아웃 — ✅ `613f801`
- **파일**: `src/orchestrator/node_executor.rs`
- **적용**: `tokio::time::timeout(Duration::from_secs(60), mcp.call_tool(...))` 래핑. Elapsed는 일반 `Err`로 처리되어 기존 실패 분기로 흐름. 행이 걸린 MCP 서버가 전체 에이전트를 블록하는 문제 해결.

---

## 3. Code Refactoring (유지보수성)

### TODO 3-1: `orchestrator/mod.rs` 분할 — ✅ 완료 (Phase 2)
- **시작**: 7,654줄 모놀리스
- **현재**: 2,984줄 — 10개 서브모듈로 분할
- **적용된 구조**:
  ```
  src/orchestrator/
  ├── mod.rs              (2,984 — Orchestrator 구조체 + 남은 유틸/트레이스 뷰)
  ├── run_manager.rs      (473 — submit_run, execute_run, run_and_wait) — 9521a13
  ├── graph_builder.rs    (652 — build_graph + recovery/followup/workflow) — 4549ff7
  ├── task_classifier.rs  (544 — classify_task + fast-path + follow-up 판정) — dff9422
  ├── node_executor.rs    (1,942 — build_run_node_fn + event_sink) — a060d4d
  ├── completion.rs       (* — verify_completion + on_completed + verdict parser) — f37b03d, e155e7c
  ├── settings.rs         (265 — AppSettings 읽기/쓰기/적용) — 9ea4035
  ├── context_builder.rs  (* — memory query/history chunks/run summary) — 61d8725
  ├── skill_router.rs     (182 — auto_skill_route) — b078125
  └── helpers.rs          (467 — 파싱/포맷 순수 유틸) — b16af71
  ```
- **전제 충족**: 이후 모든 리팩토링(3-2, 3-6 등)의 기반.

### TODO 3-2: `interface/api.rs` 도메인별 분할 — ✅ 완료 (Phase 2)
- **시작**: 2,796줄
- **현재**: 267줄 (-90%) — 라우터 조립 + ApiState + 공용 DTO + dashboard/web_client 정적 핸들러만 잔존
- **적용된 구조**:
  ```
  src/interface/
  ├── api.rs              (267 — router(), ApiState, ListQuery, WsAuthQuery, ExecuteWorkflowRequest, json_value)
  └── handlers/
      ├── runs.rs         (693 — 15개 run 엔드포인트 + SSE/WS 스트림) — d19a984
      ├── memory.rs       (393 — 7개 세션/글로벌 메모리) — 853d8ae
      ├── terminal.rs     (277 — 4개 PTY + handle_terminal_ws) — 9b60632
      ├── sessions.rs     (227 — 6개 세션 CRUD) — f33b8af
      ├── workflows.rs    (202 — 6개 워크플로우) — 6b71230
      ├── settings.rs     (198 — 6개 설정/모델/프로바이더) — 75369d0
      ├── webhooks.rs     (184 — 5개 웹훅) — cf569ef
      ├── schedules.rs    (149 — 5개 스케줄) — 6628e71
      ├── team.rs         (106 — 3개 팀/GitHub 활동) — 5e5d722
      ├── cluster.rs      (96  — 4개 클러스터) — 5e5d722
      ├── skills.rs       (90  — 4개 스킬) — 478b174
      └── mcp.rs          (75  — 3개 MCP 툴 콜) — c0352e7
  ```

### TODO 3-3: `NodeExecutionResult` 생성 중복 제거 — ✅ `7cf2d6d`
- **파일**: `src/runtime/mod.rs` (impl), `src/orchestrator/node_executor.rs` (콜사이트)
- **적용**: `NodeExecutionResult::{success, failure, failure_with_output}` 연관 함수 3종 추가. 27개 production 생성 중 **19개(70%)**를 헬퍼로 전환. 조건부 `succeeded`/`error` 표현식은 구조 리터럴 유지.

### TODO 3-4: 프론트엔드 API 설정 중복 통합 — ⏳ 대기
- **파일**: `web/src/lib/api-client.ts:3-5`, `web/src/lib/sse.ts:3-5`, `web/src/hooks/use-terminal-ws.ts:15-17`
- **문제**: 3곳에서 동일한 `API_KEY`, `API_SECRET`, `API_URL` 하드코딩
- **수정**: `web/src/lib/config.ts`로 추출하여 단일 소스 관리

### TODO 3-5: Discord/Slack 게이트웨이 서명 검증 추상화 — ⏳ 대기
- **파일**: `src/gateway/slack.rs:51-87`, `src/gateway/discord.rs`, `src/webhook/mod.rs:44-80`
- **문제**: 유사한 서명 검증 로직이 3곳에 분산
- **수정**: `crypto::SignatureVerifier` 트레이트 도입, HMAC/Ed25519 구현체 분리

### TODO 3-6: `record_action_event` + `record_node_progress` 쌍 호출 통합 — ⏳ 대기
- **현재 위치**: `src/orchestrator/node_executor.rs` (build_run_node_fn 내 ~16회)
- **문제**: 거의 동일한 payload로 두 함수를 연달아 호출하는 패턴 반복
- **수정**: `record_node_event()` 통합 헬퍼 도입, 내부에서 양쪽 모두 호출

---

## 4. Infrastructure Improvements (인프라 개선) — ⏳ 전부 대기

### TODO 4-1: API 레이트 리미팅 추가
- **파일**: `src/interface/api.rs` (라우터 조립) + `src/interface/handlers/*` (엔드포인트)
- **문제**: 모든 엔드포인트에 레이트 리밋 없음
- **수정**: `tower` 미들웨어 기반 IP/키별 레이트 리미터 추가

### TODO 4-2: CORS 정책 강화
- **파일**: `src/interface/api.rs` — `CorsLayer`
- **문제**: `Any` origin 허용 (완전 개방)
- **수정**: 환경변수 기반 `ALLOWED_ORIGINS` 설정, 프로덕션에서는 명시적 origin만 허용

### TODO 4-3: 요청 검증 미들웨어 추가
- **문제**: API 요청에 대한 입력 검증이 핸들러 내부에서만 이루어짐
- **수정**: `validator` 크레이트 기반 DTO 검증 레이어 도입

### TODO 4-4: 헬스체크 엔드포인트 추가
- **문제**: `/health` 또는 `/v1/health` 엔드포인트 없음
- **수정**: DB 연결, MCP 서버 상태, 메모리 사용량 포함하는 헬스체크 추가

### TODO 4-5: Graceful Shutdown 핸들링
- **문제**: 서버 종료 시 실행 중인 run의 정리(cleanup) 로직 없음
- **수정**: `tokio::signal` 기반 shutdown hook, 진행 중인 run을 `Cancelled` 상태로 전환

### TODO 4-6: DB 마이그레이션 시스템 도입
- **파일**: `src/memory/store.rs:36-200`
- **문제**: `ALTER TABLE ADD COLUMN` 에러를 무시하는 방식의 원시적 마이그레이션
- **수정**: `sqlx::migrate!()` 매크로 기반 버전 관리 마이그레이션 도입

### TODO 4-7: 단기 메모리 GC 구현
- **파일**: `src/memory/mod.rs`
- **문제**: `short_term: Arc<DashMap<Uuid, Vec<ShortTermItem>>>` — TTL 만료된 아이템이 정리되지 않음
- **수정**: 백그라운드 태스크로 주기적 TTL 만료 아이템 제거

### TODO 4-8: Webhook 시크릿 암호화 저장
- **파일**: `src/memory/store.rs` (webhook_endpoints 테이블)
- **문제**: `secret` 필드가 평문으로 DB에 저장됨
- **수정**: AES-GCM 등으로 암호화 후 저장, 조회 시 복호화

---

## 5. Test Coverage Expansion (테스트 커버리지) — ⏳ 전부 대기

**현재 상태**: 111개 테스트 (Phase 3에서 +24). 신규 테스트는 대부분 단위 테스트. 통합 테스트/E2E 부재는 여전함.

### TODO 5-1: API 핸들러 통합 테스트 추가
- **파일**: `tests/api_integration.rs` (신규)
- **범위**: 세션 CRUD, 실행 제출/취소/일시정지/재개, 메모리 CRUD, 웹훅 등록/배달
- **방법**: `axum::test` 또는 `reqwest` + 인메모리 서버
- **참고**: handlers/ 분할로 각 모듈별 타겟팅 테스트 쉬워짐

### TODO 5-2: 에이전트 동작 에러 복구 경로 테스트
- **현재 위치**: `src/orchestrator/run_manager.rs`, `completion.rs`, `graph_builder.rs`
- **범위**:
  - 노드 실패 → recovery graph 빌드 → 재실행 플로우
  - 검증 incomplete → continuation graph → 최대 횟수 후 실패
  - 동일 failure_class 연속 2회 → 즉시 실패 전환

### TODO 5-3: 동시 실행 테스트
- **범위**: 다중 run 병렬 실행 시 DashMap 경합, 취소/일시정지 동시 요청, 워크스페이스 격리

### TODO 5-4: MCP 툴 호출 플로우 테스트
- **범위**: 정상 호출, 타임아웃 (TODO 2-7 신규 60s 래퍼 검증), 서버 응답 없음, 잘못된 JSON 응답 등

### TODO 5-5: Coder 백엔드 단위 테스트
- **파일**: `src/orchestrator/coder_backend.rs`
- **범위**: LLM/ClaudeCode/Codex 각 백엔드의 정상/실패/타임아웃 경로 (TODO 1-3의 kill 로직 커버)
- **참고**: TODO 1-7 추가로 `flush_utf8_safe_*`와 `detect_changed_files_*` 테스트는 이미 존재

### TODO 5-6: 프론트엔드 핵심 경로 E2E 테스트 확장
- **파일**: `web/e2e/`
- **범위**: 채팅 전송→응답 수신, 실행 취소, 세션 전환, 설정 변경

---

## 6. Performance Optimizations (성능 최적화) — ⏳ 전부 대기 (부분 선반영 있음)

### TODO 6-1: `record_action_event` 배치 처리
- **현재 위치**: `src/orchestrator/mod.rs` `record_action_event` (1곳) + 호출 사이트 전체 (다수)
- **문제**: 모든 이벤트를 개별 DB INSERT로 기록. 고빈도 이벤트(token_chunk 등)에서 DB 부하
- **수정**: mpsc 채널 기반 배치 INSERT (100ms 또는 50건 단위 flush)

### TODO 6-2: 과도한 Arc clone 정리
- **현재 위치**: `src/orchestrator/node_executor.rs` `build_run_node_fn`
- **문제**: 클로저 내에서 orchestrator, agents, router, memory, context, mcp, coder_manager 등 12개 Arc를 매번 clone
- **수정**: 관련 참조를 묶은 `NodeExecutionContext` 구조체 도입, 단일 Arc clone으로 대체

### TODO 6-3: DashMap 반복 시 성능 개선
- **파일**: `src/orchestrator/mod.rs` (`list_active_runs`, `list_recent_runs`, `list_skills`)
- **문제**: 전체 DashMap 반복 + clone + sort. 대량 데이터 시 성능 저하
- **수정**: 인덱스 캐시 또는 정렬 유지 자료구조 도입

### TODO 6-4: 프론트엔드 이벤트 정렬 최적화
- **파일**: `web/src/components/agent-thinking.tsx:69`
- **문제**: `setEvents` 콜백에서 매 이벤트마다 배열 복사 + 전체 재정렬 (O(n log n))
- **수정**: 이진 삽입(O(log n)) 또는 seq 기반 append-only 보장

### TODO 6-5: Pause 메커니즘 비효율 개선
- **파일**: `src/runtime/mod.rs:243`
- **문제**: 120ms sleep 루프로 pause 상태 폴링
- **수정**: `tokio::sync::Notify` 또는 watch 채널 기반 이벤트 구동 방식으로 전환

### TODO 6-6: SSE 프론트엔드 재연결 로직 추가
- **파일**: `web/src/lib/sse.ts`, `web/src/hooks/use-sse.ts`
- **문제**: SSE 연결 끊김 시 재연결 없음. 네트워크 일시 단절 시 이후 이벤트 전부 유실
- **수정**: exponential backoff 재연결 + 마지막 수신 seq 기반 resume + "Reconnecting..." UI 표시

### TODO 6-7: 메모리 검색 인덱싱 개선
- **파일**: `src/memory/store.rs`
- **문제**: `search_memory`가 `limit * 20`행 fetch 후 Rust에서 스코어링 — O(n) 스캔
- **수정**: `memory_items(session_id, updated_at DESC)` 인덱스 추가, FTS5 키워드 검색 프리필터 적용

---

## 7. Workflow Composition & Requirement Analyzer — ⏳ 전부 대기 (신규 기능)

### 현재 상태
- 워크플로우는 `skills/*.yaml`에 정적 DAG로 정의 (7개 스킬)
- `auto_skill_route()`가 키워드 매칭으로 스킬 선택 (TODO 2-4 word-boundary 적용 완료, 오탐 현저히 감소)
- `build_graph()`가 TaskType별로 하드코딩된 고정 그래프 생성 (9가지 패턴). TODO 2-2 fast-path + 캐시 적용.
- 사용자 요구사항 분석 → 동적 워크플로우 구성 경로 없음
- SubtaskPlan은 Planner LLM 출력에 전적으로 의존, 구조적 검증 부재

### TODO 7-1: 사용자 요구사항 분석기 (Requirement Analyzer) 도입
- **파일**: `src/orchestrator/requirement_analyzer.rs` (신규)
- **문제**: 현재 `classify_task()`는 단순히 TaskType 하나로 분류할 뿐, 사용자 요구의 복합성/제약조건/우선순위를 구조적으로 분석하지 않음
- **설계**:
  ```rust
  pub struct RequirementAnalysis {
      pub primary_intent: TaskType,
      pub sub_intents: Vec<TaskType>,        // 복합 요구 분해
      pub constraints: Vec<Constraint>,       // 시간, 도구, 언어 등 제약
      pub required_capabilities: Vec<String>, // 필요한 MCP 도구/에이전트 역할
      pub priority: Priority,                 // 긴급도
      pub estimated_complexity: Complexity,   // simple/moderate/complex
      pub context_requirements: Vec<String>,  // 필요한 선행 컨텍스트
  }
  ```
- **동작**: classify_task 호출 전에 1차 분석 → 복합 요구 시 자동으로 multi-phase 워크플로우 생성
- **관련 파일**: `src/orchestrator/task_classifier.rs`, `graph_builder.rs`

### TODO 7-2: 동적 워크플로우 컴포저 (Workflow Composer)
- **파일**: `src/orchestrator/workflow_composer.rs` (신규)
- **문제**: 현재 `build_graph()`는 TaskType별 고정 그래프만 생성. 사용자 요구에 맞는 커스텀 워크플로우를 런타임에 조합할 수 없음
- **설계**:
  ```rust
  pub struct WorkflowComposer {
      skill_registry: Arc<DashMap<String, WorkflowTemplate>>,
      agent_registry: AgentRegistry,
  }
  impl WorkflowComposer {
      /// RequirementAnalysis 기반으로 최적 워크플로우 자동 구성
      pub async fn compose(&self, analysis: &RequirementAnalysis, available_tools: &[McpToolDefinition]) -> ExecutionGraph;
      /// 기존 스킬 템플릿들을 체이닝하여 복합 워크플로우 생성
      pub fn chain_skills(&self, skill_ids: &[&str], params: HashMap<String, String>) -> anyhow::Result<ExecutionGraph>;
      /// 사용자 자연어 → 워크플로우 YAML 생성 (LLM 지원)
      pub async fn generate_from_description(&self, description: &str, router: &ModelRouter) -> anyhow::Result<WorkflowTemplate>;
  }
  ```
- **핵심**: 고정 그래프 패턴(SimpleQuery, Analysis, CodeGeneration 등)을 컴포저블 빌딩 블록으로 전환

### TODO 7-3: SubtaskPlan 구조적 검증 레이어
- **현재 위치**: `src/orchestrator/completion.rs` (on_completed 내 SubtaskPlan 파싱)
- **문제**: Planner LLM이 생성한 SubtaskPlan JSON을 파싱만 하고 구조적 검증 없이 실행. 순환 의존, 없는 역할 참조, 과도한 서브태스크 등 검증 부재 (단, `MAX_DYNAMIC_SUBTASKS_PER_PLAN=50` 하드캡은 이미 존재)
- **수정**:
  - 의존성 DAG 유효성 검사 (순환 탐지)
  - agent_role 존재 여부 확인
  - mcp_tools 가용성 확인
  - 서브태스크 수 / 깊이 제한 적용
  - 검증 실패 시 Planner에 에러 피드백 + 재생성 요청

### TODO 7-4: 워크플로우 템플릿 UI — 시각적 편집기
- **파일**: `web/src/app/workflows/` (확장)
- **문제**: 현재 워크플로우 페이지는 목록/실행만 지원. DAG 기반 시각적 편집 불가
- **수정**:
  - 드래그앤드롭 노드 편집기 (react-flow 등 활용)
  - 에이전트 역할 노드 팔레트
  - 의존성 간선 연결
  - 파라미터 바인딩 UI
  - YAML 미리보기 및 내보내기

---

## 8. Agent Execution Tracing & Chat Visibility — ⏳ 전부 대기

### 현재 상태
- `RuntimeEvent` 13종 이벤트가 `EventSink` 콜백으로 발행됨
- `record_action_event` / `record_node_progress`로 DB 기록 (에러는 Phase 1에서 warn 로깅 추가됨 — TODO 1-6)
- 프론트엔드 `AgentThinking` 컴포넌트가 SSE 이벤트를 `NodeTimeline`으로 변환하여 표시
- DAG 그래프 (`trace/dag-graph.tsx`)와 이벤트 타임라인 (`trace/event-timeline.tsx`) 존재
- 행동 분석 (`behavior/swim-lane.tsx`, `action-mix.tsx`, `summary-metrics.tsx`) 존재

### 핵심 Gap
- 실시간 DAG 상태 업데이트 없음 (페이지 새로고침 필요)
- 서브에이전트 실행 계층 구조 미표시 (flat list로만 보임)
- 노드 간 데이터 흐름(어떤 출력이 어디로 전달되었는지) 불투명
- recovery/continuation 루프 진행 상태 채팅에서 미표시
- 토큰 사용량/비용 실시간 추적 없음

### TODO 8-1: 실시간 DAG 상태 스트리밍
- **파일**: `web/src/components/trace/dag-graph.tsx`, `web/src/hooks/use-sse.ts`
- **문제**: DAG 그래프가 API 폴링 기반으로만 업데이트. 실행 중 노드 상태 변화가 실시간 반영 안 됨
- **수정**:
  - SSE `action_event`에서 `node_started`/`node_completed`/`node_failed` 이벤트를 DAG 노드 상태에 즉시 반영
  - 노드 색상/애니메이션으로 실행 중(pulse), 성공(green), 실패(red) 시각적 표시
  - 동적으로 추가되는 서브태스크 노드를 DAG에 실시간 삽입

### TODO 8-2: 채팅 내 에이전트 실행 진행 인라인 카드
- **파일**: `web/src/components/agent-thinking.tsx` (확장)
- **문제**: 현재 `AgentThinking`은 노드별 토큰/툴콜만 표시. 전체 실행 흐름(classify → build_graph → execute → verify → replan) 가시화 부족
- **수정**:
  - **Phase Indicator**: 현재 어떤 단계인지 표시 (분류 → 그래프 빌드 → 실행 → 검증 → [재계획])
  - **Progress Bar**: 전체 노드 수 대비 완료 노드 수 진행률
  - **Recovery Alert Card**: replan/recovery 발생 시 이유와 함께 인라인 알림 카드
  - **Data Flow Card**: 선행 노드 출력 → 현재 노드 입력 데이터 흐름 요약

### TODO 8-3: 서브태스크 계층 트리 뷰
- **파일**: `web/src/components/trace/subtask-tree.tsx` (신규)
- **문제**: Planner가 SubtaskPlan으로 동적 서브태스크를 생성하면, 이것이 flat list로만 표시되어 계층 구조를 알 수 없음
- **수정**:
  - 트리 뷰 컴포넌트: 원본 그래프 노드 → 동적 서브태스크 → 추가 동적 노드 계층 표시
  - 각 서브태스크의 상태(pending/running/succeeded/failed) 표시
  - 서브태스크 클릭 시 해당 노드의 상세 출력/툴콜 확장

### TODO 8-4: 노드별 토큰 사용량 및 비용 실시간 추적
- **파일**: `src/runtime/mod.rs` (RuntimeEvent 확장), `src/router/mod.rs` (추론 결과에 토큰 수 포함)
- **문제**: 노드별/전체 run의 토큰 사용량, 추정 비용을 추적하는 메커니즘 없음
- **수정**:
  - `InferenceResult`에 `input_tokens`, `output_tokens` 필드 추가
  - `NodeExecutionResult`에 `token_usage: Option<TokenUsage>` 추가
  - `RuntimeEvent::NodeCompleted`에 토큰 사용량 포함
  - 프론트엔드에서 누적 토큰/비용 표시 위젯

### TODO 8-5: 검증/재계획 루프 트레이싱 강화
- **현재 위치**: `src/orchestrator/run_manager.rs` (verify → replan 루프)
- **문제**: continuation/recovery 루프의 진행 상태가 `ReplanTriggered` 이벤트 하나로만 기록. 몇 번째 시도인지, 왜 재계획인지 채팅에서 직관적으로 보이지 않음
- **수정**:
  - 새 이벤트 타입: `RunActionType::RecoveryPhaseStarted`, `RecoveryPhaseCompleted`
  - 페이로드에 `attempt`, `max_attempts`, `reason`, `mode(failure_recovery|completion_continuation)` 포함
  - 채팅 UI에 "재계획 시도 2/2: 검증 실패 — 미완성 항목 존재" 같은 명시적 상태 표시

---

## 9. Harness Engineering & Sub-Agent Management — ⏳ 전부 대기 (신규 설계)

### 현재 상태
- `AgentRegistry`가 11개 역할 에이전트를 관리 (YAML 설정 가능)
- `OrchestratorCluster`가 명명된 오케스트레이터 인스턴스를 멤버로 등록/관리
- `SubtaskPlan` 동적 파싱 → `on_completed` 콜백에서 새 AgentNode를 그래프에 주입 (`src/orchestrator/completion.rs`)
- 에이전트 실행은 `run_role()` / `run_role_stream()` 단일 진입점
- 서브에이전트 라이프사이클 관리 없음 (생성→실행→종료가 일회성)
- 에이전트 간 직접 통신 불가 (오직 의존성 출력을 통한 간접 전달)

### TODO 9-1: Agent Harness 추상 레이어 도입
- **파일**: `src/harness/mod.rs` (신규 모듈)
- **문제**: 현재 에이전트 실행은 `AgentRegistry.run_role()` → LLM 호출 → 결과 반환의 단순 파이프라인. 에이전트의 라이프사이클(초기화, 실행, 중간 상태 보고, 재시도, 정리)을 관리하는 하네스 계층 없음
- **설계**:
  ```rust
  /// 에이전트 하네스: 에이전트 실행의 전체 라이프사이클을 관리
  pub struct AgentHarness {
      registry: AgentRegistry,
      router: Arc<ModelRouter>,
      memory: Arc<MemoryManager>,
      mcp: Arc<McpRegistry>,
      active_sessions: Arc<DashMap<String, AgentSession>>,
  }

  pub struct AgentSession {
      pub session_id: String,
      pub agent_role: AgentRole,
      pub status: AgentSessionStatus,
      pub created_at: Instant,
      pub context_window: Vec<ContextChunk>,  // 누적 컨텍스트
      pub iteration_count: u32,
      pub token_budget_remaining: usize,
      pub parent_session: Option<String>,      // 서브에이전트 계층
      pub child_sessions: Vec<String>,
  }

  impl AgentHarness {
      /// 에이전트 세션 생성 및 초기화
      pub async fn spawn(&self, role: AgentRole, input: AgentInput, parent: Option<&str>) -> anyhow::Result<String>;
      /// 세션에 추가 입력 전달 (멀티턴)
      pub async fn send(&self, session_id: &str, message: &str) -> anyhow::Result<AgentOutput>;
      /// 세션 상태 조회
      pub fn status(&self, session_id: &str) -> Option<AgentSessionStatus>;
      /// 모든 자식 세션 포함 트리 구조 조회
      pub fn session_tree(&self, session_id: &str) -> SessionTree;
      /// 세션 종료 및 리소스 정리
      pub async fn terminate(&self, session_id: &str) -> anyhow::Result<()>;
      /// 토큰 예산 기반 자동 컨텍스트 압축
      pub async fn compact_context(&self, session_id: &str) -> anyhow::Result<()>;
  }
  ```
- **핵심 가치**: 에이전트를 일회성 함수 호출이 아닌 상태를 가진 세션으로 관리

### TODO 9-2: 서브에이전트 스포닝 및 계층 관리
- **파일**: `src/harness/sub_agent.rs` (신규)
- **문제**: 현재 Planner가 SubtaskPlan을 생성하면 `on_completed`에서 그래프 노드로만 추가. 서브에이전트의 부모-자식 관계, 결과 집약, 실패 전파를 체계적으로 관리하지 않음
- **설계**:
  ```rust
  pub struct SubAgentManager {
      harness: Arc<AgentHarness>,
      /// 부모 → 자식 세션 매핑
      hierarchy: Arc<DashMap<String, Vec<String>>>,
      /// 결과 수집기
      result_aggregator: Arc<ResultAggregator>,
  }

  impl SubAgentManager {
      /// 부모 세션의 컨텍스트를 상속받아 서브에이전트 생성
      pub async fn spawn_child(&self, parent_id: &str, role: AgentRole, task: &str) -> anyhow::Result<String>;
      /// 병렬 서브에이전트 일괄 생성 (SubtaskPlan에서 변환)
      pub async fn spawn_from_plan(&self, parent_id: &str, plan: &SubtaskPlan) -> anyhow::Result<Vec<String>>;
      /// 서브에이전트 결과 집약
      pub async fn collect_results(&self, parent_id: &str) -> Vec<AgentOutput>;
      /// 자식 실패 시 부모에게 통지 및 복구 전략 결정
      pub async fn handle_child_failure(&self, child_id: &str, error: &str) -> RecoveryAction;
      /// 전체 계층 트리 시각화 데이터
      pub fn hierarchy_tree(&self, root_id: &str) -> HierarchyTree;
  }
  ```
- **기존 연동**: `completion.rs::build_on_completed_fn`의 SubtaskPlan 처리 로직을 SubAgentManager로 위임

### TODO 9-3: 에이전트 간 메시지 버스
- **파일**: `src/harness/message_bus.rs` (신규)
- **문제**: 에이전트 간 통신이 오직 dependency_outputs (선행 노드 출력)으로만 가능. 실행 중 에이전트가 다른 에이전트에 질문하거나 피드백을 주고받을 수 없음
- **설계**:
  - `mpsc` 기반 에이전트 간 메시지 채널
  - 메시지 타입: `Query(질문)`, `Feedback(피드백)`, `Delegation(위임)`, `Status(상태 알림)`
  - 수신 에이전트의 컨텍스트에 메시지 자동 주입
  - 타임아웃 및 메시지 큐 크기 제한

### TODO 9-4: 하네스 수준 관측성 (Observability)
- **파일**: `src/harness/metrics.rs` (신규)
- **문제**: 현재 관측은 `RunActionEvent` 기록에 한정. 에이전트 세션 수준의 메트릭(토큰 소비율, 응답 지연, 재시도율, 컨텍스트 활용도) 부재
- **수정**:
  ```rust
  pub struct HarnessMetrics {
      pub active_sessions: usize,
      pub total_tokens_consumed: u64,
      pub total_cost_estimate: f64,
      pub avg_response_latency_ms: f64,
      pub retry_rate: f64,
      pub context_utilization: f64,     // 컨텍스트 예산 대비 사용률
      pub sub_agent_depth: u8,          // 최대 계층 깊이
      pub per_role_stats: HashMap<AgentRole, RoleStats>,
  }
  ```
- **노출**: API 엔드포인트 `GET /v1/harness/metrics` + 프론트엔드 대시보드 위젯

### TODO 9-5: 에이전트 역할 동적 확장 — 런타임 에이전트 등록
- **파일**: `src/agents/mod.rs` (AgentRegistry 확장)
- **문제**: 현재 `AgentRegistry`는 초기화 시 11개 고정 역할만 등록. 런타임에 새 역할 추가/수정 불가
- **수정**:
  - `register_role(&self, role_config: AgentRoleConfig) -> anyhow::Result<()>` 메서드 추가
  - API 엔드포인트 `POST /v1/agents/roles` 로 동적 역할 등록
  - YAML 핫 리로드: `agents/` 디렉토리 감시 → 변경 시 자동 재로드

### TODO 9-6: 하네스 기반 실행 흐름으로 orchestrator 전환
- **현재 위치**: `src/orchestrator/run_manager.rs` (execute_run), `node_executor.rs` (build_run_node_fn)
- **문제**: 현재 `build_run_node_fn`이 직접 `agents.run_role()`을 호출. 하네스 계층을 거치지 않아 세션 관리, 컨텍스트 축적, 계층 추적 등이 불가능
- **수정**:
  - `build_run_node_fn` 내부에서 `harness.spawn()` → `harness.send()` → `harness.terminate()` 패턴으로 전환
  - run 시작 시 루트 하네스 세션 생성
  - 각 노드 실행을 자식 세션으로 관리
  - 기존 `AgentRegistry.run_role()` 직접 호출 경로는 하네스 내부로 캡슐화

---

## 검증 계획

### 단계별 검증
1. **빌드 검증**: `cargo build && cargo test` — 현재 111개 테스트 통과
2. **프론트엔드 빌드**: `cd web && npm run build` — 정상 컴파일 (Phase 3까지 확인)
3. **API 기능 테스트**: 통합 테스트 추가 후 (TODO 5-1) `cargo test --test api_integration`
4. **에이전트 동작 검증** (Phase 3 완료로 대부분 자동화됨):
   - 모호한 Reviewer 응답 → 재시도 후 판단 (TODO 2-1 단위테스트)
   - 키워드 분류 경합 케이스 → context-weighted precedence (TODO 2-3 단위테스트)
   - ToolCaller 루프 상한 도달 시 정상 종료 (TODO 2-6)
   - MCP 행 걸림 → 60s 타임아웃 (TODO 2-7 — 수동 시나리오 필요, TODO 5-4로 이관)
5. **부하 테스트** (TODO 5-3): 동시 10 run 실행 시 패닉/데드락 없음 확인

### 구현 순서 (권장)
1. ~~**Phase 1**: Critical Fixes (TODO 1-1 ~ 1-7)~~ — ✅ 완료
2. ~~**Phase 2**: Core Refactoring (TODO 3-1, 3-2, 3-3)~~ — ✅ 완료
3. ~~**Phase 3**: Agent Behavior (TODO 2-1 ~ 2-7)~~ — ✅ 완료
4. **Phase 4**: Remaining Refactoring (TODO 3-4 ~ 3-6) — 중복 제거
5. **Phase 5**: Tests (TODO 5-1 ~ 5-6) — 회귀 방지 (Phase 6/7 전 필수)
6. **Phase 6**: Infrastructure (TODO 4-1 ~ 4-8) — 운영 안정성
7. **Phase 7**: Performance (TODO 6-1 ~ 6-7) — 최적화

**신규 기능 (우선순위는 사용자 합의 필요)**:
- Section 7 (Workflow Composition): 동적 요구사항 분석기 + 워크플로우 컴포저
- Section 8 (Tracing): 실시간 DAG 스트리밍 + 서브태스크 트리 + 토큰/비용 추적
- Section 9 (Harness): 에이전트 세션 라이프사이클 관리 + 메시지 버스

---

## 핵심 파일 참조 (2026-04-22 기준)

| 파일 | 줄 수 | 비고 |
|------|-------|------|
| `src/orchestrator/mod.rs` | 2,984 | 7,654 → 2,984 (Phase 2로 분할) |
| `src/orchestrator/node_executor.rs` | 1,942 | build_run_node_fn + event_sink |
| `src/orchestrator/graph_builder.rs` | 652 | build_graph + recovery/followup |
| `src/orchestrator/task_classifier.rs` | 544 | classify_task + fast-path + follow-up |
| `src/orchestrator/helpers.rs` | 467 | 파싱/포맷 순수 유틸 + contains_word |
| `src/orchestrator/run_manager.rs` | 473 | submit_run, execute_run, run_and_wait |
| `src/orchestrator/settings.rs` | 265 | AppSettings |
| `src/orchestrator/skill_router.rs` | 182 | auto_skill_route |
| `src/orchestrator/coder_backend.rs` | ~570 | Phase 1에서 git diff 감지 + kill 로직 추가 |
| `src/interface/api.rs` | 267 | 2,796 → 267 (Phase 2로 분할) |
| `src/interface/handlers/runs.rs` | 693 | 15개 run 엔드포인트 |
| `src/interface/handlers/memory.rs` | 393 | 7개 메모리 |
| `src/interface/handlers/terminal.rs` | 277 | 4개 PTY + WS |
| `src/context/mod.rs` | 354 | estimate_tokens 수정 완료 |
| `src/runtime/mod.rs` | 791 | NodeExecutionResult 헬퍼 추가됨. pause 메커니즘(TODO 6-5)은 남음 |
| `src/memory/store.rs` | 2,242 | 마이그레이션 시스템(TODO 4-6), 암호화(TODO 4-8) 남음 |
| `src/memory/mod.rs` | 610 | 단기 메모리 GC(TODO 4-7) 남음 |
| `src/terminal/mod.rs` | 241 | Phase 1 poisoning 처리 완료 |
| `web/src/lib/config.ts` | 신규 | API 설정 통합(TODO 3-4) 남음 |
| `web/src/components/agent-thinking.tsx` | ~600 | 이벤트 정렬(TODO 6-4) / phase indicator(TODO 8-2) 남음 |

---

## 커밋 레퍼런스 (35 commits)

### Phase 1 Critical Fixes
- `2171e4e` fix: Mutex poisoning in terminal scrollback
- `83cd72e` fix: estimate_tokens for CJK/long text
- `2ac2912` fix: record_action_event error logging
- `4ff69dd` fix: Coder child kill on timeout
- `b705543` refactor: static_node_ids lock scope
- `eee21fb` feat: Detect files changed by coder backends
- `523120f` fix: API handler panic via json_value helper

### Phase 2 Refactoring
- `7cf2d6d` refactor: NodeExecutionResult helpers
- `b16af71`, `b078125`, `dff9422`, `61d8725`, `9ea4035`, `f37b03d`, `4549ff7`, `9521a13`, `a060d4d` — orchestrator submodule extractions
- `6628e71`, `cf569ef`, `c0352e7`, `478b174`, `6b71230`, `75369d0`, `853d8ae`, `9b60632`, `5e5d722`, `f33b8af`, `d19a984` — interface handler extractions

### Phase 3 Agent Behavior
- `613f801` fix: MCP call_tool 60s timeout
- `7201f2b` fix: ToolCaller iteration caps + exhaustion log
- `a967eba` fix: auto_skill_route word-boundary matching
- `8213e3c` fix: looks_like_follow_up_task verb/referent pivot
- `e155e7c` fix: verify_completion JSON/prefix/keyword + retry
- `5be251b` fix: classify_task_fallback context-weighted ladder
- `ada89e5` perf: classify_task fast-path + session cache
