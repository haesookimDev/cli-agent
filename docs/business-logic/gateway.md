# Gateway: Slack & Discord

## 관련 파일

- `src/gateway/mod.rs` — GatewayManager, GatewayAdapter 트레이트
- `src/gateway/slack.rs` — Slack 봇 어댑터
- `src/gateway/discord.rs` — Discord 봇 어댑터

---

## GatewayManager 구조

```rust
GatewayManager {
  orchestrator: Arc<Orchestrator>,
  channel_sessions: DashMap<(Platform, ChannelId), SessionId>,
  adapters: Vec<Box<dyn GatewayAdapter>>,
}
```

`channel_sessions`: 플랫폼 채널을 오케스트레이터 세션에 매핑 (채널별 대화 컨텍스트 유지).

---

## GatewayAdapter 트레이트

```rust
#[async_trait]
pub trait GatewayAdapter: Send + Sync {
    async fn start(&self, manager: Arc<GatewayManager>) -> Result<()>;
    async fn send_message(&self, channel_id: &str, text: &str) -> Result<()>;
    fn platform(&self) -> Platform;
}
```

---

## GatewayAction 타입

플랫폼 커맨드 → 오케스트레이터 액션 매핑:

```rust
pub enum GatewayAction {
    SubmitRun { task: String, profile: TaskProfile },
    CancelRun { run_id: Uuid },
    PauseRun { run_id: Uuid },
    ResumeRun { run_id: Uuid },
    GetRun { run_id: Uuid },
    ListRuns,
    GetMemory,
    Help,
}
```

---

## Slack 어댑터 흐름

**파일**: `src/gateway/slack.rs`

```
1. Slack 앱 설정:
   - Bot Token: SLACK_BOT_TOKEN=xoxb-...
   - Signing Secret: SLACK_SIGNING_SECRET=...

2. 웹훅 수신:
   POST /gateway/slack/events
     → Slack 서명 검증 (X-Slack-Signature 헤더)
     → event_type 파싱:
       - "app_mention" → 봇 멘션 처리
       - "message" → DM 메시지 처리

3. 메시지 처리:
   텍스트 파싱 → GatewayAction 결정
   → GatewayManager.handle_action(channel_id, action)
   → orchestrator.submit_run(task)
   → 즉시 "처리 중..." 응답 전송

4. 완료 알림:
   웹훅 이벤트 (run.completed)
   → SlackAdapter.send_message(channel_id, result_text)
```

---

## Discord 어댑터 흐름

**파일**: `src/gateway/discord.rs`

```
1. Discord 앱 설정:
   - Bot Token: DISCORD_BOT_TOKEN=...
   - Application ID: DISCORD_APPLICATION_ID=...
   - Public Key: DISCORD_PUBLIC_KEY=... (ed25519)

2. 웹훅 수신:
   POST /gateway/discord/interactions
     → Ed25519 서명 검증 (X-Signature-Ed25519 + X-Signature-Timestamp)
     → interaction_type 파싱:
       - PING → PONG 응답
       - APPLICATION_COMMAND → 슬래시 커맨드 처리
       - MESSAGE_COMPONENT → 버튼/선택 메뉴 처리

3. 슬래시 커맨드:
   /run <task>  → SubmitRun
   /cancel <id> → CancelRun
   /status <id> → GetRun
   /list        → ListRuns

4. 완료 알림:
   Discord Webhook으로 결과 전송
```

---

## 채널→세션 매핑

```
GatewayManager.get_or_create_session(platform, channel_id)
  ↓
  channel_sessions.get(&(platform, channel_id))
  → 있으면: 기존 session_id 반환 (대화 컨텍스트 유지)
  → 없으면: orchestrator.create_session() → 신규 session_id 저장
```

채널별로 독립적인 오케스트레이터 세션 유지 → 동일 채널에서 이전 대화 컨텍스트 참조 가능.

---

## 활성화 설정

```bash
# .env

# Slack (둘 다 설정 시 활성화)
SLACK_BOT_TOKEN=xoxb-your-slack-bot-token
SLACK_SIGNING_SECRET=your-slack-signing-secret

# Discord (셋 다 설정 시 활성화)
DISCORD_BOT_TOKEN=your-discord-bot-token
DISCORD_APPLICATION_ID=123456789
DISCORD_PUBLIC_KEY=hex-encoded-ed25519-public-key
```

해당 환경 변수가 없으면 해당 어댑터 비활성화 (오류 없이 스킵).
