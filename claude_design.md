# TODO 전체 구현 설계서 (v2)

## Context

TODO.md의 5가지 핵심 요구사항 + codex_design.md 비교 분석을 반영한 상세 구현 설계.

**현재 시스템의 한계:**
- 에이전트 실패 시 단순 retry만 존재, 워크플로우 재설계 없음
- Coder는 LLM 호출로 코드 텍스트만 생성, 실제 파일 작성/실행 없음
- 병렬 코더 세션 미지원
- 단일 DAG 워크플로우만 존재, 역할 간 피드백 루프 없음
- 단일 오케스트레이터
- orchestrator/mod.rs가 3,897줄로 과도하게 비대함
- 프롬프트 구성이 build_run_node_fn 안에 하드코딩되어 있음
- SSE 파서의 UTF-8 바이트 경계 처리 불안정

---

## 공통 아키텍처 원칙

모든 TODO 구현에 걸쳐 적용되는 불변 규칙. 각 섹션에서 반복하지 않는다.

1. **session_id 단일 확정**: `submit_run`에서 확정된 session_id만 사용한다. `execute_run`에서 재생성 절대 불가.
2. **History 강제 주입**: 후속 발화는 독립 질의로 처리하지 않는다. 직전 사용자 메시지 + 직전 성공 run 요약을 History 예산에서 우선 할당(priority: 1.0)한다.
3. **filesystem 우선 라우팅**: 로컬 파일 의도가 감지되면 `filesystem/*` MCP 도구를 우선 라우팅한다. Planner/ToolCaller 프롬프트의 SystemPolicy 계층에 이 정책을 명시한다.
4. **token_seq 재정렬**: `NodeTokenChunk`에 `token_seq: u64`를 추가하여 out-of-order 도착 시 프론트엔드에서 정렬 가능하게 한다.
5. **JSONL replay tolerant**: 세션 로그는 세션 단위 직렬 append를 유지하고, concatenated JSON 복구(중간 크래시 후 재시작 시 불완전 JSON 라인 스킵)를 보장한다.
6. **UTF-8 경계 보존**: 백엔드 SSE 파서는 바이트 버퍼 유지 + 완성된 코드포인트만 플러시한다. 프론트엔드는 `TextDecoder("utf-8", { fatal: false })`의 `stream: true`를 사용한다. 양쪽 모두 청크 경계 회귀 테스트를 추가한다.

---

## 아키텍처 리팩토링: 오케스트레이터 4서비스 분리

### 동기

현재 `orchestrator/mod.rs`가 3,897줄이며 여기에 TODO 1~4를 추가하면 5,000줄 이상이 됨.
관심사를 분리하여 유지보수성 확보.

### 분리 구조

```
src/orchestrator/
├── mod.rs              (기존, 축소) — RunCoordinator 역할
├── replan.rs           (신규) — ReplanEngine
├── pipeline.rs         (신규) — WorkflowComposer (파이프라인 실행)
├── coder_backend.rs    (신규) — CoderSessionManager
└── prompt_composer.rs  (신규) — PromptComposer (프롬프트 조립 계층)
```

| 서비스 | 역할 | 현재 위치 → 이동 |
|--------|------|-------------------|
| **RunCoordinator** | submit_run, execute_run, cancel/pause/resume, finish_run | orchestrator/mod.rs (축소 유지) |
| **ReplanEngine** | diagnose_failure, build_recovery_graph, 실패분류, 복구루프 | 신규 replan.rs |
| **WorkflowComposer** | PipelineExecutor, 페이즈 실행, 피드백 루프, PhaseHook | 신규 pipeline.rs |
| **CoderSessionManager** | CLI 코더 세션 생성/관리/정리, worktree 격리, PTY 연결 | 신규 coder_backend.rs |
| **PromptComposer** | 6계층 프롬프트 조립 | 신규 prompt_composer.rs |

### 수정 파일

- `src/orchestrator/mod.rs` — build_run_node_fn에서 코더 분기 → CoderSessionManager 위임, 실패 복구 → ReplanEngine 위임
- `src/runtime/mod.rs` — RuntimeEvent에 새 이벤트 추가
- `src/runtime/graph.rs` — AgentNode에 retry_context 필드 추가
- `src/types.rs` — 모든 새 타입 정의
- `src/config.rs` — 코더 백엔드 환경변수 추가

---

## DB 스키마 확장

기존 테이블에 필드를 추가하는 대신 전용 테이블 4개 신규 생성.

### `src/memory/store.rs` — 마이그레이션 추가

```sql
-- 재시도 이력 추적
CREATE TABLE run_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    attempt_no INTEGER NOT NULL,
    status TEXT NOT NULL,             -- "succeeded" | "failed" | "timeout"
    failure_class TEXT,               -- "tool_fail" | "context_missing" | "logic_gap" | "timeout"
    reason TEXT,
    delta_prompt_json TEXT,           -- 재시도 시 추가된 프롬프트 JSON
    created_at TEXT NOT NULL,
    UNIQUE(run_id, node_id, attempt_no)
);

-- 코더 CLI 세션 추적
CREATE TABLE coder_sessions (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    backend TEXT NOT NULL,            -- "claude_code" | "codex" | "llm"
    terminal_session_id TEXT,         -- 기존 터미널 시스템의 세션 ID (PTY 재활용)
    working_dir TEXT,
    worktree_branch TEXT,             -- git worktree 브랜치명 (병렬 시)
    status TEXT NOT NULL,             -- "running" | "completed" | "failed"
    exit_code INTEGER,
    files_changed_json TEXT,          -- JSON array of CoderFileChanged
    started_at TEXT NOT NULL,
    ended_at TEXT
);

-- 파이프라인 실행 인스턴스 (실행 단위)
CREATE TABLE pipeline_executions (
    id TEXT PRIMARY KEY,
    pipeline_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    status TEXT NOT NULL,             -- "pending" | "running" | "completed" | "failed"
    current_phase_id TEXT,
    feedback_count INTEGER DEFAULT 0,
    started_at TEXT NOT NULL,
    completed_at TEXT
);

-- 파이프라인 페이즈별 상태 (페이즈 단위, pipeline_executions와 1:N)
CREATE TABLE pipeline_phase_states (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    execution_id TEXT NOT NULL REFERENCES pipeline_executions(id),
    phase_id TEXT NOT NULL,
    role TEXT NOT NULL,               -- PipelineRole
    status TEXT NOT NULL,             -- "pending" | "running" | "completed" | "feedback_required" | "failed" | "skipped"
    run_id TEXT,                      -- 이 페이즈를 실행한 orchestrator run_id
    input_contract_json TEXT,         -- 입력 계약 (구조화된 데이터)
    output_contract_json TEXT,        -- 출력 계약 (구조화된 데이터)
    output_summary TEXT,
    feedback_json TEXT,               -- 리뷰 피드백
    attempt_count INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(execution_id, phase_id)
);

CREATE INDEX idx_run_attempts_run ON run_attempts(run_id);
CREATE INDEX idx_coder_sessions_run ON coder_sessions(run_id);
CREATE INDEX idx_pipeline_executions_pipeline ON pipeline_executions(pipeline_id);
CREATE INDEX idx_pipeline_phase_states_exec ON pipeline_phase_states(execution_id);
```

---

## TODO 1: ReplanEngine — 실패 복구 및 워크플로우 재설계

### 1.1 개요

현재 `verify_completion()`이 INCOMPLETE를 반환해도 run을 Succeeded로 마킹하고 끝남. 이를 개선하여:
- **구조화된 실패 분류** (tool_fail, context_missing, logic_gap, timeout)
- 분류별 복구 전략 분기
- 복구 그래프 동적 생성
- 실패한 노드만 부분 재실행 지원

### 1.2 수정 파일 및 구현

#### A. `src/types.rs` — 새 타입

```rust
/// 실패 분류 (구조화)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FailureClass {
    ToolFail,         // MCP 도구 호출 실패
    ContextMissing,   // 필요한 정보 부족
    LogicGap,         // 논리적 오류/불완전한 추론
    Timeout,          // 시간 초과
}

/// 실패 진단 결과
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryDiagnosis {
    pub failure_class: FailureClass,
    pub incomplete_reason: String,
    pub failed_node_ids: Vec<String>,           // 실패한 특정 노드
    pub missing_capabilities: Vec<AgentRole>,
    pub suggested_actions: Vec<String>,
    pub additional_context: String,
    pub should_retry: bool,
}

/// RunStatus 확장
pub enum RunStatus {
    // 기존: Queued, Running, Succeeded, Failed, Cancelled, Paused
    Recovering,  // 복구 그래프 실행 중
}

/// RunActionType 확장
// 기존 action_event에 추가:
// "replan_triggered" — 재계획 시작, payload에 failure_class + reason
// "recovery_graph_built" — 복구 그래프 생성됨
// "partial_rerun_started" — 부분 재실행 시작
```

#### B. `src/runtime/graph.rs` — AgentNode 확장

```rust
pub struct AgentNode {
    // 기존 필드...
    pub retry_context: Option<String>,  // 재시도 시 이전 실패 정보 + 추가 컨텍스트 주입
}
```

#### C. `src/orchestrator/replan.rs` — 신규 파일 (핵심)

```rust
pub struct ReplanEngine {
    agents: AgentRegistry,
    router: Arc<ModelRouter>,
    memory: Arc<MemoryManager>,
}

impl ReplanEngine {
    /// 실패 분류 (구조화된 4가지 유형)
    fn classify_failure(
        &self,
        results: &[NodeExecutionResult],
        incomplete_reason: &str,
    ) -> FailureClass {
        // 1. 결과에 MCP tool error가 있으면 → ToolFail
        // 2. "missing", "not found", "no context" 키워드 → ContextMissing
        // 3. timeout 에러 → Timeout
        // 4. 그 외 → LogicGap
    }

    /// 실패 진단 (Planner 에이전트 활용)
    pub async fn diagnose_failure(
        &self,
        original_task: &str,
        results: &[NodeExecutionResult],
        incomplete_reason: &str,
    ) -> anyhow::Result<RecoveryDiagnosis> {
        let failure_class = self.classify_failure(results, incomplete_reason);

        // Planner에게 구조화된 진단 요청
        // 프롬프트: "작업이 {failure_class}로 실패했습니다.
        //   원인: {incomplete_reason}
        //   실행 결과 요약: {results_summary}
        //   JSON으로 답하세요: {failed_node_ids, missing_capabilities, suggested_actions, additional_context, should_retry}"
        // → RecoveryDiagnosis 파싱
    }

    /// 복구 그래프 생성 — 실패 분류별 전략 분기
    pub fn build_recovery_graph(
        &self,
        diagnosis: &RecoveryDiagnosis,
        original_graph: &ExecutionGraph,
        original_results: &[NodeExecutionResult],
        original_task: &str,
    ) -> anyhow::Result<ExecutionGraph> {
        match diagnosis.failure_class {
            FailureClass::ToolFail => {
                // 실패한 ToolCaller 노드만 재실행 + 대체 도구 제안
            }
            FailureClass::ContextMissing => {
                // Extractor 노드 추가 → 실패 노드 재실행
                // additional_context를 retry_context에 주입
            }
            FailureClass::LogicGap => {
                // 전체 그래프 재구성 with missing_capabilities 포함
            }
            FailureClass::Timeout => {
                // 동일 노드를 더 긴 타임아웃 + 더 빠른 모델로 재실행
            }
        }
        // 모든 경우: 마지막에 Reviewer 검증 노드 추가
    }

    /// 부분 재실행 — 실패한 노드만 재실행
    pub fn build_partial_rerun_graph(
        &self,
        diagnosis: &RecoveryDiagnosis,
        original_graph: &ExecutionGraph,
        successful_results: &[NodeExecutionResult],
    ) -> anyhow::Result<ExecutionGraph> {
        // diagnosis.failed_node_ids에 해당하는 노드만 포함
        // 성공한 노드의 출력을 dependency_outputs로 주입
        // 실패 노드의 retry_context에 실패 원인 + 추가 정보 주입
    }
}
```

#### D. `src/orchestrator/mod.rs` — execute_run 수정

```
기존 흐름:
  build_graph → execute_graph → verify_completion → finish_run

변경 흐름:
  build_graph → execute_graph → verify_completion
  → IF INCOMPLETE:
    → replan_engine.diagnose_failure()
    → RunActionType::ReplanTriggered 이벤트 발행 (failure_class, reason 포함)
    → RunStatus::Recovering 전환
    → IF should_retry AND recovery_attempt < max_recovery (2):
      → IF 실패 노드가 특정됨: build_partial_rerun_graph() (부분 재실행)
      → ELSE: build_recovery_graph() (전체 복구)
      → execute_graph(recovery_graph)
      → verify_completion() (재검증)
    → ELSE: finish_run(실패 정보 포함)
  → IF COMPLETE: finish_run(성공)
```

- `max_recovery_attempts: u8 = 2` (Orchestrator 필드 추가)
- 매 시도마다 run_attempts 테이블에 기록

### 1.3 API 확장

```
GET  /v1/runs/:id/attempts    → list_run_attempts_handler  (재시도 이력)
POST /v1/runs/:id/replan      → manual_replan_handler      (수동 재계획 트리거)
```

### 1.4 프론트엔드 — Replan Card

`web/src/components/replan-card.tsx` (신규):

```
┌─────────────────────────────────────────────────┐
│ ⚠ Replan Triggered (Attempt #2)                 │
├─────────────────────────────────────────────────┤
│ Failure: context_missing                         │
│ Reason: "모델 선택 로직에 필요한 스코어링 공식   │
│          정보가 컨텍스트에 없음"                   │
│                                                   │
│ Recovery Strategy:                                │
│  + Extractor 노드 추가 (router/mod.rs 분석)       │
│  ~ Coder 노드 재실행 (추가 컨텍스트 포함)          │
│                                                   │
│ Changed Graph:                                    │
│  [extract_router] → [coder_fix] → [reviewer]     │
│                                                   │
│ [View Original Graph] [Cancel Recovery]           │
└─────────────────────────────────────────────────┘
```

- `web/src/lib/types.ts`: RunStatus에 `"recovering"` 추가
- `web/src/components/status-badge.tsx`: recovering 뱃지 (주황색 순환 아이콘)
- `web/src/components/agent-thinking.tsx`: replan_triggered 이벤트 감지 시 ReplanCard 렌더링

---

## TODO 2: CoderSessionManager — CLI 코더 백엔드

### 2.1 개요

`trait CoderBackend`로 추상화하여 Claude Code / Codex / 기존 LLM 3가지 구현체를 교체 가능하게 설계. 기존 터미널 PTY 시스템을 재활용하여 구현량 감소.

### 2.2 수정 파일 및 구현

#### A. `src/config.rs` — 환경변수

```rust
pub coder_backend: CoderBackendKind,     // env CODER_BACKEND=claude_code|codex|llm (기본: llm)
pub coder_command: String,               // env CODER_COMMAND=claude
pub coder_args: Vec<String>,             // env CODER_ARGS=--dangerously-skip-permissions
pub coder_working_dir: Option<String>,   // env CODER_WORKING_DIR
pub coder_timeout_ms: u64,              // env CODER_TIMEOUT_MS=300000 (5분)
```

#### B. `src/types.rs` — 새 타입

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoderBackendKind {
    ClaudeCode,
    Codex,
    Llm,  // 기본값
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderOutputChunk {
    pub session_id: String,
    pub stream: String,           // "stdout" | "stderr"
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderFileChanged {
    pub path: String,
    pub change_type: String,      // "created" | "modified" | "deleted"
    pub diff_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderSessionResult {
    pub output: String,
    pub exit_code: i32,
    pub files_changed: Vec<CoderFileChanged>,
    pub duration_ms: u128,
}
```

#### C. `src/orchestrator/coder_backend.rs` — trait 기반 추상화

```rust
/// 코더 백엔드 trait — 확장 시 구현체만 추가
#[async_trait]
pub trait CoderBackend: Send + Sync {
    fn kind(&self) -> CoderBackendKind;

    async fn run(
        &self,
        task: &str,
        context: &str,
        working_dir: &Path,
        on_chunk: Arc<dyn Fn(CoderOutputChunk) + Send + Sync>,
    ) -> anyhow::Result<CoderSessionResult>;
}

/// LLM 코더 — 기존 방식 (router.infer)
pub struct LlmCoderBackend {
    router: Arc<ModelRouter>,
}

/// Claude Code CLI 코더
pub struct ClaudeCodeBackend {
    command: String,   // "claude"
    args: Vec<String>, // ["--dangerously-skip-permissions"]
    timeout: Duration,
}

/// Codex CLI 코더
pub struct CodexBackend {
    command: String,   // "codex"
    args: Vec<String>, // ["--approval-mode", "full-auto"]
    timeout: Duration,
}

/// 세션 관리자 — 코더 세션 생성/추적/정리
pub struct CoderSessionManager {
    backends: HashMap<CoderBackendKind, Arc<dyn CoderBackend>>,
    active_sessions: DashMap<String, CoderSessionState>,
    terminal_bridge: Arc<TerminalManager>,  // 기존 PTY 시스템 재활용
}

struct CoderSessionState {
    id: String,
    run_id: Uuid,
    node_id: String,
    backend: CoderBackendKind,
    terminal_session_id: Option<String>,  // PTY 매핑
    working_dir: PathBuf,
    worktree_branch: Option<String>,
    status: String,
    started_at: Instant,
}

impl CoderSessionManager {
    /// 코더 세션 생성 — PTY 매핑 + DB 기록
    pub async fn spawn_session(
        &self,
        run_id: Uuid,
        node_id: &str,
        backend_kind: CoderBackendKind,
        task: &str,
        context: &str,
        working_dir: &Path,
        on_chunk: Arc<dyn Fn(CoderOutputChunk) + Send + Sync>,
    ) -> anyhow::Result<String>  // session_id

    /// 완료 대기 + DB 업데이트
    pub async fn wait_for_completion(
        &self,
        session_id: &str,
    ) -> anyhow::Result<CoderSessionResult>

    /// 파일 변경 감지 (git diff --name-status)
    pub async fn detect_file_changes(
        &self,
        working_dir: &Path,
        baseline_commit: &str,
    ) -> anyhow::Result<Vec<CoderFileChanged>>

    /// 세션 강제 종료
    pub async fn kill_session(&self, session_id: &str) -> anyhow::Result<()>

    /// 활성 세션 목록
    pub fn active_sessions_for_run(&self, run_id: Uuid) -> Vec<CoderSessionState>
}
```

**Claude Code 실행:**
```bash
claude -p "task prompt" --output-format stream-json
```
- stdout 라인별 JSON 파싱 → CoderOutputChunk 변환
- 기존 terminal_session_id에 매핑하여 WebSocket으로도 접근 가능

**Codex 실행:**
```bash
codex --approval-mode full-auto "task prompt"
```

#### D. `src/orchestrator/mod.rs` — build_run_node_fn 분기

```rust
if node.role == AgentRole::Coder {
    let backend_kind = node.metadata.get("backend")
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or(config.coder_backend.clone());

    if backend_kind != CoderBackendKind::Llm {
        // CLI 코더 경로 → CoderSessionManager에 위임
        let session_id = coder_manager.spawn_session(
            run_id, &node.id, backend_kind,
            &full_prompt, &context_str, working_dir, on_chunk
        ).await?;

        event_sink.send(RuntimeEvent::CoderSessionStarted { node_id, session_id, backend });
        let result = coder_manager.wait_for_completion(&session_id).await?;
        event_sink.send(RuntimeEvent::CoderSessionCompleted { node_id, session_id, files_changed, exit_code });

        return Ok(AgentOutput { model: backend_name, content: result.output });
    }
    // else: 기존 LLM 경로 (fallthrough)
}
// 기존 router.infer() 경로
```

#### E. `src/runtime/mod.rs` — RuntimeEvent 확장

```rust
pub enum RuntimeEvent {
    // 기존...
    CoderSessionStarted { node_id: String, session_id: String, backend: String },
    CoderOutputChunk { node_id: String, session_id: String, stream: String, content: String },
    CoderFileChanged { node_id: String, session_id: String, file: CoderFileChanged },
    CoderSessionCompleted { node_id: String, session_id: String, files_changed: Vec<CoderFileChanged>, exit_code: i32 },
}
```

#### F. API 확장

```
GET  /v1/runs/:id/coder-sessions  → list_coder_sessions_handler
```

---

## TODO 3: 병렬 코더 세션 + 실시간 UI 스트리밍

### 3.1 서버 측

#### A. 병렬 코더 노드 생성

`build_on_completed_fn` 수정 — Planner의 SubtaskPlan에서 Coder 서브태스크가 복수이면 각각 독립 노드:

```rust
for subtask in plan.subtasks.iter().filter(|s| s.agent_role == AgentRole::Coder) {
    let node = AgentNode {
        id: format!("coder_{}", subtask.id),
        role: AgentRole::Coder,
        instructions: subtask.instructions.clone(),
        dependencies: subtask.dependencies.clone(),
        policy: ExecutionPolicy {
            max_parallelism: 4,  // 코더 역할 최대 4개 병렬
            ..default_coding_policy()
        },
        depth: planner_depth + 1,
        retry_context: None,
    };
    dynamic_nodes.push(node);
}
```

#### B. 경쟁 실행 모드 (동일 태스크, 다중 백엔드)

동일한 태스크를 Claude Code + Codex에 동시 실행 → Reviewer가 우수안 선택:

```rust
/// 경쟁 모드: 동일 태스크를 여러 백엔드로 동시 실행
fn build_competitive_coder_nodes(
    task: &str,
    backends: &[CoderBackendKind],
    dependencies: &[String],
    depth: u8,
) -> Vec<AgentNode> {
    let coder_nodes: Vec<AgentNode> = backends.iter().enumerate().map(|(i, backend)| {
        AgentNode {
            id: format!("coder_{}_{}", backend.as_str(), i),
            role: AgentRole::Coder,
            instructions: task.to_string(),
            metadata: HashMap::from([("backend".into(), serde_json::to_string(backend).unwrap())]),
            dependencies: dependencies.to_vec(),
            policy: ExecutionPolicy { max_parallelism: backends.len(), ..default() },
            depth,
            ..default()
        }
    }).collect();

    // Reviewer 노드: 모든 코더 결과를 비교하여 최적안 선택
    let reviewer = AgentNode {
        id: "coder_reviewer".into(),
        role: AgentRole::Reviewer,
        instructions: "여러 코더의 결과를 비교하여 최적의 구현을 선택하세요. BEST:<node_id> 형식으로 답하세요.".into(),
        dependencies: coder_nodes.iter().map(|n| n.id.clone()).collect(),
        depth: depth + 1,
        ..default()
    };

    let mut nodes = coder_nodes;
    nodes.push(reviewer);
    nodes
}
```

#### C. Git Worktree 격리

```rust
impl CoderSessionManager {
    /// 병렬 코더용 격리 세션 (git worktree)
    pub async fn spawn_isolated_session(
        &self,
        run_id: Uuid,
        node_id: &str,
        backend_kind: CoderBackendKind,
        task: &str,
        context: &str,
        repo_root: &Path,
    ) -> anyhow::Result<String> {
        let branch = format!("coder-{}-{}", node_id, Uuid::new_v4().simple());
        // 1. git worktree add .worktrees/{session_id} -b {branch}
        // 2. worktree 경로에서 코더 실행
        // 3. CoderSessionState에 worktree_branch 기록
    }

    /// 병렬 코더 결과 병합
    pub async fn merge_session_results(
        &self,
        session_ids: &[String],
        target_branch: &str,
    ) -> anyhow::Result<MergeResult> {
        // 각 worktree의 변경사항을 순차 merge
        // 충돌 시 MergeResult에 conflict 정보 포함
        // 완료 후 worktree 정리: git worktree remove
    }
}
```

#### D. SSE 이벤트

기존 action_event에 코더 세션 이벤트가 자동 포함됨 (RuntimeEvent → RunActionEvent 변환 경로 활용).

```
event: action_event
data: {"action":"coder_session_started","payload":{"session_id":"abc","backend":"claude_code","node_id":"coder_1"}}

event: action_event
data: {"action":"coder_output_chunk","payload":{"session_id":"abc","stream":"stdout","content":"Reading src/...","node_id":"coder_1"}}

event: action_event
data: {"action":"coder_session_completed","payload":{"session_id":"abc","exit_code":0,"files_changed":[...]}}
```

추가 SSE 엔드포인트:
```
GET /v1/runs/:id/events?after_seq=N  → SSE 재접속 시 이벤트 백필
```

### 3.2 프론트엔드: 3패널 레이아웃

#### A. 채팅 화면 레이아웃 변경

기존: 좌(세션 목록) + 우(대화)
변경: 좌(Session/Memory) + 중앙(Conversation+Timeline) + 우(Run Inspector)

```
┌───────────────┬──────────────────────────┬──────────────────────┐
│ Sessions      │  Conversation            │  Run Inspector       │
│ ─────────     │  ─────────────           │  ──────────────      │
│ [세션 목록]    │  [User]: 요청 내용       │  [Tab: Overview]     │
│               │                          │  [Tab: Coder #1]     │
│ Memory        │  [Agent Thinking]        │  [Tab: Coder #2]     │
│ ─────────     │  ├ Planner ✓             │  [Tab: Context]      │
│ Session:      │  ├ Extractor ✓           │                      │
│  • item 1     │  ├ Coder #1 🔄           │  ─── Coder #1 ───   │
│  • item 2     │  └ Coder #2 🔄           │  Backend: claude     │
│               │                          │  Status: running     │
│ Global:       │  [Summarizer output]     │  ┌────────────────┐  │
│  • knowledge  │                          │  │ $ claude -p ... │  │
│               │                          │  │ > Reading...    │  │
│ [+ Add]       │  [입력 필드]              │  │ > Creating...   │  │
│               │                          │  └────────────────┘  │
│               │                          │  Files Changed:      │
│               │                          │   + src/api.rs       │
│               │                          │   ~ src/main.rs      │
└───────────────┴──────────────────────────┴──────────────────────┘
```

#### B. Run Inspector 탭 구조

```typescript
// web/src/components/run-inspector.tsx (신규)
function RunInspector({ runId, events }: Props) {
  const [activeTab, setActiveTab] = useState("overview");
  const coderSessions = extractCoderSessions(events);

  return (
    <div className="h-full flex flex-col">
      {/* 탭 헤더 */}
      <div className="flex border-b">
        <Tab id="overview" label="Overview" />
        {coderSessions.map(s => (
          <Tab key={s.sessionId} id={s.sessionId}
               label={`Coder #${s.index} (${s.backend})`}
               status={s.status} />
        ))}
        <Tab id="context" label="Context" />
      </div>

      {/* 탭 콘텐츠 */}
      {activeTab === "overview" && <OverviewPanel events={events} />}
      {activeTab === "context" && <ContextSourceView runId={runId} />}
      {coderSessions.find(s => s.sessionId === activeTab) && (
        <CoderSessionPanel session={coderSessions.find(s => s.sessionId === activeTab)!} />
      )}
    </div>
  );
}
```

#### C. Context Source View (컨텍스트 출처 뷰)

```
┌──────────────────────────────────────┐
│ Context Sources                       │
├──────────────────────────────────────┤
│ ██████████████░░░░░░ History    32%  │
│ ████████████░░░░░░░░ Retrieval  25%  │
│ ███████░░░░░░░░░░░░░ Instruct.  15% │
│ █████░░░░░░░░░░░░░░░ Tool       10% │
│ ████░░░░░░░░░░░░░░░░ System      8% │
│ ████░░░░░░░░░░░░░░░░ Reserve    10% │
├──────────────────────────────────────┤
│ History Sources:                      │
│  • 직전 사용자 발화 (45%)             │
│  • 최근 run 요약 (30%)               │
│  • 이전 대화 (25%)                    │
│ Retrieval Sources:                    │
│  • Session Memory: 3 items (60%)      │
│  • Global Knowledge: 2 items (40%)    │
└──────────────────────────────────────┘
```

#### D. 이벤트 타임라인 2레벨 토글

```typescript
// web/src/components/agent-thinking.tsx 수정
function AgentThinking({ events }: Props) {
  const [viewLevel, setViewLevel] = useState<"node" | "workflow">("node");

  return (
    <div>
      <div className="flex gap-2 mb-2">
        <button onClick={() => setViewLevel("node")}
                className={viewLevel === "node" ? "font-bold" : ""}>
          Node View
        </button>
        <button onClick={() => setViewLevel("workflow")}
                className={viewLevel === "workflow" ? "font-bold" : ""}>
          Workflow View
        </button>
      </div>

      {viewLevel === "node" && <NodeTimeline events={events} />}
      {viewLevel === "workflow" && <WorkflowTimeline events={events} />}
    </div>
  );
}
```

#### E. 인앱 토스트 알림

```typescript
// web/src/components/toast-notifications.tsx (신규)
// 2채널: 인앱 토스트 + 외부 webhook digest
function useToastNotifications(runId: string, events: RunActionEvent[]) {
  useEffect(() => {
    const latest = events[events.length - 1];
    if (!latest) return;

    switch (latest.action) {
      case "node_completed":
        toast.success(`${latest.payload.role} completed (${latest.payload.duration_ms}ms)`);
        break;
      case "node_failed":
        toast.error(`${latest.payload.role} failed: ${latest.payload.error}`);
        break;
      case "replan_triggered":
        toast.warning(`Replan triggered: ${latest.payload.failure_class}`);
        break;
      case "coder_session_completed":
        const fc = latest.payload.files_changed?.length ?? 0;
        toast.info(`Coder session done: ${fc} files changed`);
        break;
    }
  }, [events.length]);
}
```

---

## TODO 4: WorkflowComposer — 역할기반 파이프라인

### 4.1 개요

단일 DAG가 아닌, **여러 페이즈로 구성된 파이프라인**. 페이즈 간 **Acceptance Gate** + **피드백 루프** + **handoff_contract** 기반 데이터 전달.

### 4.2 수정 파일 및 구현

#### A. `src/types.rs` — 파이프라인 타입

```rust
/// 파이프라인 워크플로우 정의
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineWorkflow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub phases: Vec<PipelinePhase>,
    pub gates: Vec<AcceptanceGate>,        // 품질 관문
    pub max_feedback_loops: u8,            // 기본: 3
    pub notify_on_phase: bool,
    pub created_at: DateTime<Utc>,
}

/// Acceptance Gate — 페이즈 간 품질 관문
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceGate {
    pub id: String,
    pub name: String,                       // "Plan Gate", "Code Gate", etc.
    pub after_phase: String,                // 이 페이즈 완료 후 게이트 체크
    pub criteria: String,                   // 통과 기준 프롬프트
    pub on_fail: GateFailAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GateFailAction {
    FeedbackTo(String),   // 특정 페이즈로 피드백
    Abort,                // 파이프라인 중단
    Skip,                 // 게이트 무시하고 진행
}

/// 파이프라인 페이즈
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePhase {
    pub id: String,
    pub name: String,
    pub role: PipelineRole,
    pub task_type: TaskType,
    pub prompt_template: String,
    pub depends_on: Vec<String>,
    pub feedback_target: Option<String>,
    pub on_complete: Vec<PhaseHook>,
    pub input_contract: Option<ContractSchema>,   // 입력 데이터 계약
    pub output_contract: Option<ContractSchema>,  // 출력 데이터 계약
}

/// 워크플로우 간 데이터 계약 (구조화)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractSchema {
    pub required_fields: Vec<String>,     // 필수 필드명
    pub format: String,                   // "json" | "markdown" | "free_text"
    pub validation_prompt: Option<String>, // LLM 기반 계약 검증 프롬프트
}

/// 파이프라인 역할
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PipelineRole {
    ProjectManager,
    Developer,
    Reviewer,
    QATester,
    DevOps,
    Notifier,
}

/// 페이즈 완료 훅
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PhaseHook {
    RunCommand(String),
    WebhookNotify(String),
    SlackNotify { channel: String },
    DiscordNotify { channel_id: String },
    GenerateChangelog { path: String },
    GitCommitPush { message_template: String },
}

/// 파이프라인 실행 상태
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineExecution {
    pub id: Uuid,
    pub pipeline_id: String,
    pub session_id: Uuid,
    pub status: PipelineStatus,
    pub current_phase: String,
    pub phase_states: HashMap<String, PhaseState>,
    pub gate_results: HashMap<String, GateResult>,
    pub feedback_count: u8,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseState {
    pub status: PhaseStatus,
    pub run_id: Option<Uuid>,
    pub output_summary: Option<String>,
    pub output_contract_data: Option<serde_json::Value>,  // 구조화된 출력
    pub feedback: Option<String>,
    pub attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PhaseStatus {
    Pending, Running, Completed, FeedbackRequired, Failed, Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResult {
    pub passed: bool,
    pub reason: String,
    pub checked_at: DateTime<Utc>,
}
```

#### B. `src/orchestrator/pipeline.rs` — WorkflowComposer

```rust
pub struct WorkflowComposer {
    orchestrator: Arc<Orchestrator>,
    replan_engine: Arc<ReplanEngine>,      // 실패 시 ReplanEngine 활용
    coder_manager: Arc<CoderSessionManager>,
    memory: Arc<MemoryManager>,
    webhook: Arc<WebhookDispatcher>,
    gateway: Option<Arc<GatewayManager>>,
}

impl WorkflowComposer {
    /// 파이프라인 실행 메인 루프
    pub async fn execute(
        &self,
        pipeline: &PipelineWorkflow,
        initial_task: &str,
        session_id: Uuid,
        event_sink: Option<EventSink>,
    ) -> anyhow::Result<PipelineExecution> {
        let mut execution = PipelineExecution::new(pipeline, session_id);
        let ordered_phases = topological_sort(&pipeline.phases);

        for phase in ordered_phases {
            execution.current_phase = phase.id.clone();
            let mut attempts = 0;

            loop {
                attempts += 1;
                if attempts > pipeline.max_feedback_loops + 1 {
                    execution.phase_states.get_mut(&phase.id).unwrap().status = PhaseStatus::Failed;
                    break;
                }

                // 1. 입력 계약 검증
                if let Some(ref contract) = phase.input_contract {
                    self.validate_contract(contract, &phase.depends_on, &execution).await?;
                }

                // 2. 프롬프트 조립 (이전 페이즈 결과 + 피드백 + 계약 데이터)
                let prompt = self.build_phase_prompt(&phase, initial_task, &execution);

                // 3. 오케스트레이터로 실행
                let run_result = self.orchestrator.submit_run(RunRequest {
                    task: prompt,
                    profile: phase.role.to_task_profile(),
                    session_id: Some(session_id),
                    ..Default::default()
                }).await?;

                // 4. 완료 대기
                let run = self.wait_for_run(run_result.run_id).await?;
                let state = execution.phase_states.get_mut(&phase.id).unwrap();
                state.run_id = Some(run_result.run_id);
                state.output_summary = Some(run.final_output());
                state.attempts = attempts;

                // 5. 출력 계약 검증
                if let Some(ref contract) = phase.output_contract {
                    let valid = self.validate_output_contract(
                        contract, &run.final_output()
                    ).await?;
                    if !valid {
                        state.feedback = Some("출력이 계약을 만족하지 않습니다. 재작업 필요.".into());
                        continue; // 루프 재시도
                    }
                    state.output_contract_data = self.extract_contract_data(
                        contract, &run.final_output()
                    ).await.ok();
                }

                // 6. Acceptance Gate 체크
                if let Some(gate) = pipeline.gates.iter().find(|g| g.after_phase == phase.id) {
                    let gate_result = self.check_gate(gate, &execution).await;
                    execution.gate_results.insert(gate.id.clone(), gate_result.clone());
                    if !gate_result.passed {
                        match &gate.on_fail {
                            GateFailAction::FeedbackTo(target) => {
                                if let Some(ts) = execution.phase_states.get_mut(target) {
                                    ts.feedback = Some(gate_result.reason.clone());
                                    ts.status = PhaseStatus::Pending;
                                }
                                execution.feedback_count += 1;
                                continue;
                            }
                            GateFailAction::Abort => {
                                state.status = PhaseStatus::Failed;
                                execution.status = PipelineStatus::Failed;
                                return Ok(execution);
                            }
                            GateFailAction::Skip => { /* 통과 */ }
                        }
                    }
                }

                // 7. 피드백 루프 판단
                if let Some(ref target) = phase.feedback_target {
                    match self.check_phase_review(&run).await {
                        PhaseReview::Approved => { state.status = PhaseStatus::Completed; break; }
                        PhaseReview::Feedback(fb) => {
                            state.status = PhaseStatus::FeedbackRequired;
                            state.feedback = Some(fb.clone());
                            if let Some(ts) = execution.phase_states.get_mut(target) {
                                ts.feedback = Some(fb);
                                ts.status = PhaseStatus::Pending;
                            }
                            execution.feedback_count += 1;
                            continue;
                        }
                    }
                } else {
                    state.status = PhaseStatus::Completed;
                    break;
                }
            }

            // 8. 페이즈 훅 실행 + DB 기록
            self.fire_phase_hooks(&phase.on_complete, &execution).await;
            self.save_phase_state(&phase, &execution).await?;  // pipeline_phase_states 테이블

            // 9. 알림
            if pipeline.notify_on_phase {
                self.notify_phase_complete(&phase, &execution).await;
            }
        }

        execution.status = PipelineStatus::Completed;
        Ok(execution)
    }

    /// handoff_contract 검증
    async fn validate_contract(
        &self,
        contract: &ContractSchema,
        source_phases: &[String],
        execution: &PipelineExecution,
    ) -> anyhow::Result<()> {
        for phase_id in source_phases {
            if let Some(state) = execution.phase_states.get(phase_id) {
                if let Some(ref data) = state.output_contract_data {
                    for field in &contract.required_fields {
                        if data.get(field).is_none() {
                            anyhow::bail!("Contract violation: missing field '{}' from phase '{}'", field, phase_id);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Acceptance Gate 체크 (Reviewer 에이전트 활용)
    async fn check_gate(
        &self,
        gate: &AcceptanceGate,
        execution: &PipelineExecution,
    ) -> GateResult {
        // Reviewer에게 gate.criteria + 현재 실행 상태를 전달
        // PASSED 또는 FAILED:<reason> 응답 파싱
    }
}
```

#### C. 기본 제공 파이프라인 템플릿 3종

```rust
impl PipelineWorkflow {
    /// 유지보수 파이프라인
    pub fn maintenance_pipeline() -> Self { /* 분석 → 수정 → 테스트 → 배포 */ }

    /// 기능 개발 파이프라인
    pub fn feature_delivery_pipeline() -> Self { /* 기획 → 설계리뷰 → 개발 → 코드리뷰 → QA → 배포 */ }

    /// 자기 개선 파이프라인
    pub fn self_improvement_pipeline() -> Self {
        PipelineWorkflow {
            id: "self-improvement".into(),
            name: "Self-Improvement Pipeline".into(),
            gates: vec![
                AcceptanceGate {
                    id: "plan_gate".into(),
                    name: "Plan Gate".into(),
                    after_phase: "plan_review".into(),
                    criteria: "기획이 구체적이고 실행 가능한가?".into(),
                    on_fail: GateFailAction::FeedbackTo("planning".into()),
                },
                AcceptanceGate {
                    id: "code_gate".into(),
                    name: "Code Gate".into(),
                    after_phase: "code_review".into(),
                    criteria: "코드 품질, 보안, 성능이 기준을 충족하는가?".into(),
                    on_fail: GateFailAction::FeedbackTo("development".into()),
                },
                AcceptanceGate {
                    id: "quality_gate".into(),
                    name: "Quality Gate".into(),
                    after_phase: "testing".into(),
                    criteria: "모든 테스트가 통과하고 빌드가 성공하는가?".into(),
                    on_fail: GateFailAction::FeedbackTo("development".into()),
                },
                AcceptanceGate {
                    id: "release_gate".into(),
                    name: "Release Gate".into(),
                    after_phase: "deploy".into(),
                    criteria: "커밋/푸시/빌드가 정상 완료되었는가?".into(),
                    on_fail: GateFailAction::Abort,
                },
            ],
            phases: vec![
                // planning → plan_review → development → code_review
                // → testing → deploy → notify
                // (각 페이즈에 input/output contract 포함)
            ],
            max_feedback_loops: 3,
            notify_on_phase: true,
            created_at: Utc::now(),
        }
    }
}
```

#### D. API 엔드포인트

```
POST   /v1/pipelines                     → create_pipeline_handler
GET    /v1/pipelines                     → list_pipelines_handler
GET    /v1/pipelines/:id                 → get_pipeline_handler
DELETE /v1/pipelines/:id                 → delete_pipeline_handler
POST   /v1/pipelines/:id/execute         → execute_pipeline_handler
GET    /v1/pipelines/:id/executions      → list_pipeline_executions_handler
GET    /v1/pipeline-executions/:id       → get_pipeline_execution_handler
GET    /v1/pipeline-executions/:id/stream → stream_pipeline_handler (SSE)
```

#### E. 프론트엔드 — 파이프라인 보드

`web/src/app/pipelines/page.tsx` (신규) — 칸반 + 선형 진행 뷰 토글:

```
┌──────────────────────────────────────────────────────────────┐
│ Pipeline: Self-Improvement                    [Board|Linear] │
├──────────────────────────────────────────────────────────────┤
│ Board View (칸반):                                            │
│ ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐         │
│ │ Planning │ │ Develop  │ │ Review   │ │ Deploy   │         │
│ │ ──────── │ │ ──────── │ │ ──────── │ │ ──────── │         │
│ │ ✅ Task A│ │ 🔄 Task A│ │ ⏳       │ │ ⏳       │         │
│ │          │ │ ✅ Task B│ │ 🔄 Task B│ │          │         │
│ └──────────┘ └──────────┘ └──────────┘ └──────────┘         │
│                                                              │
│ Gate Status:                                                 │
│ ✅ Plan Gate (PASSED) → ✅ Code Gate (PASSED) →              │
│ 🔄 Quality Gate (checking...) → ⏳ Release Gate              │
├──────────────────────────────────────────────────────────────┤
│ Linear View (진행 바):                                        │
│ ✅기획 → ✅리뷰 → 🔄개발 → ⏳코드리뷰 → ⏳QA → ⏳배포 → ⏳알림│
│ Progress: ████████░░░░░░░ 3/7 phases                         │
│ Feedback loops: 1 (기획 → 리뷰 → 기획 → 리뷰✅)              │
└──────────────────────────────────────────────────────────────┘
```

---

## TODO 5: 다중 오케스트레이터 (향후 확장)

### 5.1 아키텍처

```
┌───────────────────────────────────┐
│        MetaOrchestrator            │
│  (태스크 분할 + 결과 통합 + 조율)   │
├──────────┬──────────┬─────────────┤
│ Orch #1  │ Orch #2  │  Orch #3    │
│ Backend  │ Frontend │  Infra      │
└──────────┴──────────┴─────────────┘
```

### 5.2 타입 정의

```rust
pub struct MetaTask {
    pub id: Uuid,
    pub global_run_id: Uuid,           // 전역 추적 ID
    pub original_task: String,
    pub partitions: Vec<TaskPartition>,
    pub merge_strategy: MergeStrategy,
}

pub struct TaskPartition {
    pub id: String,
    pub orchestrator_id: String,
    pub local_run_id: Option<Uuid>,    // 로컬 오케스트레이터의 run_id
    pub sub_task: String,
    pub scope: String,
    pub working_dir: PathBuf,
    pub dependencies: Vec<String>,
}

pub enum MergeStrategy {
    Sequential,
    ParallelMerge,
    ConflictResolve,
}
```

### 5.3 run_id 체계

`global_run_id + local_run_id` 2단계 추적:
- 프론트엔드에서 global_run_id로 전체 진행 조회
- 각 오케스트레이터 내부에서는 local_run_id로 독립 실행

### 5.4 구현 우선순위

TODO 1~4 안정화 이후 구현. 현재는 타입/인터페이스 설계만.

---

## 프롬프트/세션 관리 개선 (횡단 관심사)

### A. Prompt Composer 6계층 분리

`src/orchestrator/prompt_composer.rs` (신규):

```rust
pub struct PromptComposer;

impl PromptComposer {
    /// 6계층으로 프롬프트 조립
    pub fn compose(
        &self,
        layers: PromptLayers,
    ) -> String {
        let mut prompt = String::new();

        // Layer 1: SystemPolicy — 전역 시스템 규칙
        prompt += &format!("[SYSTEM_POLICY]\n{}\n\n", layers.system_policy);

        // Layer 2: TaskIntent — 현재 작업 의도
        prompt += &format!("[TASK_INTENT]\n{}\n\n", layers.task_intent);

        // Layer 3: SessionAnchor — 세션 연속성 (직전 발화 + 직전 run 요약)
        prompt += &format!("[SESSION_ANCHOR]\n{}\n\n", layers.session_anchor);

        // Layer 4: MemoryRetrieval — 세션 메모리 + 글로벌 지식
        prompt += &format!("[MEMORY]\n{}\n\n", layers.memory_retrieval);

        // Layer 5: FailureDelta — 재계획 시 실패 정보 (없으면 생략)
        if let Some(ref delta) = layers.failure_delta {
            prompt += &format!("[FAILURE_DELTA]\n{}\n\n", delta);
        }

        // Layer 6: OutputSchema — 기대 출력 형식
        prompt += &format!("[OUTPUT_SCHEMA]\n{}\n\n", layers.output_schema);

        prompt
    }
}

pub struct PromptLayers {
    pub system_policy: String,       // 역할 프롬프트 + 전역 규칙
    pub task_intent: String,         // 사용자 태스크 + instructions
    pub session_anchor: String,      // 직전 사용자 메시지 + 직전 성공 run 요약
    pub memory_retrieval: String,    // session memory hits + global knowledge hits
    pub failure_delta: Option<String>, // 재계획 시 실패 원인 + 추가 컨텍스트
    pub output_schema: String,       // 기대 출력 형식 (JSON, COMPLETE/INCOMPLETE, etc.)
}
```

`src/orchestrator/mod.rs`의 `build_run_node_fn` 내 프롬프트 조립 코드를 PromptComposer.compose() 호출로 교체.

### B. 세션 안정성

#### session_id 재생성 금지
```rust
// orchestrator/mod.rs — execute_run 내
// 기존: session_id가 None이면 새로 생성
// 변경: submit_run에서 확정된 session_id만 사용, execute_run에서 재생성 절대 불가
assert!(run_request.session_id.is_some(), "session_id must be set by submit_run");
```

#### 후속 발화 앵커링 강화
```rust
// build_run_node_fn 내 History 구성:
// 항상 주입:
// 1. 직전 사용자 메시지 (가장 최근 UserMessage)
// 2. 직전 성공 run 요약 (가장 최근 Succeeded run의 final_output 축약)
// 이 두 항목은 History 예산에서 우선 할당 (priority: 1.0)
```

#### token_seq 재정렬 복원

```rust
// RuntimeEvent::NodeTokenChunk 확장
NodeTokenChunk {
    node_id: String,
    role: AgentRole,
    token: String,
    token_seq: u64,  // 신규: 노드 내 토큰 순서 번호 (재정렬용)
},
```

프론트엔드에서 out-of-order 도착 시 token_seq 기준 정렬.

### C. UTF-8 안정성

#### 백엔드 SSE 파서
```rust
// src/interface/api.rs 또는 관련 SSE 스트리밍 코드:
// 기존: String::from_utf8_lossy(&bytes) — 멀티바이트 경계에서 깨짐
// 변경: 바이트 버퍼 유지 + UTF-8 완성된 코드포인트만 플러시
fn flush_utf8_safe(buffer: &mut Vec<u8>) -> String {
    match std::str::from_utf8(buffer) {
        Ok(s) => { let out = s.to_string(); buffer.clear(); out }
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            let out = std::str::from_utf8(&buffer[..valid_up_to]).unwrap().to_string();
            *buffer = buffer[valid_up_to..].to_vec();  // 미완성 바이트 보존
            out
        }
    }
}
```

#### 프론트엔드 SSE 파서
```typescript
// web/src/hooks/use-sse.ts:
// TextDecoder with stream: true 옵션 사용
const decoder = new TextDecoder("utf-8", { fatal: false });
// chunk 경계에서 한글 깨짐 방지:
// decoder.decode(chunk, { stream: true }) — 미완성 바이트를 다음 chunk로 이월
```

---

## 운영 안전장치

### A. Feature Flags (점진적 릴리즈)

`src/config.rs`에 기능 플래그 추가:

```rust
pub feature_recovery_loop: bool,   // env FEATURE_RECOVERY_LOOP=false (기본 비활성)
pub feature_cli_coder: bool,       // env FEATURE_CLI_CODER=false
pub feature_pipeline: bool,        // env FEATURE_PIPELINE=false
pub agent_auto_push: bool,         // env AGENT_AUTO_PUSH=false (기본 비활성)
```

- 각 기능은 플래그가 `true`일 때만 활성화
- `agent_auto_push=false`일 때 GitCommitPush 훅은 커밋만 수행하고 push는 건너뜀
- Destructive 작업(파일 삭제, force push 등)은 별도 승인 플래그 또는 명시 승인 필요

### B. 리소스 제한

```rust
pub coder_max_parallel: usize,         // env CODER_MAX_PARALLEL=4
pub coder_stdout_buffer_limit: usize,  // env CODER_STDOUT_BUFFER_KB=1024 (1MB)
pub coder_timeout_ms: u64,             // env CODER_TIMEOUT_MS=300000 (5분)
pub recovery_total_timeout_ms: u64,    // env RECOVERY_TOTAL_TIMEOUT_MS=600000 (10분)
pub pipeline_max_feedback_loops: u8,   // env PIPELINE_MAX_FEEDBACK_LOOPS=3
```

- 코더 세션: 동시 실행 수 제한, stdout/stderr 버퍼 상한, 개별 타임아웃
- 복구 루프: 총 시간 상한 (10분) + 동일 failure_class 2회 연속 시 즉시 실패 종료
- 파이프라인: 피드백 루프 횟수 상한

### C. 상태 전이 안전 제약 (ReplanEngine)

```
복구 중단 조건:
1. recovery_attempt >= max_recovery_attempts (2)
2. 동일 failure_class가 2회 연속 → 즉시 Failed 종료
3. 복구 실행 누적 시간 > recovery_total_timeout_ms → 즉시 Failed 종료
4. should_retry == false → 즉시 Failed 종료
```

---

## 호환성 전략

1. **RunStatus::Recovering fallback**: 구버전 클라이언트가 `Recovering`을 모르면 `Running`으로 표시
   ```typescript
   // web/src/lib/types.ts
   function normalizeRunStatus(status: string): RunStatus {
     if (status === "recovering") return "running"; // graceful fallback
     return status as RunStatus;
   }
   ```

2. **이벤트 파서 방어 코딩**: 미지의 action_event는 무시 (에러 아님)
   ```typescript
   // web/src/hooks/use-sse.ts
   default:
     console.debug(`Unknown action event: ${event.action}, skipping`);
     break;
   ```

3. **DB 마이그레이션**: additive-first — 신규 테이블/컬럼만 추가, 기존 필드 삭제 없음
4. **API 버전**: 기존 `/v1/` 엔드포인트 동작 보장, 신규 엔드포인트만 추가

---

## 관측성 / 메트릭

### 필수 지표

| 지표 | 설명 | 수집 위치 |
|------|------|-----------|
| `run_success_rate` | 전체 run 성공률 | finish_run |
| `replan_trigger_rate` | 복구 트리거 비율 | ReplanEngine.diagnose_failure |
| `recovery_success_rate` | 복구 성공률 (복구 후 COMPLETE 비율) | ReplanEngine.execute_recovery_attempt |
| `coder_session_failure_rate` | CLI 코더 세션 실패율 | CoderSessionManager.wait_for_completion |
| `pipeline_phase_retry_count` | 파이프라인 페이즈 재시도 횟수 | WorkflowComposer.execute |
| `mean_time_to_complete` | 평균 run 완료 시간 | finish_run |
| `gate_pass_rate` | Acceptance Gate 통과율 | WorkflowComposer.check_gate |

### 로그 상관 키

모든 로그/이벤트에 다음 키를 일관 포함:
- `session_id`
- `run_id`
- `attempt_no` (복구 시도 번호)
- `pipeline_execution_id` (파이프라인 실행 시)
- `coder_session_id` (코더 세션 실행 시)

---

## 구현 순서 (권장)

> **변경 근거**: CoderBackend(trait + 3구현체)가 없으면 병렬 코더(Phase 3)와 파이프라인의 Developer 페이즈(Phase 4)가 모두 불가능하다. 따라서 CoderSessionManager를 Phase 1으로 앞당기고, ReplanEngine을 Phase 2로 이동한다.

| Phase | 항목 | 이유 | 주요 변경 파일 |
|-------|------|------|---------------|
| **1** | CoderSessionManager + DB 스키마 + Prompt Composer | 코더 백엔드가 병렬 코더/파이프라인의 전제 조건. DB 스키마와 프롬프트 체계도 함께 구축 | coder_backend.rs 신규, prompt_composer.rs 신규, types.rs, config.rs, store.rs, orchestrator/mod.rs, runtime/mod.rs |
| **2** | ReplanEngine (실패 복구 + 워크플로우 재설계) | Phase 1의 코더 세션 실패 복구에도 활용. 품질 회귀 방지 핵심 | replan.rs 신규, types.rs, runtime/graph.rs, orchestrator/mod.rs |
| **3** | 병렬 코더 + 3패널 UI + 컨텍스트뷰 | Phase 1 코더 + Phase 2 복구 위에 구축 | orchestrator/mod.rs, api.rs, agent-thinking.tsx, run-inspector.tsx 신규, toast-notifications.tsx 신규, types.ts |
| **4** | WorkflowComposer (파이프라인 + Gate + Contract) | 모든 기반 기능(코더, 복구, 병렬) 활용 | pipeline.rs 신규, types.rs, api.rs, pipelines/page.tsx 신규 |
| **5** | Multi-Orchestrator | 최종 확장 | meta.rs 신규, types.rs, api.rs |
| **횡단** | UTF-8 안정성 + token_seq + 공통 원칙 적용 | Phase 1과 함께 진행 (상단 "공통 아키텍처 원칙" 참조) | api.rs, use-sse.ts, orchestrator/mod.rs |

## 검증 방법

각 Phase 완료 후:
1. `cargo build` — 에러/경고 없음
2. `cargo test` — 14+ 테스트 통과
3. `cd web && npm run build` — 15+ 라우트 컴파일
4. Phase별 수동 테스트:
   - Phase 1: Chat에서 코드 생성 → CLI 코더 실행 확인 + coder_sessions 테이블 기록
   - Phase 2: 의도적 불완전 응답 유도 → ReplanEngine 복구 루프 + Replan Card UI 확인
   - Phase 3: 복잡한 코딩 요청 → 병렬 세션 Run Inspector 탭 + 컨텍스트 출처 뷰
   - Phase 4: self_improvement_pipeline 실행 → 4개 Gate 통과 + 피드백 루프 + 알림
