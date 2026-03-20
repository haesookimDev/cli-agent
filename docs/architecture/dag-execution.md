# DAG Execution Engine

## 관련 파일

- `src/runtime/graph.rs` — ExecutionGraph, AgentNode, ExecutionPolicy
- `src/runtime/mod.rs` — AgentRuntime (동시 실행 & 이벤트 발행)
- `src/orchestrator/mod.rs` — DAG 빌딩 (lines 2187-2415)

---

## ExecutionGraph 구조

```
ExecutionGraph
  └── nodes: Vec<AgentNode>
        ├── id: String              (예: "plan", "extract", "code")
        ├── role: AgentRole         (Planner, Extractor, Coder, ...)
        ├── task: String            (이 노드가 수행할 태스크 설명)
        ├── instructions: String    (에이전트별 추가 지시)
        ├── dependencies: Vec<String>  (선행 노드 id 목록)
        └── policy: ExecutionPolicy
```

---

## ExecutionPolicy

```rust
ExecutionPolicy {
  timeout_ms: u64,          // 노드 실행 타임아웃
  on_failure: FailureAction, // Retry | Skip | Fail
  allow_parallel: bool,      // 동일 레벨 병렬 실행 허용
}
```

**병렬 실행 제한**: `DYNAMIC_SUBTASK_MAX_PARALLELISM = 4`
**재시도 상한**: `MAX_COMPLETION_CONTINUATIONS = 2`

---

## 태스크 타입별 DAG 그래프

### SimpleQuery
```
plan → summarize
```

### Analysis
```
plan → extract → analyze → summarize
```

### ConfigQuery
```
plan → summarize (시스템 상태 포함)
```

### Configuration
```
plan → config_manage → summarize
```

### ToolOperation
```
plan → tool_call → summarize
```

### CodeGeneration
```
plan → extract → context_probe(optional) → code → review(optional) → fallback(optional) → summarize
```

### Complex / Interactive
```
plan → [동적 서브태스크 그래프] → summarize
  └── Planner가 런타임에 노드 생성
```

---

## AgentRuntime 실행 흐름

```
execute_graph(graph, run_id, session_id)
  ↓
위상 정렬 (토폴로지 순서)
  ↓
레벨별 병렬 실행 루프:
  for each level:
    의존성 충족된 노드들을 tokio::spawn으로 병렬 실행
    각 노드: AgentRegistry.run_role(role, AgentInput)
      → 프롬프트 구성
      → ModelRouter.infer_stream(...)
      → AgentOutput 반환
    완료 시:
      → RunActionEvent 기록 (SQLite + in-memory)
      → 웹훅 발화
      → SSE 채널에 이벤트 발행
  ↓
모든 노드 완료 후
  → Validator 검증
  → 성공: RunStatus::Succeeded
  → 실패 + attempts < 2: Continuation 그래프 빌드 → 재실행
  → 실패 + attempts >= 2: RunStatus::Failed
```

---

## RunActionEvent 구조

```rust
RunActionEvent {
  seq: i64,
  event_type: RunActionType,  // NodeStarted | NodeCompleted | NodeFailed | RunCompleted | ...
  timestamp: DateTime<Utc>,
  actor_type: String,         // "agent" | "system"
  actor_id: String,           // 노드 id
  payload: serde_json::Value, // 노드별 메타데이터
}
```

---

## Continuation 로직

태스크 완료 검증 실패 시 Reviewer 에이전트가 미완성 이유를 분석하고, Planner가 후속 그래프를 생성해 재실행:

```
execute_graph() 완료
  → verify_completion()
  → if !complete && attempts < MAX_COMPLETION_CONTINUATIONS:
      Reviewer.analyze(outputs) → incomplete_reason
      Planner.build_continuation(incomplete_reason) → new_graph
      execute_graph(new_graph) [재귀]
```

최대 2회 반복 (`MAX_COMPLETION_CONTINUATIONS = 2`)
