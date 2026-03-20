# Extending the System

## 새 AgentRole 추가

**파일**: `src/agents/mod.rs`, `src/types.rs`

### 1. `AgentRole` enum에 추가 (`src/types.rs`)

```rust
pub enum AgentRole {
    // 기존 역할들...
    MyNewRole,  // 추가
}
```

### 2. `AgentRegistry`에 구현 추가 (`src/agents/mod.rs`)

```rust
impl AgentRegistry {
    pub async fn run_role(&self, role: AgentRole, input: AgentInput) -> Result<AgentOutput> {
        match role {
            // 기존 매칭들...
            AgentRole::MyNewRole => {
                let prompt = format!(
                    "당신은 [역할 설명] 전문가입니다.\n\n태스크: {}\n\n{}",
                    input.task, input.instructions
                );
                self.router.infer_stream(&prompt, TaskProfile::General, ...).await
            }
        }
    }
}
```

### 3. DAG 빌딩에서 새 노드로 사용 (`src/orchestrator/mod.rs`)

```rust
AgentNode {
    id: "my_node".to_string(),
    role: AgentRole::MyNewRole,
    task: "...",
    instructions: "...",
    dependencies: vec!["plan".to_string()],
    policy: ExecutionPolicy::default(),
}
```

---

## 새 TaskType 추가

**파일**: `src/types.rs`, `src/orchestrator/mod.rs`

### 1. enum 추가

```rust
pub enum TaskType {
    // 기존 타입들...
    MyNewType,
}
```

### 2. 분류 로직에 키워드 추가 (lines 2090-2101)

```rust
// 키워드 폴백
if task.contains("my_keyword") {
    TaskType::MyNewType
}
```

### 3. DAG 빌딩에 케이스 추가 (lines 2187-2415)

```rust
TaskType::MyNewType => {
    vec![
        AgentNode { id: "plan", role: AgentRole::Planner, ... },
        AgentNode { id: "my_step", role: AgentRole::MyNewRole, dependencies: vec!["plan"], ... },
        AgentNode { id: "summarize", role: AgentRole::Summarizer, dependencies: vec!["my_step"], ... },
    ]
}
```

---

## 새 모델 프로바이더 추가

**파일**: `src/router/mod.rs`

### 1. `ProviderKind` enum 추가

```rust
pub enum ProviderKind {
    // 기존 프로바이더들...
    MyProvider,
}
```

### 2. ModelSpec 등록

```rust
// 가용 모델 목록에 추가
ModelSpec {
    provider: ProviderKind::MyProvider,
    model_id: "my-model-v1".to_string(),
    quality: 0.8,
    latency_ms: 3000,
    cost: 0.01,
    context_window: 32000,
    tool_call_accuracy: 0.9,
}
```

### 3. 추론 구현

```rust
async fn call_my_provider(&self, prompt: &str, model: &ModelSpec) -> Result<String> {
    let client = reqwest::Client::new();
    let response = client.post("https://api.myprovider.com/v1/completions")
        .bearer_auth(&self.config.my_api_key)
        .json(&json!({ "prompt": prompt, "model": model.model_id }))
        .send().await?;
    // 응답 파싱...
}
```

---

## 커스텀 스킬 YAML 작성

`data/skills/my_skill.yaml` 파일 생성:

```yaml
name: my_custom_skill
description: "내 커스텀 워크플로우 설명"
keywords:
  - "my keyword"
  - "trigger phrase"
nodes:
  - id: plan
    role: Planner
    task: "작업 계획 수립"
    instructions: "사용자 요청을 분석하고 실행 계획을 세우세요"
    dependencies: []
    policy:
      timeout_ms: 30000
      on_failure: Fail
      allow_parallel: false

  - id: execute
    role: Coder           # 또는 Analyzer, ToolCaller, ConfigManager 등
    task: "실제 작업 실행"
    instructions: "plan 노드의 계획에 따라 실행하세요"
    dependencies: [plan]
    policy:
      timeout_ms: 120000
      on_failure: Retry
      allow_parallel: false

  - id: summarize
    role: Summarizer
    task: "결과 요약"
    dependencies: [execute]
```

서버 재시작 없이 `data/skills/` 디렉토리를 감시하여 자동 로드됨.

---

## 새 MCP 서버 연결

`mcp_servers.json`에 서버 추가:

```json
{
  "servers": {
    "my_server": {
      "command": "npx",
      "args": ["-y", "my-mcp-server-package"],
      "env": {
        "MY_API_KEY": "${MY_API_KEY}"
      }
    }
  }
}
```

`.env`에 자격증명 추가:

```bash
MY_API_KEY=my-secret-key
```

서버 재시작 후 `/tools` 페이지에서 새 툴 확인.

---

## 새 게이트웨이 어댑터 추가

**파일**: `src/gateway/` 새 파일 생성 (예: `src/gateway/teams.rs`)

```rust
pub struct TeamsAdapter {
    webhook_url: String,
}

#[async_trait]
impl GatewayAdapter for TeamsAdapter {
    async fn start(&self, manager: Arc<GatewayManager>) -> Result<()> {
        // 웹훅 수신 서버 시작
        // ...
    }

    async fn send_message(&self, channel_id: &str, text: &str) -> Result<()> {
        // Microsoft Teams에 메시지 전송
        // ...
    }

    fn platform(&self) -> Platform {
        Platform::Teams  // Platform enum에 추가 필요
    }
}
```

`src/gateway/mod.rs`의 `GatewayManager::new()`에 어댑터 등록.
