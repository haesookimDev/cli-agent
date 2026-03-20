# Release Notes

## v0.3.0 - 2026-03-20

- feat: Notify-release CLI command — send release announcements to Slack/Discord via incoming webhooks
- fix: Gate Windows-only test with `#[cfg(windows)]` to prevent false failures on macOS/Linux

## v0.2.0 - 2026-03-20

- feat: Vector embedding search — switched memory similarity search from keyword to OpenAI-compatible embedding API (`EMBEDDING_API_URL`, `OPENAI_API_KEY`, `EMBEDDING_MODEL`)
- feat: WebSocket streaming — added `/v1/runs/:run_id/ws` endpoint alongside existing SSE
- feat: Parallel coder tasks — each Coder node now runs its own subtask via `node.instructions`
- feat: Multi-role dev workflow — `skills/multi_role_dev.yaml` with Planner → Reviewer → Coder → Validator → Summarizer pipeline
- fix: Structured Planner output — `extract_json_object()` removes markdown fences before JSON parse
- fix: Node-scoped circuit breaker — `node_failures` replaces `role_failures`; only direct dependents are skipped on failure
- fix: Dynamic subtask cap — `MAX_DYNAMIC_SUBTASKS_PER_PLAN = 50`

## v0.1.0 - 2026-03-01

- Initial release — Rust multi-agent orchestrator with DAG execution, REST/WebSocket API, Slack/Discord gateway, TUI, and SQLite-backed memory
