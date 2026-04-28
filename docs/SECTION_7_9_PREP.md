# Section 7~9 사전 작업 정리

## Context

`docs/PROJECT_ANALYSIS_TODO.md` Phase 1~7 (TODO 1-1 ~ 6-7) 은 모두 완료되었다.
남은 항목은 Section 7~9 — 신규 기능 14건 — 으로, 기존 코드 수정/리팩토링이
아니라 **새로운 추상 계층을 도입하는 설계 작업**이다. 이 문서는 구현 시작
전에 정리해야 할 결정·의존성·범위를 한 곳에 모은다.

본문은 의도적으로 **"실행 계획"이 아니라 "사전 정리"** 다. 실제 코드 변경은
사용자와 범위를 합의한 뒤 phase 단위로 분리해 진행한다.

---

## 1. 남은 작업 한눈에 보기

| Section | TODO | 한 줄 요약 | 추정 규모 |
|---------|------|----------|---------|
| 7. Workflow Composition | 7-1 RequirementAnalyzer | classify_task 위에 1차 분석 레이어 | medium |
| | 7-2 WorkflowComposer | 고정 그래프 → 컴포저블 빌딩 블록 | large |
| | 7-3 SubtaskPlan 검증 | DAG 순환·역할 존재·도구 가용성 검증 | small |
| | 7-4 워크플로우 시각 편집기 | react-flow 기반 DAG GUI | large (frontend) |
| 8. Tracing & Visibility | 8-1 실시간 DAG 스트리밍 | SSE 이벤트 → DAG 노드 상태 | medium (frontend) |
| | 8-2 채팅 진행 카드 | phase indicator + progress + recovery alert | small (frontend) |
| | 8-3 서브태스크 트리 뷰 | 동적 서브태스크 계층 표시 | medium (frontend) |
| | 8-4 토큰/비용 추적 | InferenceResult → usage, 모든 backend 확장 | large (backend) |
| | 8-5 재계획 루프 트레이싱 | 새 RunActionType 2종 + UI 표시 | small |
| 9. Harness Engineering | 9-1 AgentHarness 추상화 | Session 라이프사이클 관리 | large |
| | 9-2 SubAgentManager | 부모-자식 세션 + 결과 집약 | large |
| | 9-3 메시지 버스 | 에이전트 간 직접 통신 | medium |
| | 9-4 하네스 메트릭 | active sessions, retry rate, etc. | medium |
| | 9-5 동적 역할 등록 | runtime AgentRegistry hot-reload | small |
| | 9-6 orchestrator를 하네스로 | run_role 직접 호출 → harness 위임 | large (breaking) |

**총 14건**. 정직하게 보면 7-2, 7-4, 8-4, 9-1, 9-2, 9-6 중 어느 하나도
이전 Phase 평균 commit 7~8개 규모를 한참 넘는다. 한 번에 다 못 한다.

---

## 2. 현재 코드 표면 (조사 결과)

사전에 확인한 사실 — 사용자 합의 시 이 가정 위에서 시작.

### 2-1. AgentInput / AgentOutput (`src/agents/mod.rs:17-30`)
```rust
pub struct AgentInput {
    pub task: String,
    pub instructions: String,
    pub context: OptimizedContext,
    pub dependency_outputs: Vec<String>,
    pub brief: StructuredBrief,
    pub working_dir: Option<PathBuf>,
}
pub struct AgentOutput { pub model: String, pub content: String }
```
- **session_id 없음** → Section 9 Harness 도입 시 AgentOutput 또는 별도 트랜
  스포트에 session_id를 실어야 함.
- **token usage 없음** → Section 8-4 (TODO 8-4) 의 1차 변경점.

### 2-2. InferenceResult (`src/router/mod.rs:135-140`)
```rust
pub struct InferenceResult {
    pub provider: ProviderKind,
    pub model_id: String,
    pub output: String,
    pub used_fallback: bool,
}
```
- 토큰 사용량 필드 없음.
- 4개의 infer 진입점 (`infer`, `infer_stream`, `infer_stream_in_dir`,
  `infer_stream_in_dir_with_cli_output`) 가 모두 같은 타입을 반환.
- 추가 필드는 모든 provider 응답 파싱(vllm/openai/anthropic/gemini/cli)에
  영향. **순방향 호환을 위해 `Option<TokenUsage>` 로 추가.**

### 2-3. AgentRegistry::run_role / run_role_stream (`src/agents/mod.rs:89-145`)
- 단순한 함수형 호출. 세션·상태 없음.
- Section 9 Harness 가 이 호출을 **삼키는** 형태가 자연스럽다 (run_role
  유지 + harness 가 내부에서 호출).

### 2-4. RuntimeEvent (`src/runtime/mod.rs:90-180`)
- 현재 17 variant. NodeCompleted 에 `duration_ms` 만 있고 token 정보 없음.
- Section 8-4 (token tracking) → NodeCompleted 확장 OR 새 variant
  `NodeTokenUsage { node_id, input, output }` 추가.

### 2-5. Frontend (`web/src/components/trace/`)
- 현재 `dag-graph.tsx`, `event-timeline.tsx` 만 존재.
- Section 8-3 의 `subtask-tree.tsx` 는 신규 컴포넌트.
- DAG는 폴링 기반 — Section 8-1 에서 SSE 즉시 반영으로 전환.

### 2-6. Skills (`skills/*.yaml`) — 9개 (이전 문서엔 7개로 적힘, 실제 9)
- WorkflowComposer (TODO 7-2) 는 이 9개를 빌딩 블록 후보로 본다.

---

## 3. 결정해야 할 것 (사용자 합의 필요)

작업 시작 전에 답이 정해져야 의미 있는 코드가 나오는 항목들.

### Q1. **범위/우선순위**
3개 Section 을 한 번에 끌고 가지 말고, 하나만 골라 끝까지 끌어가는 게 좋다.
세 가지 후보:

- **(A) Section 8 우선 (Tracing)** — 8-4 토큰 추적이 7-1 의 complexity
  estimate 와 9-4 의 metrics 모두에 선결 조건. 가장 작은 단위로 가시적
  성과를 낸다. 추천.
- **(B) Section 9 우선 (Harness)** — 가장 깊은 변경이지만 Section 7
  WorkflowComposer 가 자연스레 위에 올라간다. 단, 9-6 은 breaking 변경.
- **(C) Section 7 우선 (Workflow)** — 사용자에게 새 능력(자연어 → 워크
  플로우 자동 생성) 을 가장 빨리 노출. 단, 7-1/7-2 는 LLM 호출 비용이
  반복 발생하므로 8-4 (토큰 추적) 없이 시작하면 비용 추정 불가.

### Q2. **Harness 도입 방식 — 점진 vs 빅뱅**
Section 9 가 가장 큰 결정 지점. 두 옵션:

- **점진**: 9-1 AgentHarness 만 먼저 도입하고 `run_role()` 옆에 두는
  사이드 카 형태. 9-6 (orchestrator 전환) 은 별도 phase. 회귀 위험 낮음.
- **빅뱅**: 9-1~9-6 을 한 phase 로 묶고 `run_role()` 직접 호출 경로를
  Harness 내부로 캡슐화. 정합성은 높지만 commit 단위가 커져 reviewer 부담.

### Q3. **WorkflowComposer 의 LLM 의존도**
TODO 7-2 의 `generate_from_description` 은 매번 LLM 호출. 캐싱 정책,
실패 시 fallback (정적 스킬 매칭으로) 을 명시해야 한다.

### Q4. **시각 편집기 (TODO 7-4) 도입 여부**
react-flow 등 신규 의존성. 기능성은 높지만 frontend bundle 가 커진다.
필수가 아니라면 Section 7 의 백엔드 부분만 1차로 끝내고 7-4 는 보류 권장.

### Q5. **메시지 버스 (TODO 9-3) — 진짜 필요한가?**
"에이전트 간 통신" 은 강력해 보이지만, 현재 서비스 형태 (사용자→오케스트
레이터→에이전트 단방향) 에서 immediate 사용처가 없다. 9-2 SubAgentManager
의 결과 집약만으로 충분할 수 있다. 명시적 use case 없으면 보류 권장.

### Q6. **Token / 비용 추적의 노출 범위 (TODO 8-4)**
- 모델별 가격표를 어디에 둘지: `src/router/cost.rs` (신규) vs 환경변수.
- "비용" 은 추정값일 뿐 — overestimate 책임을 어떻게 표시할지.
- per-run 누적만? per-session 도? per-organization (cluster) 도?

---

## 4. 추천 진행 순서 (Q1=A 가정)

### Stage I — Tracing 백엔드 기초 (Section 8 일부, 1~2 commit)
1. **TODO 8-4 1단계**: `InferenceResult` 에 `Option<TokenUsage>` 추가.
   provider 별 응답 파싱에서 usage 추출 (있는 것만 — vllm/openai/anthropic
   은 표준 필드 존재. gemini 도 있음. cli backend 는 None).
2. **TODO 8-4 2단계**: `AgentOutput.usage`, `NodeExecutionResult.token_usage`
   까지 전파. RuntimeEvent::NodeCompleted 에 추가.
3. **TODO 8-5**: `RunActionType::RecoveryPhaseStarted` / `Completed` 추가
   (이미 ReplanTriggered 만 있는 것을 확장). DB 변경 없음 (action 컬럼은
   문자열).

검증: 기존 116 + 통합 테스트, 토큰 수치가 0 이상으로 누적되는지 1개 통합
테스트. 새 dep 0.

### Stage II — Tracing 프론트엔드 (Section 8 나머지, 2~3 commit)
4. **TODO 8-1**: `dag-graph.tsx` 가 `useRunSSE` 의 events 를 감지하여
   노드 상태를 즉시 업데이트.
5. **TODO 8-2**: `agent-thinking.tsx` 에 phase indicator + progress bar +
   recovery alert.
6. **TODO 8-3**: `subtask-tree.tsx` 신규 컴포넌트 + `/runs/[id]` 에 통합.

검증: `cd web && npm run build` + 신규 Playwright spec 1~2개.

### Stage III — Workflow 백엔드 (Section 7 1~3, 3~4 commit)
7. **TODO 7-3** 먼저: SubtaskPlan 구조적 검증. cycle detect / role exists /
   tool exists. 단독으로도 안전성 향상.
8. **TODO 7-1**: `RequirementAnalyzer` 도입. classify_task 호출 사이트
   감싸기 — 처음에는 분석 결과를 *기록만* 하고 build_graph 동작은 변경
   없음 (shadow mode). 실측 후에 의사결정.
9. **TODO 7-2**: `WorkflowComposer`. 우선 `chain_skills` 만 구현 (기존
   skills.yaml 9개 합성). `generate_from_description` 은 follow-up.

검증: 단위 테스트 (cycle detect 등) + 실 LLM 호출 없이 분석/조합 검증.

### Stage IV — Harness 도입 (Section 9, 점진형) — 사용자 합의 후 분리

10. **TODO 9-1**: `src/harness/mod.rs` 스켈레톤 + `AgentHarness::spawn`
    이 내부에서 `run_role` 만 호출 (sidecar). 새 API 노출 없음.
11. **TODO 9-4**: 기본 metrics 카운터 (active_sessions, total_tokens —
    Stage I 의 token_usage 활용).
12. **TODO 9-5**: hot-reload (작은 단위, 별도 commit).
13. **TODO 9-2 + 9-6**: 사용자와 별도 phase 합의.

Stage IV 까지 완료 시 commit 12~16 개 예상. 한 phase로 다 묶지 말 것.

### 보류 권장 (Q4/Q5)
- **TODO 7-4** (시각 편집기) — 필요성 합의 후 별도 phase.
- **TODO 9-3** (메시지 버스) — use case 합의 후.
- **TODO 7-2 의 generate_from_description** — chain_skills 가 잘 동작
  하는지 본 뒤 후속.

---

## 5. 사전에 작업 가능한 zero-risk 정리 (실제 코드 변경 전)

사용자 합의가 떨어지면 바로 들어갈 수 있도록 미리 해둘 수 있는 일:

### 5-1. 데이터 모델 자리 만들기 (실 적용 없음, 코드 추가 0)
- 본 문서가 그것. 의사결정/의존성을 한 곳에 잡아두면 stage 별 실행이 빠르다.

### 5-2. Stage I 사전 조사용 grep
사용자가 합의하면 바로 들어갈 수 있도록 *이번 commit 안에서는 변경하지
않지만* 영향 범위를 적어둔다:

- `InferenceResult::output` 사용처:
  - `src/agents/mod.rs:142` (`run_role_stream` 마지막)
  - `src/router/mod.rs` 내부 4 진입점
  - 약 10개의 caller — 모두 `inference.output` 만 읽음. 새 필드 추가는
    breaking 아님.
- `AgentOutput { model, content }` → 새 필드 추가 시 영향:
  - `src/orchestrator/node_executor.rs` 내 약 20개 사이트
  - 대부분 `output.content`, `output.model` 만 읽음

→ Stage I 의 첫 commit 은 **순수 additive** 로 가능하다고 사전 확인됨.

### 5-3. 테스트 확장 지점
- 토큰 추적: `tests/api_integration.rs` 에 `register_then_list_webhook`
  옆에 토큰 0 이상 검증 추가 (Stage I 마지막)
- 재계획 트레이싱: `9f52ed8` 에서 추가한 recovery_graph 테스트를 확장
  (Stage I 의 8-5 와 함께)

### 5-4. 환경변수 정리
새로 도입할 가능성이 있는 env (사전 충돌 확인용):
- `CLI_AGENT_MODEL_PRICING_PATH` — 가격표 YAML (TODO 8-4)
- `CLI_AGENT_HARNESS_TOKEN_BUDGET` — 세션별 토큰 예산 (TODO 9-1)
- 충돌 없음. 기존 env: `CLI_AGENT_RATE_LIMIT_*`, `CLI_AGENT_ALLOWED_ORIGINS`,
  `CLI_AGENT_MAX_BODY_BYTES`, `CLI_AGENT_DB_KEY` 와 prefix 일관.

---

## 6. 다음 액션

1. 사용자가 Q1~Q6 에 답한다.
2. 합의된 Stage 를 별도 plan/commit 단위로 분리 (Phase 8/9 ... 식).
3. Stage 가 끝날 때마다 `PROJECT_ANALYSIS_TODO.md` 갱신.

이 문서를 starting point 로 한 Stage I 진입은 사용자 GO 신호 후 즉시
가능하다 (additive 변경, 새 dep 없음, 회귀 위험 낮음).
