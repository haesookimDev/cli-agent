# Backend Architecture

## src/ 파일 구조 및 역할

### 진입점

| 파일 | 역할 |
|------|------|
| `src/main.rs` | CLI 진입점. 서브커맨드: `run`, `tui`, `serve`, `replay`, `memory` |
| `src/lib.rs` | 라이브러리 루트 & 모듈 익스포트 |
| `src/types.rs` | 핵심 타입: `TaskProfile`, `AgentRole`(11종), `TaskType`(9종), `RunStatus`, `SessionEventType` |
| `src/config.rs` | `.env` 기반 설정 로딩 |
| `src/command_resolver.rs` | CLI 커맨드 경로 해석 유틸 |
| `src/session_workspace.rs` | 세션별 워크스페이스 격리 (`data/sessions/{id}/`) |

### Orchestrator (`src/orchestrator/`)

| 파일 | 역할 |
|------|------|
| `mod.rs` | 핵심 오케스트레이터. 태스크 분류, DAG 빌딩, 실행 제어, 검증 루프 |
| `coder_backend.rs` | 코더 백엔드 3종: `LlmCoderBackend`, `ClaudeCodeBackend`, `CodexBackend` |
| `git_manager.rs` | Git CLI 작업 (clone, commit, push 등) |
| `repo_analyzer.rs` | 정적 레포 구조 분석 |
| `skill_loader.rs` | YAML/JSON 스킬 등록 & 로드 |
| `tool_augment.rs` | 툴 정의 보강 |
| `validator.rs` | Validator 노드 구현 (git 명령 실행, 출력 검증) |
| `prompt_composer.rs` | 에이전트 프롬프트 템플릿 렌더링 |
| `interactive.rs` | 대화형 실행 모드 처리 |

### Agents (`src/agents/`)

| 파일 | 역할 |
|------|------|
| `mod.rs` | 11개 에이전트 역할 레지스트리 및 `SubAgent` 트레이트 구현 |

에이전트 역할: `Planner`, `Extractor`, `Coder`, `Summarizer`, `Fallback`, `ToolCaller`, `Analyzer`, `Reviewer`, `Scheduler`, `ConfigManager`, `Validator`

### Runtime (`src/runtime/`)

| 파일 | 역할 |
|------|------|
| `mod.rs` | `AgentRuntime` — 동시 노드 실행, 이벤트 발행 |
| `graph.rs` | `ExecutionGraph`, DAG 노드 정의, `ExecutionPolicy` (직렬/병렬, 실패 처리) |

### Router (`src/router/`)

| 파일 | 역할 |
|------|------|
| `mod.rs` | `MultiProvider` 라우터 — LLM 선택 스코어링, 폴백 체인, 응답 캐싱 |

### Memory (`src/memory/`)

| 파일 | 역할 |
|------|------|
| `mod.rs` | `MemoryManager` — 단기(DashMap) + 장기(SQLite) 하이브리드 |
| `store.rs` | SQLite 스키마 & 쿼리 |
| `session_log.rs` | JSONL 세션 이벤트 로깅 (`data/sessions/{id}/*.jsonl`) |

### Context (`src/context/`)

| 파일 | 역할 |
|------|------|
| `mod.rs` | `ContextManager` — 토큰 예산 할당 & 계층형 컨텍스트 주입 |

### Interface (`src/interface/`)

| 파일 | 역할 |
|------|------|
| `api.rs` | Axum REST API 핸들러 전체 (sessions, runs, memory, settings, webhooks, SSE) |
| `tui.rs` | Ratatui 터미널 UI |
| `mod.rs` | 인터페이스 디스패처 |
| `dashboard.html` | 임베디드 웹 대시보드 HTML |
| `web_client.html` | 임베디드 웹 클라이언트 HTML |

### 기타 모듈

| 파일/폴더 | 역할 |
|-----------|------|
| `src/mcp/mod.rs` | `McpRegistry` — 멀티 MCP 서버 관리 |
| `src/gateway/mod.rs` | `GatewayManager` |
| `src/gateway/slack.rs` | Slack 봇 어댑터 |
| `src/gateway/discord.rs` | Discord 봇 어댑터 |
| `src/scheduler.rs` | `CronScheduler` — 60초 틱 루프 |
| `src/terminal/mod.rs` | `TerminalManager` — PTY 세션 멀티플렉싱 |
| `src/webhook/mod.rs` | `WebhookDispatcher` & `AuthManager` |

---

## 주요 Cargo.toml 의존성

| 크레이트 | 버전 | 용도 |
|---------|------|------|
| `axum` | 0.7 | 웹 프레임워크 (macros, ws features) |
| `tokio` | 1.43 | 비동기 런타임 (full) |
| `sqlx` | 0.8 | SQLite ORM (sqlite, tokio, uuid) |
| `serde` / `serde_json` | 1.0 | 직렬화/역직렬화 |
| `reqwest` | 0.12 | HTTP 클라이언트 (json, rustls-tls) |
| `dashmap` | 6.1 | 동시성 해시맵 (단기 메모리) |
| `ratatui` | 0.29 | TUI 프레임워크 |
| `hmac` + `sha2` | 0.12 / 0.10 | HMAC-SHA256 인증 |
| `portable-pty` | 0.8 | PTY 스폰 |
| `cron` | 0.13 | Cron 표현식 파싱 |
| `clap` | 4.5 | CLI 파싱 (derive, env) |
| `crossterm` | 0.28 | 터미널 제어 |
| `tower-http` | 0.6 | HTTP 미들웨어 (CORS) |
| `tracing` | 0.3 | 구조화 로깅 |
| `uuid` | 1.13 | UUID 생성 (v4, serde) |

---

## 모듈 의존 관계

```
main.rs
  └── orchestrator/mod.rs
        ├── agents/mod.rs
        │     └── router/mod.rs
        ├── runtime/mod.rs
        │     └── runtime/graph.rs
        ├── memory/mod.rs
        │     ├── memory/store.rs
        │     └── memory/session_log.rs
        ├── context/mod.rs
        ├── mcp/mod.rs
        └── orchestrator/coder_backend.rs

interface/api.rs
  └── orchestrator/mod.rs (Arc<Orchestrator>)

gateway/ → orchestrator/mod.rs (GatewayAction)
scheduler.rs → orchestrator/mod.rs (submit_run)
```
