# Architecture Overview

## 기술 스택

| 레이어 | 기술 |
|--------|------|
| 백엔드 | Rust (Edition 2024) + Axum 0.7 + SQLite (sqlx 0.8) + Tokio 1.43 |
| 프론트엔드 | Next.js 15 (App Router) + React 19 + TypeScript + Tailwind CSS 4 |
| 인증 | HMAC-SHA256 (`X-API-Key`, `X-Signature`, `X-Timestamp`, `X-Nonce`) |
| 데이터베이스 | SQLite (`data/agent.db`) |
| 비동기 런타임 | Tokio (full features) |

---

## 모듈 맵

| 모듈 | 경로 | 역할 |
|------|------|------|
| Orchestrator | `src/orchestrator/mod.rs` | 태스크 분류, DAG 빌딩, 실행 제어, 검증 루프 |
| Agents | `src/agents/mod.rs` | 11개 에이전트 역할 레지스트리 |
| Router | `src/router/mod.rs` | 멀티 프로바이더 모델 선택, 스코어링, 응답 캐싱 |
| Runtime | `src/runtime/mod.rs` + `graph.rs` | DAG 그래프 실행 엔진, 이벤트 발행 |
| Memory | `src/memory/` | 단기(DashMap) + 장기(SQLite) 하이브리드 메모리 |
| Context | `src/context/mod.rs` | 토큰 예산 할당, 컨텍스트 주입 |
| API | `src/interface/api.rs` | Axum REST 엔드포인트 (100+ 핸들러) |
| TUI | `src/interface/tui.rs` | Ratatui 터미널 UI |
| MCP | `src/mcp/mod.rs` | 멀티 서버 Model Context Protocol 레지스트리 |
| Gateway | `src/gateway/` | Slack + Discord 어댑터 |
| Scheduler | `src/scheduler.rs` | Cron 기반 워크플로우 실행 (60s 틱) |
| Terminal | `src/terminal/mod.rs` | PTY 세션 멀티플렉싱 |
| Webhook | `src/webhook/mod.rs` | 웹훅 디스패처 & 인증 |

---

## 시스템 데이터 흐름

```
사용자 입력 (Web UI / CLI / Slack / Discord)
        ↓
  HMAC-SHA256 인증
        ↓
  POST /v1/runs → Orchestrator.submit_run()
        ↓
  [비동기 백그라운드]
  태스크 분류 (LLM 또는 키워드 폴백)
        ↓
  적응형 DAG 그래프 빌딩
        ↓
  AgentRuntime.execute_graph()
  ┌────────────────────────┐
  │  노드 병렬/직렬 실행    │
  │  ModelRouter → LLM 호출 │
  │  RunActionEvent 기록    │
  │  SSE 스트림 발행        │
  └────────────────────────┘
        ↓
  Validator 검증 → 성공/재시도(최대 2회)
        ↓
  RunStatus: Succeeded / Failed
        ↓
프론트엔드 SSE 수신 → 메시지 렌더링
```

---

## 검증 명령어

```bash
cargo build          # 오류/경고 없음
cargo test           # 14개 테스트 통과
cd web && npm run build   # 15개 라우트 컴파일
```

---

## 디렉토리 구조 요약

```
cli-agent/
├── src/           # Rust 백엔드 (21개 .rs 파일)
├── web/           # Next.js 프론트엔드
├── skills/        # 내장 스킬 YAML (6개)
├── data/          # 런타임 데이터 (SQLite, 세션 로그, 레포)
├── scripts/       # 빌드/개발 스크립트
├── docs/          # 이 문서 폴더
├── .env.example   # 환경 변수 템플릿
└── mcp_servers.json  # MCP 서버 설정
```

---

## 핵심 환경 변수 (빠른 참조)

| 변수 | 기본값 | 역할 |
|------|--------|------|
| `AGENT_API_KEY` | `local-dev-key` | API 인증 키 |
| `AGENT_API_SECRET` | `local-dev-secret` | HMAC 서명 시크릿 |
| `AGENT_SERVER_PORT` | `8080` | 백엔드 포트 |
| `AGENT_WEB_PORT` | `3000` | 프론트엔드 포트 |
| `ANTHROPIC_API_KEY` | — | Claude 모델 사용 |
| `OPENAI_API_KEY` | — | GPT 모델 사용 |
| `MCP_ENABLED` | `false` | MCP 활성화 |

전체 목록: [guides/environment.md](../guides/environment.md)
