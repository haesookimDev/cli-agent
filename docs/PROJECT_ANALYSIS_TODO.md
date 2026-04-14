# CLI-Agent 프로젝트 종합 분석 및 TODO

## Context

Rust + Next.js 기반 멀티에이전트 오케스트레이터 CLI/TUI 플랫폼의 전반적인 코드 품질, 에이전트 동작 결함, 리팩토링 필요사항을 종합 분석한 결과이다.
현재 82개 테스트 통과, 빌드 정상이지만 핵심 모듈의 비대화, 에이전트 동작의 취약한 휴리스틱, 에러 핸들링 공백, 테스트 커버리지 부족 등 다수의 개선 포인트가 존재한다.

---

## 1. Critical Fixes (안정성/정확성)

### TODO 1-1: `serde_json::to_value().unwrap()` 제거 — API 서버 패닉 방지
- **파일**: `src/interface/api.rs` (30+ 개소)
- **문제**: API 핸들러에서 `serde_json::to_value(...).unwrap()` 패턴이 30곳 이상 사용됨. 직렬화 실패 시 서버 전체 패닉
- **수정**: 모든 `.unwrap()`를 `match` 또는 `?` 연산자로 교체, `StatusCode::INTERNAL_SERVER_ERROR` 반환
- **예시 위치**: api.rs:344, 372, 404, 480, 519, 562, 757, 887, 924, 947, 978 등

### TODO 1-2: 토큰 추정 함수 버그 수정
- **파일**: `src/context/mod.rs:255`
- **문제**: `split_whitespace().count() * 1.3`은 특수문자, 구두점, 유니코드(한국어 등)의 토큰 수를 심각하게 과소평가함
- **수정**: 문자 수 기반 보정 추가 (`max(whitespace * 1.3, char_count / 3.5)` 등) 또는 tiktoken 호환 근사치 적용
- **영향**: 컨텍스트 오버플로우로 중요한 내용이 잘리는 현상 발생 가능

### TODO 1-3: Coder 백엔드 타임아웃 시 자식 프로세스 미종료
- **파일**: `src/orchestrator/coder_backend.rs:171-174`
- **문제**: `tokio::time::timeout(self.timeout, child.wait())` 타임아웃 시 child 프로세스를 kill하지 않음. 좀비 프로세스 누적 가능
- **수정**: 타임아웃 발생 시 `child.kill().await` 호출 추가

### TODO 1-4: `std::sync::Mutex` async 컨텍스트 데드락 위험
- **파일**: `src/orchestrator/mod.rs:1588-1596, 4454-4457`
- **문제**: `static_node_ids`가 `std::sync::Mutex`로 감싸져 있으며, 메인 루프(1617)와 on_completed 콜백(4455) 양쪽에서 lock. 비동기 스케줄러에서 교착 가능
- **수정**: `tokio::sync::Mutex`로 교체하거나, lock 범위를 최소화하여 즉시 clone 후 drop

### TODO 1-5: `terminal/mod.rs` Mutex poisoning 패닉
- **파일**: `src/terminal/mod.rs:143`, `src/interface/api.rs:2507`
- **문제**: `scrollback.lock().unwrap()` — poisoned mutex 시 패닉
- **수정**: `lock().unwrap_or_else(|e| e.into_inner())` 패턴 사용

### TODO 1-6: `record_action_event` 에러 무시 → 최소한 로깅 추가
- **파일**: `src/orchestrator/mod.rs:1252-1264` (및 run_node_fn 내 ~15곳)
- **문제**: `let _ = self.memory.append_run_action_event(...)` 로 에러 완전 무시. 텔레메트리 데이터 유실 시 디버깅 불가
- **수정**: `let _ =` 대신 `if let Err(e) = ... { tracing::warn!("...") }` 패턴으로 교체

### TODO 1-7: Coder 백엔드 파일 변경 감지 미구현
- **파일**: `src/orchestrator/coder_backend.rs:73-78, 179-184`
- **문제**: `ClaudeCodeBackend`와 `CodexBackend` 모두 `files_changed: vec![]` 하드코딩. 후속 git commit/validation 노드가 변경된 파일을 알 수 없음
- **수정**: 프로세스 실행 전 working_dir의 `git status` 스냅샷 → 실행 후 diff로 변경 파일 탐지. ClaudeCode는 `stream-json` 출력에서 파일 변경 이벤트 파싱

---

## 2. Agent Behavior Improvements (에이전트 동작 개선)

### TODO 2-1: `verify_completion` — 모호한 리뷰어 응답 처리 개선
- **파일**: `src/orchestrator/mod.rs:1865-1896`
- **문제**: Reviewer 출력이 "COMPLETE"/"INCOMPLETE" 접두어로 시작하지 않으면 무조건 incomplete 처리 → 불필요한 re-planning 루프 유발
- **수정**:
  - 구조화된 JSON 응답 형식 (`{"status": "complete"|"incomplete", "reason": "..."}`) 요구
  - JSON 파싱 실패 시 키워드 기반 fallback ("satisfied", "done", "completed" 등)
  - 모호한 응답에 대해 1회 재시도 후 판단

### TODO 2-2: `classify_task` — 매번 LLM 호출하는 비효율 제거
- **파일**: `src/orchestrator/mod.rs:2157-2261`
- **문제**: 모든 태스크에 대해 LLM 분류 호출 수행. 캐싱 없음
- **수정**:
  - 키워드 기반 분류를 1차 필터로 승격 (현재는 fallback으로만 사용)
  - 명확한 패턴 매칭되면 LLM 호출 skip
  - LLM 분류 결과 세션 내 캐싱

### TODO 2-3: `classify_task_fallback` — 오분류 문제
- **파일**: `src/orchestrator/mod.rs:2265-2315`
- **문제**: `"setting"` 키워드만으로 Configuration 분류 → "debugging a settings display bug" 같은 코딩 태스크를 오분류
- **수정**: 키워드 매칭에 컨텍스트 가중치 추가. "fix" + "setting" → CodeGeneration, "change" + "setting" → Configuration 등

### TODO 2-4: `auto_skill_route` — 스킬 ID 부분 문자열 매칭의 오탐
- **파일**: `src/orchestrator/mod.rs:259-268`
- **문제**: `lower.contains(skill.id.to_lowercase().as_str())` — 스킬 ID가 짧으면 관련 없는 텍스트에서도 매칭
- **수정**: 단어 경계 기반 매칭 또는 정규식 사용

### TODO 2-5: `looks_like_follow_up_task` — 5토큰 이하 무조건 후속 판정
- **파일**: `src/orchestrator/mod.rs:1965-1988`
- **문제**: "Fix this bug" (3토큰) 같은 독립 태스크를 후속 질문으로 오판
- **수정**: 토큰 수 + 대명사/지시어 존재 여부 조합 조건으로 변경

### TODO 2-6: ToolCaller 반복 루프 상한 부재
- **파일**: `src/orchestrator/mod.rs:3319-3774`
- **문제**: ToolCaller의 iteration 루프에 명시적 상한 없이 타임아웃에만 의존
- **수정**: `const MAX_TOOL_ITERATIONS: usize = 10;` 같은 하드 리밋 추가

### TODO 2-7: MCP 개별 툴 호출 타임아웃 부재
- **파일**: `src/orchestrator/mod.rs:3692`
- **문제**: `mcp.call_tool()` 에 개별 타임아웃 없음. 행이 걸린 MCP 서버가 전체 에이전트를 블록
- **수정**: `tokio::time::timeout(Duration::from_secs(60), mcp.call_tool(...))` 래핑

---

## 3. Code Refactoring (유지보수성)

### TODO 3-1: `orchestrator/mod.rs` 분할 — 7,552줄 모놀리스 해체
- **파일**: `src/orchestrator/mod.rs`
- **문제**: 단일 파일에 태스크 분류, 그래프 빌딩, 실행 루프, 검증, 노드 실행, 설정 관리, 메모리 쿼리, 스킬 라우팅 등 모든 로직이 혼재
- **수정 계획**:
  ```
  src/orchestrator/
  ├── mod.rs              (Orchestrator 구조체 + 위임만, ~200줄)
  ├── run_manager.rs      (submit_run, execute_run, run_and_wait, control 로직)
  ├── graph_builder.rs    (build_graph, build_recovery_graph, build_completion_followup_graph)
  ├── task_classifier.rs  (classify_task, classify_task_fallback, is_continuation_command 등)
  ├── node_executor.rs    (build_run_node_fn — 현재 1,500줄 클로저를 모듈화)
  ├── completion.rs       (build_on_completed_fn, verify_completion)
  ├── settings.rs         (get_settings, update_settings, apply_settings_to_runtime)
  ├── context_builder.rs  (build_memory_query, build_recent_history_chunks, build_recent_run_summary)
  ├── skill_router.rs     (auto_skill_route, extract helpers)
  └── helpers.rs          (trim_for_context, clean_llm_output, extract_repo_url 등)
  ```

### TODO 3-2: `interface/api.rs` 도메인별 분할 — 2,683줄
- **파일**: `src/interface/api.rs`
- **수정 계획**:
  ```
  src/interface/
  ├── api.rs              (라우터 조립 + 공통 미들웨어만)
  ├── handlers/
  │   ├── sessions.rs     (세션 CRUD)
  │   ├── runs.rs         (실행 제어, 트레이스, 행동 분석)
  │   ├── memory.rs       (세션/글로벌 메모리)
  │   ├── webhooks.rs     (웹훅 등록/배달)
  │   ├── workflows.rs    (워크플로우 CRUD/실행)
  │   ├── settings.rs     (설정 조회/변경)
  │   └── terminal.rs     (터미널 세션/WebSocket)
  ```

### TODO 3-3: `NodeExecutionResult` 생성 중복 제거
- **파일**: `src/orchestrator/mod.rs` (run_node_fn 내 ~15회 중복)
- **문제**: 성공/실패 결과 생성 패턴이 15곳에서 반복
- **수정**: 헬퍼 함수 도입
  ```rust
  fn node_success(node: &AgentNode, model: &str, output: String, started: Instant) -> NodeExecutionResult
  fn node_failure(node: &AgentNode, model: &str, error: String, started: Instant) -> NodeExecutionResult
  ```

### TODO 3-4: 프론트엔드 API 설정 중복 통합
- **파일**: `web/src/lib/api-client.ts:3-5`, `web/src/lib/sse.ts:3-5`, `web/src/hooks/use-terminal-ws.ts:15-17`
- **문제**: 3곳에서 동일한 `API_KEY`, `API_SECRET`, `API_URL` 하드코딩
- **수정**: `web/src/lib/config.ts`로 추출하여 단일 소스 관리

### TODO 3-5: Discord/Slack 게이트웨이 서명 검증 추상화
- **파일**: `src/gateway/slack.rs:51-87`, `src/gateway/discord.rs`, `src/webhook/mod.rs:44-80`
- **문제**: 유사한 서명 검증 로직이 3곳에 분산
- **수정**: `crypto::SignatureVerifier` 트레이트 도입, HMAC/Ed25519 구현체 분리

### TODO 3-6: `record_action_event` + `record_node_progress` 쌍 호출 통합
- **파일**: `src/orchestrator/mod.rs` (build_run_node_fn 내 ~16회 반복)
- **문제**: 거의 동일한 payload로 두 함수를 연달아 호출하는 패턴 반복
- **수정**: `record_node_event()` 통합 헬퍼 도입, 내부에서 양쪽 모두 호출

---

## 4. Infrastructure Improvements (인프라 개선)

### TODO 4-1: API 레이트 리미팅 추가
- **파일**: `src/interface/api.rs` (라우터 설정)
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

## 5. Test Coverage Expansion (테스트 커버리지)

### TODO 5-1: API 핸들러 통합 테스트 추가
- **파일**: `tests/api_integration.rs` (신규)
- **범위**: 세션 CRUD, 실행 제출/취소/일시정지/재개, 메모리 CRUD, 웹훅 등록/배달
- **방법**: `axum::test` 또는 `reqwest` + 인메모리 서버

### TODO 5-2: 에이전트 동작 에러 복구 경로 테스트
- **파일**: `src/orchestrator/mod.rs` (#[cfg(test)] 확장)
- **범위**:
  - 노드 실패 → recovery graph 빌드 → 재실행 플로우
  - 검증 incomplete → continuation graph → 최대 횟수 후 실패
  - 동일 failure_class 연속 2회 → 즉시 실패 전환

### TODO 5-3: 동시 실행 테스트
- **범위**: 다중 run 병렬 실행 시 DashMap 경합, 취소/일시정지 동시 요청, 워크스페이스 격리

### TODO 5-4: MCP 툴 호출 플로우 테스트
- **범위**: 정상 호출, 타임아웃, 서버 응답 없음, 잘못된 JSON 응답 등

### TODO 5-5: Coder 백엔드 단위 테스트
- **파일**: `src/orchestrator/coder_backend.rs`
- **범위**: LLM/ClaudeCode/Codex 각 백엔드의 정상/실패/타임아웃 경로

### TODO 5-6: 프론트엔드 핵심 경로 E2E 테스트 확장
- **파일**: `web/e2e/`
- **범위**: 채팅 전송→응답 수신, 실행 취소, 세션 전환, 설정 변경

---

## 6. Performance Optimizations (성능 최적화)

### TODO 6-1: `record_action_event` 배치 처리
- **파일**: `src/orchestrator/mod.rs` — 전체에 산재
- **문제**: 모든 이벤트를 개별 DB INSERT로 기록. 고빈도 이벤트(token_chunk 등)에서 DB 부하
- **수정**: mpsc 채널 기반 배치 INSERT (100ms 또는 50건 단위 flush)

### TODO 6-2: 과도한 Arc clone 정리
- **파일**: `src/orchestrator/mod.rs:2768-2793` (build_run_node_fn)
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

## 검증 계획

### 단계별 검증
1. **빌드 검증**: `cargo build && cargo test` — 82+ 테스트 통과
2. **프론트엔드 빌드**: `cd web && npm run build` — 정상 컴파일
3. **API 기능 테스트**: 통합 테스트 추가 후 `cargo test --test api_integration`
4. **에이전트 동작 검증**: 시나리오별 수동 테스트
   - 모호한 Reviewer 응답 → 올바른 재시도 동작
   - 키워드 분류 경합 케이스 → 정확한 TaskType 판정
   - ToolCaller 루프 상한 도달 시 정상 종료
5. **부하 테스트**: 동시 10 run 실행 시 패닉/데드락 없음 확인

### 구현 순서 (권장)
1. **Phase 1**: Critical Fixes (TODO 1-1 ~ 1-7) — 안정성 확보
2. **Phase 2**: Core Refactoring (TODO 3-1 ~ 3-3) — 7,552줄 모놀리스 분할 (이후 작업의 전제)
3. **Phase 3**: Agent Behavior (TODO 2-1 ~ 2-7) — 동작 정확성
4. **Phase 4**: Remaining Refactoring (TODO 3-4 ~ 3-6) — 중복 제거
5. **Phase 5**: Tests (TODO 5-1 ~ 5-6) — 회귀 방지
6. **Phase 6**: Infrastructure (TODO 4-1 ~ 4-8) — 운영 안정성
7. **Phase 7**: Performance (TODO 6-1 ~ 6-7) — 최적화

---

## 핵심 파일 참조

| 파일 | 줄 수 | 주요 변경 사항 |
|------|-------|--------------|
| `src/orchestrator/mod.rs` | 7,552 | 분할 리팩토링 + 에이전트 동작 수정 |
| `src/interface/api.rs` | 2,683 | unwrap 제거 + 도메인 분할 |
| `src/context/mod.rs` | ~200 | 토큰 추정 버그 수정 |
| `src/runtime/mod.rs` | ~600 | pause 메커니즘 개선 |
| `src/orchestrator/coder_backend.rs` | ~400 | 프로세스 kill + 테스트 추가 |
| `src/memory/store.rs` | 2,094 | 마이그레이션 시스템 + 암호화 |
| `src/memory/mod.rs` | ~250 | 단기 메모리 GC |
| `src/terminal/mod.rs` | ~200 | Mutex poisoning 처리 |
| `web/src/lib/config.ts` | 신규 | API 설정 통합 |
| `web/src/components/agent-thinking.tsx` | ~600 | 이벤트 정렬 최적화 |
