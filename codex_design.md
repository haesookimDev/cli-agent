# TODO 구현 설계서 (Codex 개선판)

## 1. 문서 목적

`TODO.md`의 5개 요구사항을 현재 코드베이스에 맞춰 바로 구현 가능한 수준으로 정리한다.

핵심 개선 포인트:
- 추상 설계를 파일/함수 단위 변경 계획으로 구체화
- 복구 루프, 코더 세션, 파이프라인의 상태 전이를 명시
- API/DB/SSE/UI를 한 번에 연결하는 End-to-End 설계
- 운영 안전장치(승인, 리소스 제한, 호환성) 포함

---

## 2. 공통 아키텍처 원칙

1. `session_id`는 `submit_run`에서 확정 후 `execute_run`까지 동일하게 사용한다.
2. follow-up 발화는 독립 질의로 처리하지 않고 `History + recent run summary`를 강제 주입한다.
3. 로컬 파일 의도는 `filesystem/*` 우선 라우팅 정책을 planner/tool-caller prompt에 유지한다.
4. `node_token_chunk` 저장은 직렬 큐를 유지하고 `token_seq`를 추가해 재정렬 가능성을 높인다.
5. JSONL은 세션 단위 직렬 append와 concatenated JSON 복구(replay tolerant)를 유지한다.
6. SSE는 UTF-8 경계 보존 파서를 사용하고, 프론트/백 모두 청크 경계 회귀 테스트를 추가한다.

---

## 3. TODO 1: 실패 복구 및 워크플로우 재설계

### 3.1 목표

현재 `verify_completion`이 불완전(`INCOMPLETE`)이어도 종료되는 문제를 수정한다.
검증 실패 시 자동으로 원인 진단, 복구 그래프 생성, 재실행, 재검증 루프를 수행한다.

### 3.2 변경 파일

- `src/types.rs`
- `src/orchestrator/mod.rs`
- `src/runtime/graph.rs`
- `src/memory/store.rs`
- `src/interface/api.rs`
- `web/src/lib/types.ts`
- `web/src/components/status-badge.tsx`

### 3.3 타입/상태 추가

- `RunStatus::Recovering`
- `RecoveryDiagnosis`
  - `reason`
  - `failure_class` (`tool_fail | context_missing | logic_gap | timeout`)
  - `missing_roles`
  - `additional_context`
  - `should_retry`
- `RunAttemptRecord`
  - `run_id`
  - `node_id`
  - `attempt_no`
  - `stage` (`primary | recovery`)
  - `status`
  - `reason`

### 3.4 핵심 함수

- `diagnose_failure(task, results, reason) -> RecoveryDiagnosis`
- `build_recovery_graph(diagnosis, prior_results) -> ExecutionGraph`
- `execute_recovery_attempt(run_id, attempt_no, graph)`
- `should_trigger_replan(verified, diagnosis, attempt_no) -> bool`

### 3.5 상태 전이

`running -> verification_incomplete -> recovering -> (running/recovering 반복) -> succeeded|failed`

제한:
- `max_recovery_attempts = 2`
- 동일 `failure_class` 2회 연속이면 즉시 실패 종료
- 복구 실행 총 시간 상한(예: 10분)

### 3.6 API/DB

- DB: `run_attempts`
- API:
  - `GET /v1/runs/{run_id}/attempts`
  - `POST /v1/runs/{run_id}/replan` (manual trigger, admin/debug)

---

## 4. TODO 2: Coder에 Claude Code/Codex CLI 통합

### 4.1 목표

Coder를 텍스트 생성 전용 LLM 노드에서 실제 코드 실행형 백엔드로 확장한다.

### 4.2 변경 파일

- `src/config.rs`
- `src/types.rs`
- `src/orchestrator/coder_backend.rs` (신규)
- `src/orchestrator/mod.rs`
- `src/runtime/mod.rs`
- `src/memory/store.rs`

### 4.3 설정

- `CODER_BACKEND=claude_code|codex|llm`
- `CODER_COMMAND`
- `CODER_ARGS`
- `CODER_WORKING_DIR`
- `CODER_TIMEOUT_MS`
- `CODER_MAX_PARALLEL`

### 4.4 백엔드 인터페이스

- `trait CoderBackend`
  - `spawn_session(prompt, working_dir, on_chunk) -> session_id`
  - `wait_completion(session_id, timeout) -> result`
  - `kill_session(session_id)`
  - `detect_file_changes(session_id|working_dir) -> changed_files`

구현체:
- `LlmCoderBackend`
- `ClaudeCodeBackend`
- `CodexBackend`

### 4.5 런타임 이벤트

- `coder_session_started`
- `coder_output_chunk`
- `coder_file_changed`
- `coder_session_completed`

이벤트 payload에 `node_id`, `session_id`, `backend`, `stream`, `exit_code` 포함.

---

## 5. TODO 3: 병렬 코더 세션 + 실시간 UI

### 5.1 목표

복수 코더 노드를 병렬 실행하고, 각 세션 출력을 채팅에서 분리해 실시간 확인 가능하게 한다.

### 5.2 변경 파일

- `src/orchestrator/mod.rs`
- `src/interface/api.rs`
- `src/types.rs`
- `web/src/lib/types.ts`
- `web/src/components/agent-thinking.tsx`
- `web/src/hooks/use-sse.ts`

### 5.3 병렬 실행 규칙

1. Planner의 `SubtaskPlan`에서 코딩 서브태스크를 분리해 `coder_*` 노드 생성
2. Coder role policy: `max_parallelism = min(4, CODER_MAX_PARALLEL)`
3. 세션별 상태 추적: `running|completed|failed`

### 5.4 파일 충돌 방지

기본 전략:
- 코더 세션별 git worktree 격리 실행
- 병합 순서: `backend -> frontend -> shared`
- 충돌 발생 시 자동 머지 중단 + Reviewer/Developer 피드백 루프 전환

### 5.5 UI

채팅 화면 3패널:
- 좌: 세션/메모리
- 중: 대화 + 노드 타임라인
- 우: Run Inspector (코더 세션 탭 + 파일 변경 목록)

추가 카드:
- `Replan Card`
- `Parallel Coder Grid`
- `Context Source Card`

---

## 6. TODO 4: 역할기반 멀티 워크플로우 파이프라인

### 6.1 목표

단일 DAG를 넘어 `기획 -> 리뷰 -> 개발 -> QA -> 배포 -> 알림` 페이즈형 워크플로우를 지원한다.

### 6.2 변경 파일

- `src/types.rs`
- `src/orchestrator/pipeline.rs` (신규)
- `src/interface/api.rs`
- `src/memory/store.rs`
- `web/src/app/pipelines/page.tsx` (신규)
- `web/src/app/pipelines/[id]/page.tsx` (신규)

### 6.3 타입

- `PipelineWorkflow`
- `PipelinePhase`
- `PipelineRole`
- `PhaseHook`
- `PipelineExecution`
- `PhaseState`

핵심 필드:
- `depends_on`
- `feedback_target`
- `max_feedback_loops`
- `on_complete_hooks`

### 6.4 실행 엔진

`PipelineExecutor::execute`:
1. 위상 정렬로 phase 실행 순서 계산
2. phase prompt 조립(선행 결과 + 피드백)
3. orchestrator run 실행/대기
4. 승인/피드백 판단
5. feedback loop 또는 다음 phase 이동
6. hooks 실행

### 6.5 API

- `POST /v1/pipelines`
- `GET /v1/pipelines`
- `GET /v1/pipelines/{id}`
- `DELETE /v1/pipelines/{id}`
- `POST /v1/pipelines/{id}/execute`
- `GET /v1/pipeline-executions/{id}`
- `GET /v1/pipeline-executions/{id}/stream`

---

## 7. TODO 5: 다중 오케스트레이터 확장

### 7.1 목표

대규모 프로젝트를 도메인별 오케스트레이터에 분배하고 상위 메타 오케스트레이터가 통합한다.

### 7.2 변경 파일

- `src/types.rs`
- `src/orchestrator/meta.rs` (신규)
- `src/interface/api.rs` (후속)

### 7.3 모델

- `MetaTask`
- `TaskPartition`
- `MergeStrategy`
- `MetaExecutionResult`

분배 예시:
- `core orchestrator`
- `frontend orchestrator`
- `infra orchestrator`

---

## 8. 데이터/이벤트 스키마 추가

### 8.1 DB 테이블

- `run_attempts`
- `coder_sessions`
- `pipeline_workflows`
- `pipeline_executions`
- `pipeline_phase_states`

### 8.2 SSE 이벤트

- `action_event` (기존 확장)
- `coder_output_chunk` (세분화 가능)
- `pipeline_phase_update`
- `run_terminal`

---

## 9. 운영 안전장치

1. 자동 `git commit/push`는 기본 비활성 (`AGENT_AUTO_PUSH=false`)
2. destructive 작업은 정책 플래그 또는 명시 승인 필요
3. 코더 세션별 리소스 제한
   - 최대 동시 세션 수
   - stdout/stderr 버퍼 상한
   - 실행 timeout
4. 복구 루프/피드백 루프 상한 고정
5. 기능 플래그 기반 점진적 릴리즈
   - `feature_recovery_loop`
   - `feature_cli_coder`
   - `feature_pipeline`

---

## 10. 호환성 전략

1. 신규 `RunStatus::Recovering`을 모르는 클라이언트는 `running`으로 표시하도록 fallback
2. 신규 이벤트를 모르는 UI는 무시해도 동작하도록 이벤트 파서 방어 코딩
3. 마이그레이션은 additive-first로 적용하고 구버전 필드 유지

---

## 11. 관측성/메트릭

필수 지표:
- `run_success_rate`
- `replan_trigger_rate`
- `recovery_success_rate`
- `coder_session_failure_rate`
- `pipeline_phase_retry_count`
- `mean_time_to_complete`

로그 상관키:
- `session_id`
- `run_id`
- `attempt_no`
- `pipeline_execution_id`

---

## 12. 구현 순서 (최종 권장)

1. **Phase 1: CLI Coder Backend**
   - 이유: 이후 병렬/파이프라인의 기반
2. **Phase 2: Recovery Loop + run_attempts**
   - 이유: 품질 회귀 방지 핵심
3. **Phase 3: Parallel Coder + Chat Inspector UI**
   - 이유: 사용자 가시성/생산성 확보
4. **Phase 4: Pipeline Engine + API/UI**
   - 이유: 역할기반 협업 자동화
5. **Phase 5: Meta-Orchestrator**
   - 이유: 상위 확장 기능

---

## 13. 검증 계획

단계별 공통:
- `cargo build`
- `cargo test`
- `cd web && npm run build`

시나리오 테스트:
1. 불완전 답변 유도 -> recovery loop 동작 확인
2. codex/claude 백엔드 각각 실행 -> 출력/파일 변경 이벤트 확인
3. 병렬 코더 실행 -> worktree 충돌 없는지 검증
4. pipeline 실행 -> feedback loop + hook 수행 확인
