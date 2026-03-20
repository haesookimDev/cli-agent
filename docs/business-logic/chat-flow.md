# Chat Flow: 입력→처리→출력 완전 흐름

채팅 메시지가 입력되어 화면에 출력되기까지의 11단계 전체 흐름.

---

## 단계 1: 프론트엔드 입력

**파일**: `web/src/app/chat/page.tsx` (lines 236-278)

사용자가 채팅창에 메시지를 입력하고 전송:

1. 사용자가 텍스트 입력 + `TaskProfile` 선택 (`general` / `planning` / `extraction` / `coding`)
2. 전송 버튼 클릭 → `handleSubmit()` 실행
3. 낙관적 UI 업데이트: 사용자 메시지를 즉시 채팅에 추가 (pending 상태)
4. API 호출:
   ```typescript
   apiPost<RunSubmission>("/v1/runs", {
     task: userMessage,
     profile: selectedProfile,     // "general" | "planning" | "extraction" | "coding"
     session_id: activeSessionId,  // 없으면 서버에서 신규 생성
   })
   ```

---

## 단계 2: HMAC-SHA256 인증

**파일**: `web/src/lib/api-client.ts`

모든 API 호출은 `apiPost()` / `apiGet()` 헬퍼를 통과:

```typescript
const timestamp = Date.now().toString()
const nonce = crypto.randomUUID()
const rawBody = JSON.stringify(body)
const message = `${timestamp}.${nonce}.${rawBody}`
const signature = HMAC-SHA256(AGENT_API_SECRET, message)  // hex 인코딩

headers: {
  "Content-Type": "application/json",
  "X-API-Key": AGENT_API_KEY,
  "X-Signature": signature,
  "X-Timestamp": timestamp,
  "X-Nonce": nonce,
}
```

---

## 단계 3: API 수신 — POST /v1/runs

**파일**: `src/interface/api.rs` (lines 875-916)

백엔드 Axum 핸들러 `create_run_handler()`:

1. `state.auth.verify_headers(headers, body)` — HMAC 서명 검증
2. `CreateRunRequest` 파싱:
   ```rust
   CreateRunRequest {
     task: String,
     profile: Option<TaskProfile>,
     session_id: Option<Uuid>,
     repo_url: Option<String>,
   }
   ```
3. `orchestrator.submit_run(RunRequest)` 호출
4. **즉시 HTTP 202 반환**: `RunSubmission { run_id, session_id, status: Queued }`

프론트엔드는 202를 받자마자 SSE 연결을 시작함.

---

## 단계 4: 오케스트레이터 — 비동기 처리 시작

**파일**: `src/orchestrator/mod.rs` (lines 716-803)

`submit_run()` 내부:

1. `run_id = Uuid::new_v4()`, `session_id` 확인/생성
2. **자동 스킬 라우팅 체크**: 태스크 텍스트가 등록된 스킬 키워드와 매칭되는지 확인
   - "clone", "overview", "analyze repo" 등 → 해당 스킬 자동 실행
3. `RunRecord` 생성 → DashMap에 삽입 + SQLite 저장
4. `RunControl { cancel: AtomicBool, pause: AtomicBool }` 초기화
5. **`tokio::spawn(execute_run(run_id, req))`** — 백그라운드 태스크 스폰
6. 즉시 `RunSubmission` 반환 (submit_run 종료)

---

## 단계 5: 태스크 분류

**파일**: `src/orchestrator/mod.rs` (lines 1982-2101)

백그라운드에서 `execute_run()` 시작:

1. **Planner 에이전트**가 태스크 텍스트를 LLM으로 분류:
   - `SimpleQuery` — 단순 질문/답변
   - `Analysis` — 데이터 추출, 패턴 분석
   - `CodeGeneration` — 코드 작성, 버그 수정
   - `Configuration` — 시스템 설정 변경
   - `ConfigQuery` — 설정 조회
   - `ToolOperation` — MCP 툴 호출 필요
   - `ExternalProject` — repo_url 포함
   - `Interactive` — 멀티스텝 탐색
   - `Complex` — 여러 카테고리 복합
2. **LLM 불가 시 키워드 폴백** (lines 2090-2101):
   - "code", "implement", "fix" → `CodeGeneration`
   - "analyze", "extract" → `Analysis`
   - "config", "set" → `Configuration`
   - 기타 → `SimpleQuery`

---

## 단계 6: DAG 그래프 빌딩

**파일**: `src/orchestrator/mod.rs` (lines 2187-2415)

분류된 `TaskType`에 따라 `ExecutionGraph` 빌드:

| TaskType | 노드 구성 |
|----------|----------|
| SimpleQuery | `plan` → `summarize` |
| Analysis | `plan` → `extract` → `analyze` → `summarize` |
| Configuration | `plan` → `config_manage` → `summarize` |
| ToolOperation | `plan` → `tool_call` → `summarize` |
| CodeGeneration | `plan` → `extract` → `context_probe` → `code` → `fallback` → `summarize` |
| Complex | `plan` → (동적 서브태스크 노드들) → `summarize` |

각 노드에는 `AgentRole`, `task`, `instructions`, `dependencies`, `ExecutionPolicy` 포함.

---

## 단계 7: 에이전트 실행

**파일**: `src/agents/mod.rs`, `src/runtime/mod.rs`

`AgentRuntime.execute_graph()` 실행:

```
의존성 위상 정렬 → 레벨별 병렬 실행

for each level in topological_order:
  tokio::spawn으로 병렬 실행 (최대 4개)

  각 노드 실행:
  1. MemoryManager.retrieve_relevant() → 관련 메모리 조회
  2. ContextManager.build_context() → 토큰 예산 내 컨텍스트 구성
  3. AgentInput 구성:
     {
       task: node.task,
       instructions: node.instructions,
       context: retrieved_memories,
       dependency_outputs: completed_nodes_outputs,
       working_dir: session_workspace_path,
     }
  4. AgentRegistry.run_role(role, input) 실행
```

**에이전트 프롬프트 구성** (`src/orchestrator/prompt_composer.rs`):
```
[시스템] 역할별 프롬프트 (Planner/Coder/Analyzer...)
[사용자] 태스크 설명
[컨텍스트] 관련 메모리
[선행 출력] 의존 노드 결과
[지시사항] 추가 instructions
```

---

## 단계 8: 모델 라우팅 & LLM 호출

**파일**: `src/router/mod.rs`

`ModelRouter.infer_stream_in_dir_with_cli_output()`:

1. `TaskProfile` → `RoutingConstraints` 조회 (quality/latency/cost/context 제약)
2. 가용 모델 스코어링:
   ```
   score = quality_weight * quality + latency_weight * (1 - latency/max) + cost_weight * (1 - cost/max)
   ```
3. 최고 점수 모델 선택, 응답 캐시 확인 (TTL 내 동일 프롬프트)
4. `CLI_ONLY=true`이면 Claude Code / Codex CLI 강제 사용
5. 스트리밍 추론 실행 → `TokenCallback`으로 토큰별 콜백
6. CLI 실행 시 `CliOutputCallback`으로 stdout 실시간 전달

---

## 단계 9: 런타임 이벤트 처리

각 노드 완료 시:

1. `RunActionEvent` SQLite 저장 & DashMap 캐시
2. 웹훅 발화 (`src/webhook/mod.rs`):
   - `run.started`, `agent.started`, `agent.completed`, `run.completed`
3. SSE 이벤트 채널에 발행 (tokio broadcast channel)

완료 검증:
- `verify_completion()` → Validator 에이전트가 출력 검토
- 미완료 + 재시도 가능: Continuation 그래프 빌드 → 재실행 (최대 2회)
- 최종 상태: `RunStatus::Succeeded` 또는 `RunStatus::Failed`

---

## 단계 10: 프론트엔드 SSE 수신

**파일**: `web/src/hooks/use-sse.ts`, `web/src/app/api/stream/[runId]/route.ts`

202 응답 수신 직후 SSE 연결 시작:

```
GET /api/stream/{runId}?poll_ms=400&behavior=false
  ↓ (Next.js API Route: api/stream/[runId]/route.ts)
  HMAC 헤더 첨부 후 백엔드로 프록시:
GET /v1/runs/{runId}/stream?poll_ms=400
  ↓ (백엔드: src/interface/api.rs)
  매 400ms: list_run_action_events_since(run_id, last_seq, 500)
  이벤트 타입별 SSE 발행:
    data: {"type":"action_event","payload":{...}}
    data: {"type":"behavior_snapshot","payload":{...}}
    data: {"type":"run_terminal","payload":{"status":"Succeeded"}}
  → KeepAlive 매 10초
  → run_terminal 이벤트 후 스트림 종료
```

프론트엔드 `use-sse.ts`:
- `onMessage(event)` → `setEvents((prev) => [...prev, event])`
- `run_terminal` 수신 → `setTerminalStatus(status)` → SSE 연결 해제

---

## 단계 11: 메시지 렌더링

**파일**: `web/src/app/chat/page.tsx` (lines 440-522)

SSE 종료 후 최종 메시지 로드:

```typescript
// 세션 메시지 전체 조회
const messages = await apiGet(`/v1/sessions/${sessionId}/messages?limit=200`)
// 낙관적 UI의 pending 메시지와 병합
setMessages(mergeWithPending(messages, pendingUserMessage))
```

각 메시지 렌더링 규칙:

| 조건 | 렌더링 컴포넌트 |
|------|---------------|
| `role === "user"` | `<ChatBubble>` |
| `role === "agent"` + action events 있음 | `<AgentThinking>` (타임라인 뷰) |
| `role === "agent"` + action events 없음 | `<ChatBubble>` + `<ToolCallCard>` (선택적) |

`<AgentThinking>` 컴포넌트는 run의 `RunActionEvent` 목록을 시간순으로 표시:
- 각 노드의 시작/완료 시간
- 사용된 에이전트 역할
- 출력 텍스트 미리보기

---

## 전체 흐름 요약 (타임라인)

```
t=0ms   사용자 메시지 입력 → apiPost /v1/runs
t=~50ms 202 수신 → SSE 연결 시작
t=~100ms 태스크 분류 (LLM 호출)
t=~500ms DAG 그래프 빌딩 완료
t=~500ms+ Planner 노드 실행 시작
          SSE: action_event (NodeStarted: plan)
t=~2s   Planner 완료
          SSE: action_event (NodeCompleted: plan)
t=~2s+  다음 노드 실행 (extract/code/...)
          SSE: 연속 이벤트 스트림
t=Ns    마지막 노드 완료 → Validator 검증
          SSE: run_terminal (Succeeded)
t=N+50ms SSE 종료 → /v1/sessions/{id}/messages 조회
t=N+100ms 메시지 렌더링 완료
```
