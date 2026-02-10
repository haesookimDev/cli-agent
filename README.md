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

## TUI 조작
- 포커스/이동: `Tab`, `↑/↓` 또는 `j/k`
- 작업 실행: `i` (입력 모드) -> 작업 입력 -> `Enter`
- 세션 이어하기: 세션 선택 후 `Enter` 또는 `c`
- 새 세션: `n`
- 활성 세션 해제: `x`
- 세션 삭제: 세션 선택 후 `d` -> `y` 확인
- 세션 요약(compact): `m`
- 세션 리플레이 미리보기: `v`
- 실행 제어: `s(cancel)`, `z(pause)`, `u(resume)`, `t(retry)`, `g(clone)`
- 프로필 변경: `1(planning)`, `2(extraction)`, `3(coding)`, `4(general)`, `p(순환)`
- 자동 새로고침 간격: `[` 감소, `]` 증가
- 세션 목록 크기: `,` 감소, `.` 증가
- 실행 목록 크기: `-` 감소, `=` 증가
- 활성 세션 필터 토글: `f`
- 수동 새로고침: `r`
- 종료: `q`

TUI 사용자 설정은 `data/tui-settings.json`에 저장됩니다.

TUI의 `Details` 패널에는 선택한 런의 동작 기반 시각화가 표시됩니다.
- `Behavior Graph`: 노드 상태(`WAIT/RUN/OK/ERR/SKIP`), 의존성, 선택 모델
- `Execution Timeline`: 노드 실행 구간을 시간축 lane으로 표시
- `Action Mix`: 행동 타입 분포 카운트
- `Recent Actions`: 최신 런타임 행동 이벤트 시퀀스(`node_started`, `model_selected`, `dynamic_node_added` 등)

## REST 엔드포인트
- `POST /v1/sessions`
- `GET /v1/sessions`
- `GET /v1/sessions/{session_id}`
- `GET /v1/sessions/{session_id}/runs`
- `POST /v1/runs`
- `GET /v1/runs/{run_id}`
- `POST /v1/runs/{run_id}/cancel`
- `POST /v1/runs/{run_id}/pause`
- `POST /v1/runs/{run_id}/resume`
- `POST /v1/runs/{run_id}/retry`
- `POST /v1/runs/{run_id}/clone`
- `GET /v1/runs/{run_id}/behavior`
- `GET /v1/runs/{run_id}/trace`
- `GET /v1/runs/{run_id}/stream` (SSE)
- `POST /v1/webhooks/endpoints`
- `GET /v1/webhooks/endpoints`
- `GET /v1/webhooks/deliveries`
- `POST /v1/webhooks/deliveries/{delivery_id}/retry`
- `POST /v1/webhooks/test`

`trace`는 동작 이벤트와 그래프 스냅샷을 반환합니다.
`behavior`는 시각화 친화적인 실행 lane/오프셋/행동 분포를 반환합니다.
`stream`은 실시간 행동 이벤트를 SSE로 전송합니다.
`/v1/webhooks/deliveries?dead_letter=true`로 DLQ 대상 전달 이력을 조회할 수 있습니다.

### SSE 이벤트 타입
- `action_event`: 런타임 행동 이벤트 payload
- `run_terminal`: 런 종료 상태 이벤트
- `error`: 스트림 오류

### Run 상태
- `queued`
- `cancelling`
- `cancelled`
- `paused`
- `running`
- `succeeded`
- `failed`

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
