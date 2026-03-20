# Environment Variables Reference

`.env.example` 기반 전체 환경 변수 레퍼런스.

---

## 서버 설정

| 변수 | 기본값 | 필수 | 설명 |
|------|--------|------|------|
| `AGENT_DATA_DIR` | `data` | - | 데이터 저장 루트 디렉토리 |
| `AGENT_DATABASE_URL` | `sqlite://data/agent.db` | - | SQLite DB 경로 |
| `AGENT_API_KEY` | `local-dev-key` | 필수 | API 인증 키 |
| `AGENT_API_SECRET` | `local-dev-secret` | 필수 | HMAC 서명 시크릿 |
| `AGENT_SERVER_HOST` | `0.0.0.0` | - | 백엔드 바인딩 호스트 |
| `AGENT_SERVER_PORT` | `8080` | - | 백엔드 포트 |
| `AGENT_WEB_PORT` | `3000` | - | 프론트엔드 포트 |
| `AGENT_MAX_PARALLELISM` | `8` | - | 최대 동시 에이전트 수 |
| `AGENT_MAX_GRAPH_DEPTH` | `6` | - | DAG 최대 깊이 |
| `AGENT_MAX_CONTEXT_TOKENS` | `16000` | - | 에이전트당 최대 컨텍스트 토큰 |
| `SKILLS_DIR` | `skills` | - | 내장 스킬 YAML 디렉토리 |
| `AGENT_WEBHOOK_TIMEOUT_SECS` | `5` | - | 웹훅 요청 타임아웃 (초) |

---

## AI 모델 API

| 변수 | 기본값 | 필수 | 설명 |
|------|--------|------|------|
| `ANTHROPIC_API_KEY` | — | 선택 | Claude 모델 사용 시 필수 |
| `OPENAI_API_KEY` | — | 선택 | GPT 모델 사용 시 필수 |
| `GEMINI_API_KEY` | — | 선택 | Gemini 모델 사용 시 필수 |
| `VLLM_BASE_URL` | `http://127.0.0.1:8000` | 선택 | 로컬 vLLM 서버 URL |

최소 하나 이상의 AI API 키 설정 필요 (또는 CLI 모델 설정).

---

## CLI 모델 백엔드 (선택)

모든 LLM 트래픽을 로컬 CLI를 통해 라우팅:

| 변수 | 예시값 | 설명 |
|------|--------|------|
| `MODEL_CLI_BACKEND` | `claude_code` | CLI 백엔드 종류 (`claude_code` \| `codex`) |
| `MODEL_CLI_COMMAND` | `claude` | CLI 실행 명령어 |
| `MODEL_CLI_ARGS` | `--dangerously-skip-permissions` | CLI 추가 인자 |
| `MODEL_CLI_TIMEOUT_MS` | `300000` | CLI 타임아웃 (ms) |
| `MODEL_CLI_ONLY` | `true` | CLI 전용 모드 (API 미사용) |

---

## 코더 전용 CLI 백엔드 (선택)

코더 에이전트만 별도 CLI 사용 (나머지는 일반 API 라우팅):

| 변수 | 예시값 | 설명 |
|------|--------|------|
| `CODER_BACKEND` | `codex` | 코더 백엔드 (`llm` \| `claude_code` \| `codex`) |
| `CODER_COMMAND` | `codex` | 코더 CLI 명령어 |
| `CODER_ARGS` | `--model o3` | 코더 CLI 인자 |
| `CODER_TIMEOUT_MS` | `300000` | 코더 타임아웃 (ms) |
| `CODER_WORKING_DIR` | — | 코더 기본 작업 디렉토리 |

---

## Slack 게이트웨이 (선택)

두 변수 모두 설정 시 Slack 어댑터 활성화:

| 변수 | 설명 |
|------|------|
| `SLACK_BOT_TOKEN` | Slack 봇 토큰 (`xoxb-...`) |
| `SLACK_SIGNING_SECRET` | Slack 앱 서명 시크릿 |

---

## Discord 게이트웨이 (선택)

세 변수 모두 설정 시 Discord 어댑터 활성화:

| 변수 | 설명 |
|------|------|
| `DISCORD_BOT_TOKEN` | Discord 봇 토큰 |
| `DISCORD_APPLICATION_ID` | Discord 앱 ID (숫자) |
| `DISCORD_PUBLIC_KEY` | ed25519 공개키 (hex 인코딩) |

---

## MCP 설정 (선택)

| 변수 | 기본값 | 설명 |
|------|--------|------|
| `MCP_ENABLED` | `false` | MCP 활성화 여부 |
| `MCP_CONFIG_PATH` | `mcp_servers.json` | MCP 서버 설정 파일 경로 |

---

## MCP 서버 자격증명 (선택)

`mcp_servers.json`에서 `${VAR}` 형식으로 참조:

| 변수 | 설명 |
|------|------|
| `GITHUB_PERSONAL_ACCESS_TOKEN` | GitHub PAT (github MCP 서버용) |
| `TAVILY_API_KEY` | Tavily 검색 API 키 |
| `AGENT_WORKSPACE_DIR` | MCP filesystem 서버가 접근할 프로젝트 경로 |
| `AGENT_EXTRA_DIR` | MCP filesystem 서버 추가 디렉토리 |
