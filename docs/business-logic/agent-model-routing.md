# Agent Execution & Model Routing

## 관련 파일

- `src/agents/mod.rs` — AgentRole, SubAgent 트레이트, AgentRegistry
- `src/router/mod.rs` — ModelRouter, ProviderKind, RoutingConstraints
- `src/types.rs` — AgentRole, TaskProfile enum

---

## 11개 AgentRole 역할

| 역할 | 역할 설명 |
|------|----------|
| `Planner` | 태스크 분류, DAG 설계, 계획 수립 |
| `Extractor` | 코드베이스/텍스트에서 관련 정보 추출 |
| `Coder` | 코드 생성, 수정, 디버깅 |
| `Summarizer` | 에이전트 출력 요약, 최종 응답 생성 |
| `Fallback` | 코더/툴 실패 시 대안 시도 |
| `ToolCaller` | MCP 툴 직접 호출 |
| `Analyzer` | 데이터 패턴 분석, 인사이트 도출 |
| `Reviewer` | 코드 리뷰, 완성도 검증 |
| `Scheduler` | 스케줄 관리 |
| `ConfigManager` | 시스템 설정 읽기/쓰기 |
| `Validator` | 실행 결과 검증, Continuation 트리거 |

---

## SubAgent 트레이트

```rust
#[async_trait]
pub trait SubAgent: Send + Sync {
    async fn run(&self, input: AgentInput) -> Result<AgentOutput>;
}

pub struct AgentInput {
    pub task: String,
    pub instructions: String,
    pub context: String,               // 메모리에서 조회한 관련 컨텍스트
    pub dependency_outputs: Vec<String>, // 선행 노드 출력
    pub brief: Option<String>,         // 태스크 요약 (선택적)
    pub working_dir: Option<PathBuf>,  // 작업 디렉토리
}

pub struct AgentOutput {
    pub model: String,    // "anthropic:claude-3-5-sonnet", "openai:gpt-4o", ...
    pub content: String,  // LLM 응답 텍스트
}
```

---

## ModelRouter: 모델 선택 흐름

```
ModelRouter.infer_stream(task_profile, prompt, ...)
  ↓
1. RoutingConstraints 조회 (TaskProfile별 제약)
2. 응답 캐시 확인 (동일 프롬프트 해시 + TTL)
3. 가용 모델 목록 필터링
4. 각 모델 스코어 계산
5. 최고 점수 모델 선택
6. CLI_ONLY=true → CLI 프로바이더 강제
7. 스트리밍 LLM 호출
```

---

## ProviderKind

```rust
pub enum ProviderKind {
    OpenAi,       // OpenAI API (GPT-4o, GPT-4 등)
    Anthropic,    // Anthropic API (Claude 3.5, Claude 3)
    Gemini,       // Google Gemini API
    Vllm,         // 로컬 vLLM 서버 (VLLM_BASE_URL)
    ClaudeCode,   // Claude Code CLI (claude 명령)
    Codex,        // OpenAI Codex CLI
    Mock,         // 테스트용 목 프로바이더
}
```

---

## RoutingConstraints (TaskProfile별)

각 `TaskProfile`에 맞는 모델 선택 기준:

| TaskProfile | Quality | Latency (ms) | Cost | Context |
|-------------|---------|-------------|------|---------|
| Planning | 0.45 | 8000 | 0.08 | 24k |
| Extraction | 0.20 | 2000 | 0.02 | 12k |
| Coding | 0.60 | 30000 | 0.15 | 32k |
| Analysis | 0.40 | 10000 | 0.06 | 20k |
| General | 0.30 | 5000 | 0.04 | 16k |

---

## 스코어링 알고리즘

```
ModelSpec { quality: f32, latency_ms: u64, cost: f32, context_window: u32 }

score = (quality_weight * model.quality)
      + (latency_weight * (1.0 - model.latency_ms / max_latency))
      + (cost_weight * (1.0 - model.cost / max_cost))
      + (context_bonus if model.context_window >= required)
```

---

## 폴백 체인 순서

기본 폴백 체인 (설정에 따라 변경 가능):

```
1. Anthropic Claude 3.5 Sonnet  (고품질 기본)
2. OpenAI GPT-4o                (폴백 1)
3. Google Gemini 1.5 Pro        (폴백 2)
4. vLLM 로컬 모델               (폴백 3, VLLM_BASE_URL 설정 시)
5. Mock                         (테스트 환경)
```

`MODEL_CLI_ONLY=true` 설정 시: Claude Code CLI 또는 Codex CLI만 사용.

---

## CLI 프로바이더 처리

`ClaudeCode` / `Codex` 프로바이더는 외부 CLI 프로세스를 스폰:

```
ModelRouter.infer_stream_in_dir_with_cli_output()
  ↓
std::process::Command::new(MODEL_CLI_COMMAND)
  .args(MODEL_CLI_ARGS)
  .current_dir(working_dir)
  .stdin(Stdio::piped())
  .stdout(Stdio::piped())
  .spawn()

stdout를 실시간으로 읽어 CliOutputCallback 호출
→ SSE를 통해 프론트엔드에 실시간 전달
```

---

## 응답 캐싱

```rust
CacheEntry {
  response: String,
  created_at: Instant,
  ttl: Duration,    // 기본 5분
}
```

동일 `(provider, model, prompt_hash)` 조합에 대해 TTL 내 캐시 반환.
LLM 비용 절감 및 응답 속도 향상 목적.
