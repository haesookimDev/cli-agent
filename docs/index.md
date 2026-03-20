# cli-agent Documentation Index

빠른 탐색 가이드 — 필요한 정보에 맞는 파일만 로드하세요.

## 어떤 정보가 필요한가?

| 상황 | 참조 파일 |
|------|----------|
| 시스템 전체 구조 파악 | [architecture/overview.md](architecture/overview.md) |
| Rust 백엔드 모듈 상세 | [architecture/backend.md](architecture/backend.md) |
| Next.js 프론트엔드 구조 | [architecture/frontend.md](architecture/frontend.md) |
| DAG 실행 엔진 이해 | [architecture/dag-execution.md](architecture/dag-execution.md) |
| 메모리/컨텍스트 시스템 | [architecture/memory-context.md](architecture/memory-context.md) |
| 채팅 입력→처리→출력 흐름 | [business-logic/chat-flow.md](business-logic/chat-flow.md) |
| 태스크 분류 & DAG 빌딩 | [business-logic/task-classification.md](business-logic/task-classification.md) |
| 에이전트 실행 & 모델 라우팅 | [business-logic/agent-model-routing.md](business-logic/agent-model-routing.md) |
| 스킬/워크플로우 시스템 | [business-logic/skill-workflow.md](business-logic/skill-workflow.md) |
| 메모리 저장/조회 로직 | [business-logic/memory-system.md](business-logic/memory-system.md) |
| MCP 서버 통합 | [business-logic/mcp-integration.md](business-logic/mcp-integration.md) |
| Slack/Discord 게이트웨이 | [business-logic/gateway.md](business-logic/gateway.md) |
| 처음 설치 & 실행 | [guides/getting-started.md](guides/getting-started.md) |
| 환경 변수 레퍼런스 | [guides/environment.md](guides/environment.md) |
| REST API 엔드포인트 | [guides/api-reference.md](guides/api-reference.md) |
| CLI 명령어 사용법 | [guides/cli-usage.md](guides/cli-usage.md) |
| 기능 확장 방법 | [guides/extending.md](guides/extending.md) |
| 개발 완료 항목 | [development/status.md](development/status.md) |
| TODO / 미완성 기능 | [development/todo.md](development/todo.md) |

---

## 폴더 구조

```
docs/
├── index.md                        ← 이 파일 (전체 목차)
├── architecture/
│   ├── overview.md                 ← 시스템 개요, 기술 스택, 모듈 맵
│   ├── backend.md                  ← Rust 백엔드 모듈 상세
│   ├── frontend.md                 ← Next.js 프론트엔드 구조
│   ├── dag-execution.md            ← DAG 실행 엔진
│   └── memory-context.md           ← 메모리 & 컨텍스트 시스템
├── business-logic/
│   ├── chat-flow.md                ← 채팅 11단계 완전 흐름 (핵심)
│   ├── task-classification.md      ← 9가지 태스크 타입 & DAG 빌딩
│   ├── agent-model-routing.md      ← 에이전트 실행 & 모델 라우팅
│   ├── skill-workflow.md           ← 스킬 YAML 시스템
│   ├── memory-system.md            ← 메모리 저장/검색 로직
│   ├── mcp-integration.md          ← MCP 서버 통합
│   └── gateway.md                  ← Slack/Discord 게이트웨이
├── guides/
│   ├── getting-started.md          ← 설치 및 첫 실행
│   ├── environment.md              ← 환경 변수 전체 레퍼런스
│   ├── api-reference.md            ← REST API 엔드포인트
│   ├── cli-usage.md                ← CLI 서브커맨드
│   └── extending.md                ← 기능 확장 가이드
└── development/
    ├── status.md                   ← 개발 완료 항목
    └── todo.md                     ← TODO 목록
```
