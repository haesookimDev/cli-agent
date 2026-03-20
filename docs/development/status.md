# Development Status

## 완료된 기능

### 핵심 오케스트레이터

- [x] 태스크 분류 (9가지 TaskType, LLM + 키워드 폴백) — `src/orchestrator/mod.rs`
- [x] 적응형 DAG 그래프 빌딩 (타입별 노드 구성) — `src/orchestrator/mod.rs`
- [x] 병렬 DAG 실행 엔진 (최대 4 병렬) — `src/runtime/mod.rs`, `src/runtime/graph.rs`
- [x] 완료 검증 + Continuation 루프 (최대 2회) — `src/orchestrator/mod.rs`
- [x] 실행 취소 / 일시정지 / 재개 (`AtomicBool` 기반) — `src/orchestrator/mod.rs`
- [x] 세션별 워크스페이스 격리 — `src/session_workspace.rs`

### 에이전트 시스템

- [x] 11개 AgentRole 구현 (Planner, Extractor, Coder, Summarizer, Fallback, ToolCaller, Analyzer, Reviewer, Scheduler, ConfigManager, Validator) — `src/agents/mod.rs`
- [x] SubAgent 트레이트 + AgentRegistry — `src/agents/mod.rs`
- [x] 프롬프트 구성 엔진 — `src/orchestrator/prompt_composer.rs`

### 모델 라우팅

- [x] MultiProvider 라우팅 (OpenAI, Anthropic, Gemini, vLLM) — `src/router/mod.rs`
- [x] 스코어링 기반 모델 선택 — `src/router/mod.rs`
- [x] CLI 모델 백엔드 (Claude Code, Codex) — `src/orchestrator/coder_backend.rs`
- [x] 응답 캐싱 (TTL 기반) — `src/router/mod.rs`
- [x] 폴백 체인 — `src/router/mod.rs`

### 메모리 & 컨텍스트

- [x] 하이브리드 메모리 (단기 DashMap + 장기 SQLite) — `src/memory/mod.rs`
- [x] 메모리 스코어링 (최신성/중요도/유사도) — `src/memory/mod.rs`
- [x] JSONL 세션 이벤트 로깅 — `src/memory/session_log.rs`
- [x] 토큰 예산 할당 컨텍스트 관리 — `src/context/mod.rs`
- [x] 글로벌 지식 베이스 — `src/memory/store.rs`

### API & 인터페이스

- [x] REST API (sessions, runs, memory, settings, models, schedules, workflows, webhooks) — `src/interface/api.rs`
- [x] HMAC-SHA256 인증 — `src/interface/api.rs`, `src/webhook/mod.rs`
- [x] SSE 스트리밍 (실시간 이벤트) — `src/interface/api.rs`
- [x] TUI (Ratatui) — `src/interface/tui.rs`
- [x] 임베디드 웹 대시보드 — `src/interface/dashboard.html`

### 프론트엔드

- [x] Next.js 15 App Router (18개 페이지) — `web/src/app/`
- [x] 채팅 인터페이스 — `web/src/app/chat/page.tsx`
- [x] SSE 스트리밍 클라이언트 — `web/src/hooks/use-sse.ts`
- [x] DAG 트레이스 시각화 — `web/src/trace/dag-graph.tsx`
- [x] 에이전트 행동 분석 — `web/src/components/behavior/`
- [x] PTY 터미널 UI — `web/src/app/terminal/page.tsx`
- [x] HMAC 클라이언트 인증 — `web/src/lib/api-client.ts`

### 스킬 & 스케줄러

- [x] YAML 스킬 로더 — `src/orchestrator/skill_loader.rs`
- [x] 6개 내장 스킬 — `skills/`
- [x] AutoSkillRoute 자동 라우팅 — `src/orchestrator/mod.rs`
- [x] Cron 스케줄러 (60s 틱) — `src/scheduler.rs`

### 통합

- [x] MCP 멀티 서버 레지스트리 — `src/mcp/mod.rs`
- [x] Slack 게이트웨이 — `src/gateway/slack.rs`
- [x] Discord 게이트웨이 — `src/gateway/discord.rs`
- [x] 웹훅 디스패처 — `src/webhook/mod.rs`
- [x] PTY 터미널 세션 — `src/terminal/mod.rs`
- [x] Git 작업 관리자 — `src/orchestrator/git_manager.rs`
- [x] 레포 분석기 — `src/orchestrator/repo_analyzer.rs`
- [x] 세션 리플레이 — `cargo run -- replay`

---

## 테스트 현황

```bash
cargo test  # 14개 테스트 통과
```

주요 테스트 위치: `src/` 각 모듈의 `#[cfg(test)]` 블록
