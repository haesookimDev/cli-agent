# Development Status

## 완료된 기능

### 핵심 오케스트레이터

- [x] 태스크 분류 (9가지 TaskType, LLM + 키워드 폴백) — `src/orchestrator/mod.rs`
- [x] 적응형 DAG 그래프 빌딩 (타입별 노드 구성) — `src/orchestrator/mod.rs`
- [x] 병렬 DAG 실행 엔진 (최대 4 병렬) — `src/runtime/mod.rs`, `src/runtime/graph.rs`
- [x] 완료 검증 + Continuation 루프 (최대 2회) — `src/orchestrator/mod.rs`
- [x] 실행 취소 / 일시정지 / 재개 (`AtomicBool` 기반) — `src/orchestrator/mod.rs`
- [x] 세션별 워크스페이스 격리 — `src/session_workspace.rs`
- [x] 코더 에이전트 병렬 실행 — 각 Coder 노드가 `node.instructions`로 개별 서브태스크 실행 (2026-03-20)
- [x] 실행 중 태스크 재분류 (M-5) — `OnNodeCompletedFn` 반환 타입 확장, static 노드 Skipped 처리 (2026-03-20)

### 에이전트 시스템

- [x] 11개 AgentRole 구현 (Planner, Extractor, Coder, Summarizer, Fallback, ToolCaller, Analyzer, Reviewer, Scheduler, ConfigManager, Validator) — `src/agents/mod.rs`
- [x] SubAgent 트레이트 + AgentRegistry — `src/agents/mod.rs`
- [x] 프롬프트 구성 엔진 — `src/orchestrator/prompt_composer.rs`
- [x] 역할 기반 멀티 워크플로우 (`skills/multi_role_dev.yaml`): plan → review_plan → code → validate_lint → summarize (2026-03-20)
- [x] in-graph Reviewer → Planner 피드백: INCOMPLETE 시 replan 주입 (2026-03-20)
- [x] 에이전트 출력 구조화 (M-1) — `extract_json_object()` 헬퍼, Planner JSON 파싱 강화 (2026-03-20)
- [x] 서킷 브레이커 노드 단위 세분화 (M-2) — `node_failures`, `mark_dependents_pending_as_skipped` (2026-03-20)
- [x] 동적 노드 생성 상한선 (M-3) — `MAX_DYNAMIC_SUBTASKS_PER_PLAN = 50` (2026-03-20)

### 모델 라우팅

- [x] MultiProvider 라우팅 (OpenAI, Anthropic, Gemini, vLLM) — `src/router/mod.rs`
- [x] 스코어링 기반 모델 선택 — `src/router/mod.rs`
- [x] CLI 모델 백엔드 (Claude Code, Codex) — `src/orchestrator/coder_backend.rs`
- [x] 응답 캐싱 (TTL 기반) — `src/router/mod.rs`
- [x] 폴백 체인 — `src/router/mod.rs`
- [x] 폴백 타임아웃 상수화 (M-4) — `FALLBACK_CHAIN_DEADLINE_SECS = 300` (2026-03-20)
- [x] 요청 수준 HTTP 타임아웃 (M-6) — `INFER_TOTAL_TIMEOUT_SECS = 600`, `tokio::time::timeout` 래핑 (2026-03-20)

### 메모리 & 컨텍스트

- [x] 하이브리드 메모리 (단기 DashMap + 장기 SQLite) — `src/memory/mod.rs`
- [x] 메모리 스코어링 (최신성/중요도/유사도) — `src/memory/mod.rs`
- [x] JSONL 세션 이벤트 로깅 — `src/memory/session_log.rs`
- [x] 토큰 예산 할당 컨텍스트 관리 — `src/context/mod.rs`
- [x] 글로벌 지식 베이스 — `src/memory/store.rs`
- [x] 임베딩 벡터 메모리 검색 — `src/memory/embedding.rs`, SQLite `embedding BLOB`, OpenAI-compatible API (`EMBEDDING_API_URL`, `EMBEDDING_MODEL`) (2026-03-20)

### API & 인터페이스

- [x] REST API (sessions, runs, memory, settings, models, schedules, workflows, webhooks) — `src/interface/api.rs`
- [x] HMAC-SHA256 인증 — `src/interface/api.rs`, `src/webhook/mod.rs`
- [x] SSE 스트리밍 (실시간 이벤트) — `src/interface/api.rs`
- [x] WebSocket 스트리밍 — `/v1/runs/:run_id/ws` 엔드포인트 (2026-03-20)
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
- [x] Playwright E2E 테스트 — `web/e2e/` (navigation, runner; API 모킹 포함) (2026-03-20)

### 스킬 & 스케줄러

- [x] YAML 스킬 로더 — `src/orchestrator/skill_loader.rs`
- [x] 6개 내장 스킬 — `skills/`
- [x] AutoSkillRoute 자동 라우팅 — `src/orchestrator/mod.rs`
- [x] Cron 스케줄러 (60s 틱) — `src/scheduler.rs`

### 멀티 오케스트레이터 클러스터

- [x] `OrchestratorCluster` — 이름 기반 멤버 등록, 서브태스크 병렬 디스패치 — `src/orchestrator/cluster.rs` (2026-03-20)
- [x] 클러스터 런 상태 집계 — `ClusterRunStatus`: Succeeded / PartialFailure / Failed (2026-03-20)
- [x] `CLUSTER_MEMBERS` 환경변수로 멤버 구성 (기본값: `"default"`) (2026-03-20)
- [x] REST API: `GET/POST /v1/cluster/runs`, `GET /v1/cluster/members`, `GET /v1/cluster/runs/:id` (2026-03-20)

### 통합

- [x] MCP 멀티 서버 레지스트리 — `src/mcp/mod.rs`
- [x] Slack 게이트웨이 — `src/gateway/slack.rs`
- [x] Discord 게이트웨이 — `src/gateway/discord.rs`
- [x] 웹훅 디스패처 — `src/webhook/mod.rs`
- [x] PTY 터미널 세션 — `src/terminal/mod.rs`
- [x] Git 작업 관리자 — `src/orchestrator/git_manager.rs`
- [x] 레포 분석기 — `src/orchestrator/repo_analyzer.rs`
- [x] 세션 리플레이 — `cargo run -- replay`
- [x] 업데이트 알림 — `src/notifier.rs`, `agent notify-release` CLI (`SLACK_NOTIFY_WEBHOOK_URL`, `DISCORD_NOTIFY_WEBHOOK_URL`), `RELEASE.md` (2026-03-20)

---

## 테스트 현황

```bash
cargo test  # 81개 테스트 통과 (2026-03-20)
cd web && npm run test:e2e  # Playwright E2E (chromium)
```

주요 테스트 위치: `src/` 각 모듈의 `#[cfg(test)]` 블록, `web/e2e/`
