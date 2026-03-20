# Getting Started

## 전제 조건

| 도구 | 최소 버전 |
|------|----------|
| Rust | 1.80+ (Edition 2024 지원) |
| Node.js | 18+ |
| npm | 9+ |
| SQLite | 3.x (sqlx가 자동 번들) |

---

## 1. 저장소 클론 & 빌드

```bash
git clone <repo-url>
cd cli-agent

# Rust 백엔드 빌드
cargo build

# Node.js 의존성 설치
cd web && npm install && cd ..
```

---

## 2. 환경 변수 설정

```bash
cp .env.example .env
```

최소 필수 설정:

```bash
# .env
AGENT_API_KEY=local-dev-key
AGENT_API_SECRET=local-dev-secret

# AI 모델 (최소 하나 이상 설정)
ANTHROPIC_API_KEY=sk-ant-...
# 또는
OPENAI_API_KEY=sk-...
```

전체 환경 변수: [environment.md](environment.md) 참조

---

## 3. 데이터 디렉토리 초기화

```bash
mkdir -p data/sessions data/repos data/skills
```

SQLite DB는 첫 실행 시 자동 생성됨 (`data/agent.db`).

---

## 4. 서버 실행

### 개발 환경 (백엔드 + 프론트엔드 동시)

```bash
# macOS/Linux
./scripts/dev.sh

# Windows
./scripts/dev.bat
```

### 수동 실행

```bash
# 터미널 1: 백엔드 (포트 8080)
cargo run -- serve

# 터미널 2: 프론트엔드 (포트 3000)
cd web && npm run dev
```

---

## 5. 첫 실행 확인

브라우저에서 `http://localhost:3000` 접속:

- **Runner** 페이지: 태스크 직접 입력 & 실행
- **Chat** 페이지: 대화형 인터페이스
- **Sessions** 페이지: 실행 세션 목록

### CLI로 테스트

```bash
# 단순 질문 실행
cargo run -- run "2 + 2는 얼마인가요?"

# TUI 모드
cargo run -- tui
```

---

## 6. 빌드 검증

```bash
cargo build          # 컴파일 오류 없음
cargo test           # 14개 테스트 통과
cd web && npm run build   # 15개 라우트 빌드
```

---

## 주요 접속 포인트

| 포인트 | URL |
|--------|-----|
| 웹 대시보드 | `http://localhost:3000` |
| REST API | `http://localhost:8080/v1/` |
| API 문서 (임베디드) | `http://localhost:8080/` |
