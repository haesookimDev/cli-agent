# CLI Usage

## 기본 구문

```bash
cargo run -- <subcommand> [options]
```

또는 빌드 후:

```bash
./target/debug/cli-agent <subcommand>
```

---

## Subcommands

### `serve` — REST API 서버 실행

```bash
cargo run -- serve
```

백엔드 서버를 `AGENT_SERVER_HOST:AGENT_SERVER_PORT` (기본 `0.0.0.0:8080`)에서 실행.
웹 대시보드 임베디드 HTML도 같은 포트에서 서빙.

---

### `run` — 단일 태스크 실행

```bash
cargo run -- run "<task>"
cargo run -- run "<task>" --profile coding
cargo run -- run "<task>" --session <session-id>
```

옵션:
- `--profile <profile>`: `general` | `planning` | `extraction` | `coding` (기본: `general`)
- `--session <id>`: 기존 세션에 연결
- `--repo <url>`: 외부 레포 URL

예시:
```bash
cargo run -- run "이 프로젝트의 구조를 설명해줘"
cargo run -- run "src/main.rs 파일을 읽어서 진입점을 설명해줘" --profile analysis
```

---

### `tui` — 터미널 UI 모드

```bash
cargo run -- tui
```

Ratatui 기반 인터랙티브 TUI 실행.

TUI 키 바인딩:
- `Enter`: 메시지 전송
- `Tab`: 패널 전환
- `Esc`: 입력 취소
- `q` / `Ctrl+C`: 종료
- `↑↓`: 히스토리 탐색

TUI 설정은 `data/tui-settings.json`에 저장.

---

### `replay` — 세션 재현

```bash
cargo run -- replay <session-id>
```

`data/sessions/<session-id>/` 의 JSONL 이벤트 로그를 재생.
디버깅 및 이전 실행 분석용.

---

### `memory` — 메모리 관리

```bash
# 세션 메모리 조회
cargo run -- memory list --session <session-id>

# 글로벌 지식 조회
cargo run -- memory list --global

# 메모리 항목 삭제
cargo run -- memory delete --session <session-id> --key <key>

# 글로벌 지식 삭제
cargo run -- memory delete --global --key <key>
```

---

## 환경 변수로 동작 제어

```bash
# Claude Code CLI만 사용 (API 없이)
MODEL_CLI_BACKEND=claude_code MODEL_CLI_COMMAND=claude MODEL_CLI_ONLY=true \
  cargo run -- serve

# 특정 포트로 서버 실행
AGENT_SERVER_PORT=9090 cargo run -- serve

# 디버그 로그 활성화
RUST_LOG=debug cargo run -- serve
```

---

## 개발 스크립트

```bash
# 백엔드 + 프론트엔드 동시 시작
./scripts/dev.sh        # macOS/Linux
./scripts/dev.bat       # Windows
./scripts/dev.ps1       # PowerShell

# 프로덕션 빌드
./scripts/build.sh

# 프로덕션 시작
./scripts/start.sh
```
