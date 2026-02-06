# cli-agent

Rust 기반 멀티 에이전트 CLI/TUI 플랫폼입니다.

## 주요 기능
- 메인 오케스트레이터 + 역할 기반 서브 에이전트
- DAG 기반 의존성 실행(직렬/병렬), 실패 정책, 폴백 노드
- 멀티 모델 라우팅(OpenAI/Anthropic/Gemini/Ollama/Mock)
- 세션 이벤트 JSONL 저장(`data/sessions/{session_id}.jsonl`)
- SQLite 기반 장기 메모리 + TTL 단기 메모리
- 컨텍스트 계층화 및 토큰 예산 최적화
- REST + Webhook 인터페이스
- API Key + HMAC 인증(`X-API-Key/X-Signature/X-Timestamp/X-Nonce`)

## 빌드/테스트
```bash
cargo test
```

## CLI
```bash
# 동기 실행(완료까지 대기)
cargo run -- run --task "build a webhook enabled agent" --profile coding

# 비동기 제출
cargo run -- run --task "do something" --profile general --no-wait

# TUI
cargo run -- tui

# API 서버
cargo run -- serve --host 0.0.0.0 --port 8080

# 세션 리플레이
cargo run -- replay --session <SESSION_UUID>

# 메모리 운영
cargo run -- memory compact --session <SESSION_UUID>
cargo run -- memory vacuum
```

## REST 엔드포인트
- `POST /v1/sessions`
- `POST /v1/runs`
- `GET /v1/runs/{run_id}`
- `POST /v1/webhooks/endpoints`
- `POST /v1/webhooks/test`

## 인증 시그니처
시그니처 원문:
```
{timestamp}.{nonce}.{raw_body}
```

알고리즘:
- `HMAC-SHA256`
- hex 인코딩 문자열을 `X-Signature`에 전달

필수 헤더:
- `X-API-Key`
- `X-Signature`
- `X-Timestamp` (unix seconds)
- `X-Nonce` (재사용 금지)

## 환경 변수
- `AGENT_DATA_DIR` (default: `data`)
- `AGENT_DATABASE_URL` (default: `sqlite://data/agent.db`)
- `AGENT_API_KEY` (default: `local-dev-key`)
- `AGENT_API_SECRET` (default: `local-dev-secret`)
- `AGENT_SERVER_HOST` (default: `0.0.0.0`)
- `AGENT_SERVER_PORT` (default: `8080`)
- `AGENT_MAX_PARALLELISM` (default: `8`)
- `AGENT_MAX_GRAPH_DEPTH` (default: `6`)
- `AGENT_MAX_CONTEXT_TOKENS` (default: `16000`)
- `AGENT_WEBHOOK_TIMEOUT_SECS` (default: `5`)
- `OLLAMA_BASE_URL` (default: `http://127.0.0.1:11434`)
