# Frontend Architecture

## 기술 스택

- Next.js 15 (App Router)
- React 19
- TypeScript (strict mode)
- Tailwind CSS 4
- Server-Sent Events (SSE) for real-time streaming

---

## 페이지 구조 (`web/src/app/`)

| 경로 | 파일 | 역할 |
|------|------|------|
| `/` | `page.tsx` | Runner 홈 — 태스크 직접 실행 |
| `/chat` | `chat/page.tsx` | 대화형 채팅 인터페이스 |
| `/sessions` | `sessions/page.tsx` | 세션 목록 |
| `/sessions/[id]` | `sessions/[id]/page.tsx` | 세션 상세 & 메시지 히스토리 |
| `/runs` | `runs/page.tsx` | 실행 이력 목록 |
| `/runs/[id]` | `runs/[id]/page.tsx` | 실행 상세 & 트레이스 |
| `/behavior` | `behavior/page.tsx` | 에이전트 행동 시각화 |
| `/trace` | `trace/page.tsx` | DAG 그래프 & 이벤트 타임라인 |
| `/memory` | `memory/page.tsx` | 메모리 조회/관리 UI |
| `/workflows` | `workflows/page.tsx` | 워크플로우(스킬) 목록 |
| `/workflows/[id]` | `workflows/[id]/page.tsx` | 워크플로우 상세 |
| `/schedules` | `schedules/page.tsx` | Cron 스케줄 관리 |
| `/settings` | `settings/page.tsx` | 앱 설정 |
| `/terminal` | `terminal/page.tsx` | PTY 터미널 인터페이스 |
| `/tools` | `tools/page.tsx` | MCP 툴 카탈로그 |
| `/results` | `results/page.tsx` | 결과 집계 뷰 |
| `/api/stream/[runId]` | `api/stream/[runId]/route.ts` | SSE 프록시 (백엔드 → 클라이언트) |

---

## 컴포넌트 (`web/src/components/`)

| 컴포넌트 | 역할 |
|---------|------|
| `nav.tsx` | 전역 네비게이션 |
| `chat-bubble.tsx` | 채팅 메시지 말풍선 |
| `agent-thinking.tsx` | 에이전트 추론 과정 시각화 (action events 타임라인) |
| `tool-call-card.tsx` | 툴 호출 카드 UI |
| `stream-log.tsx` | 실시간 로그 스트림 표시 |
| `status-badge.tsx` | 실행 상태 배지 |
| `run-actions.tsx` | 실행 제어 버튼 (cancel, pause) |
| `terminal-panel.tsx` | 터미널 에뮬레이터 패널 |
| `behavior/swim-lane.tsx` | 에이전트별 실행 타임라인 |
| `behavior/action-mix.tsx` | 액션 타입 분포 차트 |
| `behavior/summary-metrics.tsx` | 성능 지표 요약 |
| `trace/dag-graph.tsx` | DAG 그래프 렌더링 |
| `trace/event-timeline.tsx` | 이벤트 시퀀스 타임라인 |

---

## API 클라이언트 (`web/src/lib/api-client.ts`)

모든 API 호출에 HMAC-SHA256 인증 헤더를 자동 첨부:

```typescript
// 서명 생성
const timestamp = Date.now().toString()
const nonce = crypto.randomUUID()
const signature = HMAC-SHA256(API_SECRET, `${timestamp}.${nonce}.${rawBody}`)

// 헤더
X-API-Key: <AGENT_API_KEY>
X-Signature: <hex-encoded-signature>
X-Timestamp: <timestamp>
X-Nonce: <nonce>
```

---

## SSE 스트리밍 흐름

```
채팅 제출
  → POST /v1/runs (202 즉시 반환)
  → SSE 연결: GET /api/stream/{runId}?poll_ms=400
       ↓ (Next.js API Route)
  → 백엔드 프록시: GET /v1/runs/{runId}/stream
       ↓ (백엔드 폴링 400ms)
  → 이벤트 수신: action_event | behavior_snapshot | run_terminal
       ↓
  → run_terminal 수신 시 SSE 종료
  → GET /v1/sessions/{sessionId}/messages 로 최종 메시지 로드
```

SSE 훅: `web/src/hooks/use-sse.ts`
SSE 프록시: `web/src/app/api/stream/[runId]/route.ts`

---

## 환경 변수 (프론트엔드)

`web/src/env.d.ts` + `NEXT_PUBLIC_*` 접두어:

| 변수 | 기본값 | 역할 |
|------|--------|------|
| `NEXT_PUBLIC_API_URL` | `http://localhost:8080` | 백엔드 API URL |
| `NEXT_PUBLIC_API_KEY` | — | API 인증 키 |
| `NEXT_PUBLIC_API_SECRET` | — | HMAC 서명 시크릿 |

---

## 빌드 & 개발

```bash
cd web
npm install
npm run dev      # 개발 서버 (포트 3000)
npm run build    # 프로덕션 빌드 (15개 라우트 컴파일)
npm run start    # 프로덕션 서버
```
