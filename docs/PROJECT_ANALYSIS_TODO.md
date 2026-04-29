# CLI-Agent 프로젝트 종합 분석 및 TODO

## Context

Rust + Next.js 기반 멀티에이전트 오케스트레이터 CLI/TUI 플랫폼의 전반적인 코드 품질, 에이전트 동작 결함, 리팩토링 필요사항을 종합 분석한 결과.

**현재 상태 (최신 커밋 `c3dc569` 기준)**: 빌드 정상, **213개 테스트 통과 (lib 197 + integration 16)**, Phase 1~12 + Stage I~IV 완료. Stage I/II에서 token 추적·재계획 트레이싱·라이브 DAG·subtask tree·data flow card가 들어왔고, Stage III/IV에서 SubtaskPlan 검증·RequirementAnalyzer (shadow)·chain_skills·AgentHarness sidecar가 도입되었다. Phase 8에서 비용 추정 (config/pricing.yaml + per-run/session/cluster), Phase 9에서 Data Flow Card, Phase 10에서 react-flow 시각 편집기, Phase 11에서 orchestrator 가 본격적으로 harness 위에서 동작 (root session + 자식 노드 + SubAgentManager + 메시지 버스 + `/v1/harness/metrics`), Phase 12에서 `/v1/agents/reload` + generate_from_description 파서 헬퍼가 추가됨. 보류 항목: F1 RequirementAnalyzer promotion (1주 production 데이터 후), F2 generate_from_description LLM 호출 wrapper (D2 가격 정책 후), filesystem watcher 자동 reload (F5 옵션 b).

---

## 진행 상태 요약

| Phase | 범위 | 상태 | 커밋 수 |
|-------|------|------|--------|
| Phase 1 | Critical Fixes (TODO 1-1 ~ 1-7) | ✅ 완료 | 7 |
| Phase 2 | Core Refactoring (TODO 3-1, 3-2, 3-3) | ✅ 완료 | 21 |
| Phase 3 | Agent Behavior (TODO 2-1 ~ 2-7) | ✅ 완료 | 7 |
| Phase 4 | Remaining Refactoring (TODO 3-4 ~ 3-6) | ✅ 완료 | 3 |
| Phase 5 | Test Coverage (TODO 5-1 ~ 5-6) | ✅ 완료 | 6 |
| Phase 6 | Infrastructure (TODO 4-1 ~ 4-8) | ✅ 완료 | 8 |
| Phase 7 | Performance (TODO 6-1 ~ 6-7) | ✅ 완료 | 6 (6-2 deferred) |
| Stage I | Tracing 백엔드 (TODO 8-4 + 8-5) | ✅ 완료 | 3 |
| Stage II | Tracing 프론트 (TODO 8-1 + 8-2 + 8-3) | ✅ 완료 | 3 |
| Stage III | Workflow 백엔드 (TODO 7-3 + 7-1 + 7-2 chain) | ✅ 완료 | 3 |
| Stage IV | Harness sidecar (TODO 9-1 + 9-4 + 9-5) | ✅ 완료 | 1 |
| Phase 8 | Cost Tracking (TODO 8-4 가격표) | ✅ 완료 | 1 |
| Phase 9 | Data Flow Card (TODO 8-2 추가 슬라이스) | ✅ 완료 | 1 |
| Phase 10 | Visual Workflow Editor (TODO 7-4) | ✅ 완료 | 1 |
| Phase 11 | Harness 본격 전환 + SubAgentManager + 메시지 버스 + 메트릭 API (D5+D3+D4+F4) | ✅ 완료 | 2 |
| Phase 12 | /v1/agents/reload + generate_from_description 파서 (F5 + F2 일부) | ✅ 완료 | 1 |
| 잔여 | F1 RequirementAnalyzer promotion · F2 LLM wrapper · F5 watcher | ⏸️ production 운영/추가 합의 후 | — |

**누적 78 commits** · 테스트 87 → 213 (+126)

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

### TODO 3-4: 프론트엔드 API 설정 중복 통합 — ✅ `a1e52f2`
- **파일**: `web/src/lib/config.ts` (신규), `web/src/lib/api-client.ts`, `web/src/lib/sse.ts`, `web/src/hooks/use-terminal-ws.ts`
- **적용**: `API_KEY`/`API_SECRET`/`API_URL` 환경변수 기본값을 `web/src/lib/config.ts`로 추출. 3개 파일이 모두 해당 모듈에서 import 하도록 전환. 프론트 빌드 정상, 19개 라우트 유지.

### TODO 3-5: Discord/Slack 게이트웨이 서명 검증 추상화 — ✅ `74b2089`
- **파일**: `src/crypto.rs` (신규), `src/gateway/slack.rs`, `src/gateway/discord.rs`, `src/webhook/mod.rs`
- **적용**: `crypto::SignatureVerifier` 트레이트 도입 + `SlackHmacVerifier` / `DiscordEd25519Verifier` 구현체. 공용 프리미티브 `constant_time_eq`·`to_hex`·`hmac_sha256_hex`를 crypto 모듈로 이동 (slack + webhook에 중복 존재하던 `constant_time_eq` 제거). `SlackAdapter::verify_slack_signature`·`DiscordAdapter::verify_discord_signature`는 트레이트 위임. `webhook::sign_payload`도 `hmac_sha256_hex` 경유. 유닛 테스트 5종 추가 (`constant_time_eq`, `hmac_sha256_hex`, Slack accept/stale/tampered).

### TODO 3-6: `record_action_event` + `record_node_progress` 쌍 호출 통합 — ✅ `3775b09`
- **파일**: `src/memory/mod.rs` (helper 추가), `src/orchestrator/node_executor.rs` (event_sink), `src/orchestrator/mod.rs` (record_node_progress)
- **적용**: `MemoryManager::record_node_event(run_id, session_id, event_type, action, actor_type, ...)` 헬퍼 신설. 내부에서 `append_event` + `append_run_action_event`를 순차 호출하고 각 실패는 `tracing::warn!`으로 기록. `build_event_sink`에서 25줄 중복 블록 → 11줄 단일 호출. `record_node_progress`도 동일 헬퍼로 재작성.

---

## 4. Infrastructure Improvements (인프라 개선) — ✅ Phase 6 완료

### TODO 4-1: API 레이트 리미팅 추가 — ✅ `7451d8d`
- **파일**: `src/interface/rate_limit.rs` (신규), `src/interface/api.rs` (`serve` 미들웨어 부착)
- **적용**: per-key/per-IP 토큰 버킷 (`X-API-Key` → `X-Forwarded-For` → `X-Real-IP` → `anonymous`). 기본 capacity 120, refill 2/sec. `CLI_AGENT_RATE_LIMIT_CAPACITY` / `CLI_AGENT_RATE_LIMIT_PER_SEC` 로 override. 4개 unit + 1개 integration 테스트.

### TODO 4-2: CORS 정책 강화 — ✅ `1bd84d4`
- **파일**: `src/interface/api.rs` (`build_cors_layer`)
- **적용**: `CLI_AGENT_ALLOWED_ORIGINS` (콤마 구분 origin 리스트). 미설정/`*`은 와일드카드 유지(로컬 dev 호환). 잘못된 origin은 warn + 와일드카드 fallback.

### TODO 4-3: 요청 검증 미들웨어 추가 — ✅ `8e03adb`
- **파일**: `src/interface/api.rs` (`DefaultBodyLimit::max(2 MiB)`), `src/interface/handlers/webhooks.rs` (`validate_webhook_request`)
- **적용**: 본문 크기 한도 (`CLI_AGENT_MAX_BODY_BYTES` override) + RegisterWebhookRequest 검증 (URL scheme, 빈 events, 빈 secret 거부). 2개 integration 테스트.

### TODO 4-4: 헬스체크 엔드포인트 추가 — ✅ `afbc40b`
- **파일**: `src/interface/handlers/health.rs` (신규)
- **적용**: `/health` + `/v1/health` (인증 미요구). DB ping (`SELECT 1`), version, elapsed_ms, active run count, MCP 서버 수 포함. degraded 시 503. `Orchestrator::active_run_count` / `mcp_server_count` 헬퍼 추가.

### TODO 4-5: Graceful Shutdown 핸들링 — ✅ `35a6a63`
- **파일**: `src/interface/api.rs` (`shutdown_signal`)
- **적용**: `axum::serve(...).with_graceful_shutdown(...)` + SIGINT / SIGTERM (Unix) handler. 종료 시 모든 in-flight run을 `cancel_run()`으로 Cancelling 전환 후 axum drain.

### TODO 4-6: DB 마이그레이션 시스템 도입 — ✅ `59ecfe7`
- **파일**: `migrations/0001_initial_schema.sql` (신규), `src/memory/store.rs::init_schema`
- **적용**: 250줄 인라인 CREATE TABLE을 `sqlx::migrate!("./migrations").run(...)` 으로 대체. `_sqlx_migrations` 테이블이 버전 추적. embedding 컬럼은 0001에 포함, 호환성을 위해 정의 후 defensive ALTER (legacy) 유지. `sqlx`에 `migrate` feature 추가.

### TODO 4-7: 단기 메모리 GC 구현 — ✅ `8ad41c8`
- **파일**: `src/memory/mod.rs` (`gc_short_term`, `spawn_short_term_gc`), `src/main.rs` (5분 주기로 시작)
- **적용**: 만료된 ShortTermItem + 비어 있는 세션 버킷을 함께 정리. JoinHandle은 분리(detached); `MemoryManager` Arc가 drop되면 자연 종료. 1개 unit 테스트.

### TODO 4-8: Webhook 시크릿 암호화 저장 — ✅ `5eb2ae3`
- **파일**: `src/crypto.rs` (`encrypt_secret`/`decrypt_secret`), `src/memory/store.rs` (`register_webhook` / `list_webhooks`)
- **적용**: AES-256-GCM, 키는 `CLI_AGENT_DB_KEY` env 기반 SHA-256 derive. 포맷 `enc:v1:<base64(nonce||ciphertext)>`. legacy plaintext 행은 통과(decrypt 시 prefix 없으면 원문 반환). 5개 unit 테스트 (roundtrip, idempotent, legacy passthrough, wrong-key, fresh nonce).

---

## 5. Test Coverage Expansion (테스트 커버리지) — ✅ Phase 5 완료

**현재 상태**: 158개 테스트 (lib 144 + integration 14). Phase 5에서 +42 추가.

### TODO 5-1: API 핸들러 통합 테스트 추가 — ✅ `da8a189`
- **파일**: `tests/api_integration.rs` (신규)
- **적용**: in-process `axum::Router` + `tower::ServiceExt::oneshot` 으로 backend-free 통합 테스트. 8개 케이스 (auth glue, 세션 CRUD round-trip, unknown/invalid path, 빈 active runs, webhook register+list, skills list, replay nonce 차단). dev-deps `tower` + `http-body-util` 추가.

### TODO 5-2: 에이전트 동작 에러 복구 경로 테스트 — ✅ `9f52ed8`
- **파일**: `src/orchestrator/mod.rs` (#[cfg(test)] 모듈 확장)
- **적용**: `build_failure_recovery_graph` 가 실패 노드 ID·에러·성공 결과를 instructions에 포함, attempt 카운터로 노드 ID 갱신. `build_completion_followup_graph` 의 local-first 힌트 분기 (workspace 작업 vs URL/remote). 4개 테스트.

### TODO 5-3: 동시 실행 테스트 — ✅ `bce85e0`
- **파일**: `tests/api_integration.rs`, `src/session_workspace.rs`
- **적용**: 20개 병렬 POST /v1/sessions 모두 unique ID, 16개 cancel-unknown-run 동시 호출 5xx 없음. 32개 ensure_session_dir 동일 id idempotent + 8 세션 marker.txt 격리 검증. 4개 테스트.

### TODO 5-4: MCP 툴 호출 플로우 테스트 — ✅ `97f2c2f`
- **파일**: `src/orchestrator/tool_augment.rs` (#[cfg(test)] 확장)
- **적용**: malformed JSON / 누락 tool_name / unterminated tag 모두 panic 없음. 빈 McpRegistry call_tool 실패 처리, allowlist 미일치 차단. 6개 테스트.

### TODO 5-5: Coder 백엔드 단위 테스트 — ✅ `664ef75`
- **파일**: `src/orchestrator/coder_backend.rs` (#[cfg(test)] 확장)
- **적용**: tracked 파일 modify 후 `git status --porcelain=v1` modified 분류, ClaudeCodeBackend timeout 시 child kill (200ms timeout, 30s sleep stub → <5s에 Err), 정상 exit_code 반환. 3개 테스트 (Unix만).

### TODO 5-6: 프론트엔드 핵심 경로 E2E 테스트 확장 — ✅ `1d2bec9`
- **파일**: `web/e2e/sessions.spec.ts`, `web/e2e/run-actions.spec.ts`, `web/e2e/settings.spec.ts` (신규)
- **적용**: 세션 페이지 빈 상태 + 신규 생성 흐름 + 상세 navigation, RunActions Cancel 버튼 → POST /v1/runs/:id/cancel, settings 카탈로그 렌더 + provider toggle. Playwright `page.route` 로 backend mock, `npm run test:e2e` 로 실행.

---

## 6. Performance Optimizations (성능 최적화) — ✅ Phase 7 완료 (6-2 deferred)

### TODO 6-1: `record_action_event` 배치 처리 — ✅ `d7ac466`
- **파일**: `src/orchestrator/node_executor.rs` (`build_event_sink`), `src/memory/store.rs` (`batch_insert_run_action_events`, `RunActionEventInput`)
- **적용**: 토큰 chunk 처리 task를 `tokio::select!` (mpsc + 100ms ticker) 로 변경, 50개 또는 100ms마다 단일 트랜잭션으로 flush. 채널 close 시 final drain. 2개 unit 테스트 (empty noop, 7-row monotonic seq).

### TODO 6-2: 과도한 Arc clone 정리 — ⏸️ 보류
- **이유**: 측정 결과 closure 생성 시 Arc clone × 12 = 약 ~130ns. inner 작업이 LLM/IO로 ms-to-s 단위라 의미 있는 효과 없음. 리팩토링 비용 > 실측 이득으로 판단되어 deferred.

### TODO 6-3: DashMap 반복 시 성능 개선 — ✅ `1e9318c`
- **파일**: `src/orchestrator/mod.rs` (`list_recent_runs`, `list_active_runs`, `list_skills`)
- **적용**: `list_recent_runs` O(n*m) any() → HashSet 조회로 O(n+m). 모든 list 함수 `Vec::with_capacity` 사용. `list_skills` 결정적 ID 정렬 추가.

### TODO 6-4: 프론트엔드 이벤트 정렬 최적화 — ✅ `94ce11c`
- **파일**: `web/src/hooks/use-sse.ts`
- **적용**: in-order 도착 시 즉시 push (fast path), out-of-order 시 binary insertion. O(n log n) per chunk → O(n) 최악, append 시 O(1) amortized.

### TODO 6-5: Pause 메커니즘 비효율 개선 — ✅ `df08e2a`
- **파일**: `src/runtime/mod.rs` (`PauseNotify`, `execute_graph_with_pause_notify`), `src/orchestrator/mod.rs` (`RunControl.pause_changed`)
- **적용**: `RunControl` 에 `Arc<tokio::sync::Notify>` 추가, pause/resume/cancel 호출 시 `notify_waiters()`. runtime은 `tokio::time::timeout(1s, notify.notified())`로 변경 (immediate wake + 1s safety net). 기존 execute_graph는 pause_notify=None thin wrapper 유지.

### TODO 6-6: SSE 프론트엔드 재연결 로직 추가 — ✅ `3abeea3`
- **파일**: `src/interface/handlers/runs.rs` (`StreamQuery::after_seq`), `web/src/hooks/use-sse.ts`
- **적용**: backend 가 `?after_seq=N` 쿼리 인식, 그 seq부터 재생. frontend 는 self-restarting loop + exponential backoff (250ms·500·1s·2s·4s·8s cap) + `connectionState` ("connecting"/"open"/"reconnecting"/"closed") + duplicate seq 차단.

### TODO 6-7: 메모리 검색 인덱싱 개선 — ✅ `5376340`
- **파일**: `migrations/0002_memory_search_index.sql` (신규)
- **적용**: `(session_id, updated_at DESC)` 복합 인덱스 추가로 search_memory 의 ORDER BY → 인덱스 스캔. FTS5 가상 테이블은 더 큰 변경이라 별도 phase 로 보류.

---

## 7. Workflow Composition & Requirement Analyzer — ✅ 완료 (F1/F2 잔여)

### 현재 상태
- 워크플로우는 `skills/*.yaml`에 정적 DAG로 정의 (7개 스킬)
- `auto_skill_route()`가 키워드 매칭으로 스킬 선택 (TODO 2-4 word-boundary 적용 완료, 오탐 현저히 감소)
- `build_graph()`가 TaskType별로 하드코딩된 고정 그래프 생성 (9가지 패턴). TODO 2-2 fast-path + 캐시 적용.
- 사용자 요구사항 분석 → 동적 워크플로우 구성 경로 없음
- SubtaskPlan은 Planner LLM 출력에 전적으로 의존, 구조적 검증 부재

### TODO 7-1: 사용자 요구사항 분석기 (Requirement Analyzer) — ✅ shadow mode (`36e5586`)
- **파일**: `src/orchestrator/requirement_analyzer.rs` (신규), `src/orchestrator/run_manager.rs` (shadow 호출)
- **적용**: 휴리스틱-only `analyze(task, primary)` → `RequirementAnalysis { primary_intent, sub_intents, required_capabilities, priority, estimated_complexity, context_requirements }`. classify_task 결과를 echo + 단어 수/키워드 기반으로 보조 정보 산출. shadow mode: SubtaskPlanned 액션 이벤트로만 기록되고 build_graph 동작은 변경 없음. LLM 기반 promotion은 follow-up.
- **테스트**: 8 단위 (simple/long/complex promotion, urgent priority, github/git 캐퍼빌리티, local_repo 컨텍스트, 복합 의도)

### TODO 7-2: 동적 워크플로우 컴포저 (Workflow Composer) — ✅ chain_skills 슬라이스 (`2728564`)
- **파일**: `src/orchestrator/workflow_composer.rs` (신규)
- **적용**: `chain_skills(skills, params, name, description)` 만 1차 구현. 노드 ID `{skill_id}__{node_id}` 네임스페이스, 내부 의존성 재작성, 다음 스킬의 root를 이전 스킬의 terminal에 자동 연결, 파라미터 dedup. `compose(analysis)`/`generate_from_description(LLM)` 은 후속 phase.
- **테스트**: 4 단위 (네임스페이스, 파라미터 dedup, 빈 입력 거부, 다이아몬드 terminal)

### TODO 7-3: SubtaskPlan 구조적 검증 레이어 — ✅ `1dd1513`
- **파일**: `src/orchestrator/completion.rs` (`validate_subtask_plan` + on_completed 호출)
- **적용**: 빈 plan / 캡 초과 / 중복 ID / 빈 필드 / 모르는 의존성 / 사이클 (반복 DFS) 검출. 검증 실패 시 SubtaskPlanned 액션 이벤트 `{rejected: true, reason}` 으로 기록 후 plan 무시.
- **테스트**: 8 단위 (empty/duplicate/unknown-dep/blank-id/cap-exceeded/cycle/static-dep/diamond DAG)

### TODO 7-4: 워크플로우 템플릿 UI — 시각적 편집기 — ✅ Phase 10 (`5a8d51c`)
- **파일**: `web/src/app/workflows/[id]/edit/page.tsx` (신규), `web/src/app/workflows/[id]/page.tsx` (Edit 버튼)
- **적용**: `@xyflow/react` 도입. 자동 BFS layout, 드래그 노드, 클릭 선택 + 사이드바 폼 (id/role/instructions/mcp_tools), Add Node 팔레트 (역할별 컬러), 엣지 드래그 추가, "Save as new workflow" → POST /v1/workflows. 기존 워크플로우를 in-place 수정하는 PATCH 엔드포인트는 follow-up.

### TODO 7-4 원본 설계 참고 (보류 시 사용)
- **파일**: `web/src/app/workflows/` (확장)
- **문제**: 현재 워크플로우 페이지는 목록/실행만 지원. DAG 기반 시각적 편집 불가
- **수정**:
  - 드래그앤드롭 노드 편집기 (react-flow 등 활용)
  - 에이전트 역할 노드 팔레트
  - 의존성 간선 연결
  - 파라미터 바인딩 UI
  - YAML 미리보기 및 내보내기

---

## 8. Agent Execution Tracing & Chat Visibility — ✅ 완료

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

### TODO 8-1: 실시간 DAG 상태 스트리밍 — ✅ `0364081`
- **파일**: `web/src/app/trace/page.tsx` (`useRunSSE` overlay), `web/src/components/trace/dag-graph.tsx`
- **적용**: trace 페이지가 Live ON일 때 useRunSSE를 켜서 `node_started/completed/failed/skipped/dynamic_node_added` 이벤트를 폴링한 스냅샷 위에 overlay. dynamic_node_added는 placeholder로 즉시 삽입. Live 버튼은 SSE connectionState에 따라 "Reconnecting…" 표시.

### TODO 8-2: 채팅 내 에이전트 실행 진행 인라인 카드 — ✅ `b3b0a51`
- **파일**: `web/src/components/agent-thinking.tsx` 확장, `web/src/lib/types.ts`
- **적용**: phase pill (준비/그래프 빌드/실행/검증/재계획/완료) + progress bar (completed/total) + amber recovery alert card (recovery_phase_started ↔ completed) + 누적 token tally. RunActionType union에 recovery_phase_started/completed 추가. Data Flow Card 는 보류 (별도 phase).

### TODO 8-3: 서브태스크 계층 트리 뷰 — ✅ `4fda187`
- **파일**: `web/src/components/trace/subtask-tree.tsx` (신규), `web/src/app/trace/page.tsx` (배치)
- **적용**: dynamic_node_added 의 `from` 필드 (없으면 첫 의존성) 기준으로 부모-자식 트리 구성. static 노드는 top-level, dynamic 은 sub pill 표시. status color · role · duration 표시. Live ON 시 SSE 이벤트 사용.

### TODO 8-4: 노드별 토큰 사용량 및 비용 실시간 추적 — ✅ Stage I + Phase 8 (`6bed393` `d294302` `1453070`)
- **파일**: `src/router/mod.rs`, `src/agents/mod.rs`, `src/runtime/mod.rs`, `src/orchestrator/node_executor.rs`, `src/pricing.rs` (신규), `config/pricing.yaml` (신규), `web/src/components/agent-thinking.tsx`
- **적용 (Stage I)**: 모든 provider (OpenAI/vLLM/Anthropic/Gemini) usage 추출 → `Option<TokenUsage>` 전파. InferenceResult → AgentOutput → NodeExecutionResult → RuntimeEvent::NodeCompleted. 캐시 항목 보존. ModelSelected 페이로드에 누적 usage.
- **적용 (Phase 8)**: `Pricing` 모듈 + `config/pricing.yaml` (override via `CLI_AGENT_PRICING_PATH`). 4-step lookup. RunRecord/SessionSummary/ClusterRunRecord 에 `total_token_usage` + `total_cost_estimate_usd` + `cost_is_estimate`. `agent_runs` 에 `total_input_tokens` / `total_output_tokens` / `total_cost_usd` 컬럼 추가 (migration 0003). `finish_run` 이 per-node cost + run total 계산. ModelSelected 페이로드에 `cost_estimate_usd` 포함. 프론트엔드 phase 배너에 "≈ $0.XXXX est." 표시.

### TODO 8-2 추가: Data Flow Card — ✅ Phase 9 (`e0f886f`)
- **파일**: `web/src/components/agent-thinking.tsx`
- **적용**: 각 노드 카드에 "Inputs from depA: <preview>… · depB: <preview>…" 라인 추가. graph_initialized 의 dependencies + node_completed 의 output_preview 로 데이터 흐름 가시화. 의존성 없거나 preview 없으면 카드 숨김.

### TODO 8-5: 검증/재계획 루프 트레이싱 강화 — ✅ `a39df71`
- **파일**: `src/types.rs` (RunActionType), `src/memory/store.rs` (parser), `src/orchestrator/run_manager.rs` (emit), `src/orchestrator/mod.rs` (trace match arm), `web/src/components/agent-thinking.tsx`
- **적용**: `RunActionType::RecoveryPhaseStarted` / `RecoveryPhaseCompleted` 신설. failure_recovery + completion_continuation 양쪽 사이클에서 ReplanTriggered 와 함께 Started 발행, 종료 시점 (succeeded/exhausted) 에 Completed 발행. 채팅 UI 가 "재계획 시도 N/MAX" 형태로 inline 표시.

---

## 9. Harness Engineering & Sub-Agent Management — ✅ 완료 (orchestrator 가 본격적으로 harness 위에서 동작)

### 현재 상태
- `AgentRegistry`가 11개 역할 에이전트를 관리 (YAML 설정 가능)
- `OrchestratorCluster`가 명명된 오케스트레이터 인스턴스를 멤버로 등록/관리
- `SubtaskPlan` 동적 파싱 → `on_completed` 콜백에서 새 AgentNode를 그래프에 주입 (`src/orchestrator/completion.rs`)
- 에이전트 실행은 `run_role()` / `run_role_stream()` 단일 진입점
- 서브에이전트 라이프사이클 관리 없음 (생성→실행→종료가 일회성)
- 에이전트 간 직접 통신 불가 (오직 의존성 출력을 통한 간접 전달)

### TODO 9-1: Agent Harness 추상 레이어 도입 — ✅ sidecar (`3ce2a2a`)
- **파일**: `src/harness/mod.rs` (신규)
- **적용**: `AgentHarness::{spawn, send, status, terminate, session_tree, sessions}` + `AgentSession { status, total_usage, iteration_count, parent_session, child_sessions, last_output }`. 내부에서 기존 `AgentRegistry::run_role` 을 호출하는 sidecar 형태로, orchestrator 호출 경로는 변경 없음. 4 unit 테스트 (실패 spawn 도 row 기록, unknown send 거부, terminate, session_tree 재귀 빌드). compact_context / context_budget 은 follow-up.
- **원래 설계 참고**:
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

### TODO 9-2: 서브에이전트 스포닝 및 계층 관리 — ✅ Phase 11-C (`dbabd96`)
- **파일**: `src/harness/sub_agent.rs` (신규), `src/orchestrator/completion.rs`
- **적용**: `SubAgentManager::{spawn_from_plan, spawn_child, collect_child_outputs, classify_child_failure, hierarchy}`. `completion.rs` 가 SubtaskPlan 검증 직후 SubAgentManager.spawn_from_plan 으로 자식 harness 세션을 등록. classify_child_failure 임계: 1/n→Continue, ≥n/2→EscalateToPlanner, n/n→AbortParent.

### TODO 9-2 원본 설계 (참고용 archive)
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

### TODO 9-3: 에이전트 간 메시지 버스 — ✅ Phase 11-D (`dbabd96`)
- **파일**: `src/harness/message_bus.rs` (신규), `src/harness/mod.rs`
- **적용**: 사용자 use case ("서브에이전트와의 통신을 주로 사용") 채택. per-session `tokio::sync::mpsc` mailbox (cap 64), AgentHarness::create_session 이 자동 ensure_mailbox, terminate 시 close. 메시지 종류: Query / Feedback / Delegation / Status. ensure_mailbox / send / try_recv / drain / close 5개 메서드. agent 실행 경로에 자동 inject 는 follow-up — 현재는 primitive만 노출.

### TODO 9-3 원본 설계 (참고용 archive)
- **파일**: `src/harness/message_bus.rs` (신규)
- **문제**: 에이전트 간 통신이 오직 dependency_outputs (선행 노드 출력)으로만 가능. 실행 중 에이전트가 다른 에이전트에 질문하거나 피드백을 주고받을 수 없음
- **설계**:
  - `mpsc` 기반 에이전트 간 메시지 채널
  - 메시지 타입: `Query(질문)`, `Feedback(피드백)`, `Delegation(위임)`, `Status(상태 알림)`
  - 수신 에이전트의 컨텍스트에 메시지 자동 주입
  - 타임아웃 및 메시지 큐 크기 제한

### TODO 9-4: 하네스 수준 관측성 (Observability) — ✅ Phase 11-E (`3ce2a2a` `dbabd96`)
- **파일**: `src/harness/metrics.rs` (신규)
- **적용**: `HarnessMetrics::snapshot(&AgentHarness)` — sessions DashMap 을 walk 해서 status별 count, total_iterations, total_tokens, sub_agent_depth, per_role rollup 산출. API 엔드포인트 (`GET /v1/harness/metrics`) + 프론트엔드 위젯은 follow-up.
- **원래 설계 참고**:
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

### TODO 9-5: 에이전트 역할 동적 확장 — ✅ on-demand reload (`3ce2a2a`)
- **파일**: `src/agents/mod.rs` (AgentRegistry 확장)
- **적용**: `agents` 필드를 `Arc<RwLock<HashMap<...>>>` 로 전환, `reload_from_dir(&self, dir)` 신설. Phase 12 `c3dc569` 에서 `POST /v1/agents/reload` API + `Orchestrator::set_agents_dir/reload_agents` 헬퍼 추가. filesystem watcher (notify crate) 는 follow-up.

### TODO 9-6: 하네스 기반 실행 흐름으로 orchestrator 전환 — ✅ Phase 11-A/B (`348a094`)
- **파일**: `src/orchestrator/mod.rs` (harness + root_harness_sessions 필드), `src/orchestrator/run_manager.rs` (root session 생성), `src/orchestrator/node_executor.rs` (per-node child session)
- **적용**: D5 옵션 B 채택. execute_run 시작 시 `harness.create_session(Planner, None)` 으로 root session 생성, `root_harness_sessions: DashMap<run_id, session_id>` 에 저장. build_run_node_fn 의 LLM 스트리밍 경로 직전에 `harness.create_session(role, parent=root)` 으로 자식 세션 생성, run_role_stream 결과에 따라 `record_output` / `record_error`. finish_run 이 root session terminate. 인퍼런스 경로 (run_role_stream 호출, 토큰 스트리밍, ModelSelected) 자체는 변경 없음 — harness 는 metadata 추적 + tree 구조만 담당.

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
4. ~~**Phase 4**: Remaining Refactoring (TODO 3-4 ~ 3-6)~~ — ✅ 완료
5. ~~**Phase 5**: Tests (TODO 5-1 ~ 5-6)~~ — ✅ 완료
6. ~~**Phase 6**: Infrastructure (TODO 4-1 ~ 4-8)~~ — ✅ 완료
7. ~~**Phase 7**: Performance (TODO 6-1 ~ 6-7)~~ — ✅ 완료 (6-2 deferred)
8. ~~**Stage I**: Tracing 백엔드 (TODO 8-4 + 8-5)~~ — ✅ 완료
9. ~~**Stage II**: Tracing 프론트 (TODO 8-1 + 8-2 + 8-3)~~ — ✅ 완료
10. ~~**Stage III**: Workflow 백엔드 (TODO 7-3 + 7-1 + 7-2 chain)~~ — ✅ 완료
11. ~~**Stage IV**: Harness sidecar (TODO 9-1 + 9-4 + 9-5)~~ — ✅ 완료
12. ~~**Phase 8**: Cost Tracking (TODO 8-4 가격표)~~ — ✅ 완료
13. ~~**Phase 9**: Data Flow Card (TODO 8-2 추가 슬라이스)~~ — ✅ 완료
14. ~~**Phase 10**: Visual Workflow Editor (TODO 7-4)~~ — ✅ 완료
15. ~~**Phase 11**: Harness 본격 전환 (D5 + D3 + D4 + F4 = TODO 9-6 + 9-2 + 9-3 + HarnessMetrics API)~~ — ✅ 완료
16. ~~**Phase 12**: F5 hot reload + F2 parser groundwork~~ — ✅ 완료

**잔여 — production 운영 또는 추가 합의 후**:
- F1: RequirementAnalyzer shadow → driver promotion (1주 production SubtaskPlanned 이벤트 분포 누적 후 의사결정)
- F2: WorkflowComposer.generate_from_description LLM wrapper (캐시 정책 + fallback 전략 합의 후)
- F5(b): notify crate 기반 filesystem watcher 자동 reload

---

## 핵심 파일 참조 (2026-04-27 기준)

| 파일 | 줄 수 | 비고 |
|------|-------|------|
| `src/orchestrator/mod.rs` | ~3,100 | 7,654 → ~3,100 (Phase 2 분할 + Phase 5/7 추가) |
| `src/orchestrator/node_executor.rs` | ~2,000 | build_run_node_fn + event_sink (token chunk 배치 추가) |
| `src/orchestrator/graph_builder.rs` | 652 | build_graph + recovery/followup |
| `src/orchestrator/task_classifier.rs` | 544 | classify_task + fast-path + follow-up |
| `src/orchestrator/helpers.rs` | 467 | 파싱/포맷 순수 유틸 + contains_word |
| `src/orchestrator/run_manager.rs` | ~480 | submit_run, execute_run, run_and_wait, pause_notify |
| `src/orchestrator/settings.rs` | 265 | AppSettings |
| `src/orchestrator/skill_router.rs` | 182 | auto_skill_route |
| `src/orchestrator/coder_backend.rs` | ~680 | git diff 감지 + kill + unit tests |
| `src/interface/api.rs` | ~330 | router + serve + CORS allowlist + body limit + graceful shutdown |
| `src/interface/rate_limit.rs` | 213 | Phase 6 신규: per-key 토큰 버킷 |
| `src/interface/handlers/runs.rs` | ~700 | 15개 run 엔드포인트 (after_seq 추가) |
| `src/interface/handlers/health.rs` | 56 | Phase 6 신규: /health 라이브니스 |
| `src/interface/handlers/memory.rs` | 393 | 7개 메모리 |
| `src/interface/handlers/terminal.rs` | 277 | 4개 PTY + WS |
| `src/interface/handlers/webhooks.rs` | ~210 | DTO validator 추가 |
| `src/context/mod.rs` | 354 | estimate_tokens 수정 완료 |
| `src/runtime/mod.rs` | ~810 | execute_graph + execute_graph_with_pause_notify |
| `src/memory/store.rs` | ~2,140 | sqlx::migrate! + RunActionEventInput + 암호화 |
| `src/memory/mod.rs` | ~700 | record_node_event + spawn_short_term_gc |
| `src/terminal/mod.rs` | 241 | Phase 1 poisoning 처리 완료 |
| `src/crypto.rs` | ~370 | SignatureVerifier + AES-256-GCM secret encryption |
| `migrations/0001_initial_schema.sql` | 167 | Phase 6 신규: 초기 스키마 |
| `migrations/0002_memory_search_index.sql` | 6 | Phase 7 신규: search index |
| `tests/api_integration.rs` | ~340 | 14개 integration 테스트 |
| `web/src/lib/config.ts` | 8 | API 설정 단일 소스 |
| `web/src/hooks/use-sse.ts` | ~150 | 재연결 + after_seq + connection state |
| `web/e2e/{sessions,run-actions,settings}.spec.ts` | — | Phase 5 신규 Playwright |
| `src/orchestrator/requirement_analyzer.rs` | ~190 | Stage III 신규: 휴리스틱 분석기 (shadow mode) |
| `src/orchestrator/workflow_composer.rs` | ~310 | Stage III/Phase 12: chain_skills + parse_skill_chain_response |
| `src/harness/mod.rs` | ~360 | Stage IV/Phase 11: AgentHarness sidecar + bus + create/record helpers |
| `src/harness/metrics.rs` | ~180 | Stage IV 신규: HarnessMetrics snapshot |
| `src/harness/sub_agent.rs` | ~290 | Phase 11-C 신규: SubAgentManager + classify_child_failure |
| `src/harness/message_bus.rs` | ~210 | Phase 11-D 신규: per-session mpsc mailbox |
| `src/pricing.rs` | ~270 | Phase 8 신규: Pricing + ModelPrice + parse helpers |
| `src/interface/handlers/harness.rs` | 32 | Phase 11-E 신규: GET /v1/harness/metrics |
| `src/interface/handlers/agents.rs` | 41 | Phase 12 신규: POST /v1/agents/reload |
| `config/pricing.yaml` | 71 | Phase 8 신규: per-million USD 가격표 |
| `migrations/0003_run_cost_columns.sql` | 9 | Phase 8 신규: agent_runs 비용 컬럼 |
| `web/src/components/trace/subtask-tree.tsx` | ~210 | Stage II 신규: 서브태스크 계층 트리 |
| `web/src/app/workflows/[id]/edit/page.tsx` | ~430 | Phase 10 신규: react-flow 시각 편집기 |
| `web/src/app/harness/page.tsx` | ~140 | Phase 11-E 신규: 하네스 메트릭 대시보드 |
| `docs/SECTION_7_9_PREP.md` | 238 | Section 7~9 사전 정리 브리프 |

---

## 커밋 레퍼런스 (78 commits)

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

### Phase 4 Remaining Refactoring
- `a1e52f2` refactor: Consolidate frontend API config into lib/config.ts
- `3775b09` refactor: Collapse paired session + action event writes
- `74b2089` refactor: Extract signature verification into crypto module

### Phase 5 Test Coverage
- `da8a189` test: Add HTTP integration tests for interface::api router
- `9f52ed8` test: Cover failure recovery and completion follow-up graph builders
- `bce85e0` test: Cover concurrent session, run-control, and workspace paths
- `97f2c2f` test: Cover MCP tool extract / execute failure paths
- `664ef75` test: Cover ClaudeCodeBackend timeout-kill and modified-file detection
- `1d2bec9` test: Add Playwright specs for sessions, run actions, and settings

### Phase 6 Infrastructure
- `7451d8d` feat: Add token-bucket rate limiter middleware on the API router
- `1bd84d4` feat: Honor CLI_AGENT_ALLOWED_ORIGINS env var for CORS allowlist
- `8e03adb` feat: Add 2 MiB body limit and webhook DTO validation
- `afbc40b` feat: Add /health (and /v1/health) liveness + readiness endpoint
- `35a6a63` feat: Graceful shutdown — cancel in-flight runs on SIGINT/SIGTERM
- `59ecfe7` refactor: Switch DB schema bootstrap to sqlx::migrate!
- `8ad41c8` feat: Periodically GC expired short-term memory items
- `5eb2ae3` feat: Encrypt webhook secrets at rest with AES-256-GCM

### Phase 7 Performance
- `d7ac466` perf: Batch high-frequency token-chunk INSERTs in the event sink
- `1e9318c` perf: HashSet-based dedup in list_recent_runs + pre-allocated Vec
- `94ce11c` perf: Append-fast / binary-insert for SSE event accumulation
- `df08e2a` perf: Wake the run loop via Notify on pause/resume instead of poll
- `3abeea3` feat: SSE reconnection with after_seq resume + connection state
- `5376340` perf: Add (session_id, updated_at DESC) index for memory search
- (TODO 6-2 deferred — measured Arc clone cost is dominated by LLM/IO)

### Stage I Tracing 백엔드 (TODO 8-4 + 8-5)
- `e0f310d` docs: Add Section 7~9 prep brief
- `6bed393` feat: Capture provider token usage on InferenceResult
- `d294302` feat: Propagate token usage through Agent → Node → RuntimeEvent
- `a39df71` feat: Emit RecoveryPhaseStarted/Completed action events around recovery loop

### Stage II Tracing 프론트 (TODO 8-1 + 8-2 + 8-3)
- `0364081` feat: Live SSE overlay on the trace DAG graph
- `b3b0a51` feat: Phase indicator + progress bar + recovery alert in AgentThinking
- `4fda187` feat: SubtaskTree component for the trace page

### Stage III Workflow 백엔드 (TODO 7-3 + 7-1 + 7-2 chain)
- `1dd1513` feat: Validate Planner SubtaskPlan structurally before applying
- `36e5586` feat: Heuristic RequirementAnalyzer (shadow mode, TODO 7-1)
- `2728564` feat: WorkflowComposer.chain_skills sequential composer

### Stage IV Harness sidecar (TODO 9-1 + 9-4 + 9-5)
- `3ce2a2a` feat: AgentHarness sidecar + HarnessMetrics + AgentRegistry hot reload

### Phase 8 Cost Tracking (TODO 8-4 가격표)
- `1453070` feat(phase8): Cost tracking — per-run/session/cluster + UI banner

### Phase 9 Data Flow Card (TODO 8-2 추가 슬라이스)
- `e0f886f` feat(phase9): Data Flow Card — show upstream node outputs inline

### Phase 10 Visual Workflow Editor (TODO 7-4)
- `5a8d51c` feat(phase10): Visual workflow editor (react-flow)

### Phase 11 Harness 본격 전환 (D5 + D3 + D4 + F4)
- `348a094` feat(phase11ab): Wire AgentHarness into the run lifecycle
- `dbabd96` feat(phase11cde): SubAgentManager + message bus + /v1/harness/metrics

### Phase 12 Reload + parser groundwork (F5 + F2 일부)
- `c3dc569` feat(phase12): /v1/agents/reload + parse_skill_chain_response helper
