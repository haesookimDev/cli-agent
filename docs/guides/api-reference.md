# REST API Reference

## 인증

모든 요청에 HMAC-SHA256 서명 헤더 필요:

```
X-API-Key: <AGENT_API_KEY>
X-Signature: <HMAC-SHA256(AGENT_API_SECRET, "{timestamp}.{nonce}.{body}")>
X-Timestamp: <unix-ms-timestamp>
X-Nonce: <uuid-v4>
```

타임스탬프 유효 범위: ±5분 (재전송 공격 방지)

---

## Sessions

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `POST` | `/v1/sessions` | 세션 생성 |
| `GET` | `/v1/sessions` | 세션 목록 조회 |
| `GET` | `/v1/sessions/:id` | 세션 상세 조회 |
| `DELETE` | `/v1/sessions/:id` | 세션 삭제 |
| `GET` | `/v1/sessions/:id/messages` | 세션 메시지 조회 |

```json
// POST /v1/sessions
// Request
{ "metadata": {} }
// Response
{ "session_id": "uuid", "created_at": "..." }

// GET /v1/sessions/:id/messages?limit=200
// Response
[
  { "id": "uuid", "role": "user", "content": "...", "created_at": "...", "run_id": "uuid|null" },
  { "id": "uuid", "role": "agent", "content": "...", "created_at": "...", "run_id": "uuid" }
]
```

---

## Runs

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `POST` | `/v1/runs` | 실행 생성 (태스크 제출) |
| `GET` | `/v1/runs` | 실행 목록 조회 |
| `GET` | `/v1/runs/:id` | 실행 상세 조회 |
| `GET` | `/v1/runs/:id/trace` | 실행 트레이스 조회 |
| `GET` | `/v1/runs/:id/stream` | SSE 스트림 (실시간 이벤트) |
| `POST` | `/v1/runs/:id/cancel` | 실행 취소 |
| `POST` | `/v1/runs/:id/pause` | 실행 일시정지 |
| `POST` | `/v1/runs/:id/resume` | 실행 재개 |

```json
// POST /v1/runs
// Request
{
  "task": "이 코드를 분석해줘",
  "profile": "general",        // general | planning | extraction | coding
  "session_id": "uuid",        // 선택적, 없으면 신규 생성
  "repo_url": "https://..."    // 선택적
}
// Response (202 Accepted)
{
  "run_id": "uuid",
  "session_id": "uuid",
  "status": "Queued"
}

// GET /v1/runs/:id
// Response
{
  "run_id": "uuid",
  "session_id": "uuid",
  "task": "...",
  "profile": "general",
  "status": "Succeeded",        // Queued | Running | Succeeded | Failed | Cancelled
  "created_at": "...",
  "outputs": [
    {
      "role": "Planner",
      "output_text": "...",
      "model_used": "anthropic:claude-3-5-sonnet",
      "tokens": 150,
      "succeeded": true
    }
  ]
}

// GET /v1/runs/:id/stream?poll_ms=400&behavior=false
// SSE 이벤트 스트림
data: {"type":"action_event","payload":{"seq":1,"event_type":"NodeStarted",...}}
data: {"type":"run_terminal","payload":{"status":"Succeeded"}}
```

---

## Memory

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/v1/sessions/:id/memory` | 세션 메모리 조회 |
| `POST` | `/v1/sessions/:id/memory` | 세션 메모리 추가 |
| `PUT` | `/v1/sessions/:id/memory/:key` | 세션 메모리 수정 |
| `DELETE` | `/v1/sessions/:id/memory/:key` | 세션 메모리 삭제 |
| `GET` | `/v1/memory/global` | 글로벌 지식 조회 |
| `POST` | `/v1/memory/global` | 글로벌 지식 추가 |

---

## Workflows (Skills)

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/v1/workflows` | 워크플로우 목록 |
| `GET` | `/v1/workflows/:id` | 워크플로우 상세 |
| `POST` | `/v1/workflows` | 워크플로우 생성 |
| `DELETE` | `/v1/workflows/:id` | 워크플로우 삭제 |

---

## Schedules

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/v1/schedules` | 스케줄 목록 |
| `POST` | `/v1/schedules` | 스케줄 생성 |
| `DELETE` | `/v1/schedules/:id` | 스케줄 삭제 |

```json
// POST /v1/schedules
{
  "workflow_id": "uuid",
  "cron": "0 9 * * 1-5",    // 평일 오전 9시
  "enabled": true
}
```

---

## Settings & Models

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/v1/settings` | 현재 설정 조회 |
| `PUT` | `/v1/settings` | 설정 업데이트 |
| `GET` | `/v1/models` | 가용 모델 목록 |

---

## Webhooks

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `POST` | `/v1/webhooks` | 웹훅 등록 |
| `GET` | `/v1/webhooks` | 웹훅 목록 |
| `DELETE` | `/v1/webhooks/:id` | 웹훅 삭제 |

웹훅 이벤트: `run.started`, `run.completed`, `run.failed`, `agent.started`, `agent.completed`

---

## Gateway

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `POST` | `/gateway/slack/events` | Slack 이벤트 수신 |
| `POST` | `/gateway/discord/interactions` | Discord 인터랙션 수신 |
