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

## 7. Workflow Composition & Requirement Analyzer (워크플로우 구성 및 요구사항 분석기)

### 현재 상태
- 워크플로우는 `skills/*.yaml`에 정적 DAG로 정의 (7개 스킬)
- `auto_skill_route()`가 키워드 매칭으로 스킬 선택 (오탐 위험, TODO 2-4 참조)
- `build_graph()`가 TaskType별로 하드코딩된 고정 그래프 생성 (9가지 패턴)
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
- **관련 파일**: `src/orchestrator/mod.rs` (classify_task, build_graph 호출부)

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
- **파일**: `src/orchestrator/mod.rs:4392-4397` (on_completed 내 SubtaskPlan 파싱 부분)
- **문제**: Planner LLM이 생성한 SubtaskPlan JSON을 파싱만 하고 구조적 검증 없이 실행. 순환 의존, 없는 역할 참조, 과도한 서브태스크 등 검증 부재
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

## 8. Agent Execution Tracing & Chat Visibility (에이전트 실행 트레이싱 & 채팅 가시성)

### 현재 상태
- `RuntimeEvent` 13종 이벤트가 `EventSink` 콜백으로 발행됨
- `record_action_event` / `record_node_progress`로 DB 기록
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
- **파일**: `src/orchestrator/mod.rs:1719-1786` (verify → replan 루프)
- **문제**: continuation/recovery 루프의 진행 상태가 `ReplanTriggered` 이벤트 하나로만 기록. 몇 번째 시도인지, 왜 재계획인지 채팅에서 직관적으로 보이지 않음
- **수정**:
  - 새 이벤트 타입: `RunActionType::RecoveryPhaseStarted`, `RecoveryPhaseCompleted`
  - 페이로드에 `attempt`, `max_attempts`, `reason`, `mode(failure_recovery|completion_continuation)` 포함
  - 채팅 UI에 "재계획 시도 2/2: 검증 실패 — 미완성 항목 존재" 같은 명시적 상태 표시

---

## 9. Harness Engineering & Sub-Agent Management (하네스 엔지니어링 & 서브에이전트 관리)

### 현재 상태
- `AgentRegistry`가 11개 역할 에이전트를 관리 (YAML 설정 가능)
- `OrchestratorCluster`가 명명된 오케스트레이터 인스턴스를 멤버로 등록/관리
- `SubtaskPlan` 동적 파싱 → `on_completed` 콜백에서 새 AgentNode를 그래프에 주입
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
- **기존 연동**: `build_on_completed_fn`의 SubtaskPlan 처리 로직을 SubAgentManager로 위임

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
- **파일**: `src/orchestrator/mod.rs` (execute_run, build_run_node_fn)
- **문제**: 현재 `build_run_node_fn`이 직접 `agents.run_role()`을 호출. 하네스 계층을 거치지 않아 세션 관리, 컨텍스트 축적, 계층 추적 등이 불가능
- **수정**:
  - `build_run_node_fn` 내부에서 `harness.spawn()` → `harness.send()` → `harness.terminate()` 패턴으로 전환
  - run 시작 시 루트 하네스 세션 생성
  - 각 노드 실행을 자식 세션으로 관리
  - 기존 `AgentRegistry.run_role()` 직접 호출 경로는 하네스 내부로 캡슐화

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
