# Skill & Workflow System

## 관련 파일

- `src/orchestrator/skill_loader.rs` — 스킬 등록 & 로드
- `src/orchestrator/mod.rs` — AutoSkillRoute, 자동 라우팅
- `skills/` — 내장 스킬 YAML 파일 (6개)
- `data/skills/` — 커스텀 스킬 디렉토리

---

## 내장 스킬 목록 (`skills/`)

| 파일 | 스킬명 | 용도 |
|------|--------|------|
| `git_clone_repo.yaml` | Git Clone | 원격 레포 클론 |
| `git_commit_push.yaml` | Git Commit & Push | 변경사항 커밋 & 푸시 |
| `git_recent_history.yaml` | Git Recent History | 최근 커밋 히스토리 조회 |
| `git_worktree_summary.yaml` | Git Worktree Summary | worktree 현황 요약 |
| `github_repo_overview.yaml` | GitHub Repo Overview | GitHub 레포 구조 분석 |
| `local_repo_overview.yaml` | Local Repo Overview | 로컬 레포 구조 분석 |

---

## 스킬 YAML 구조

```yaml
name: github_repo_overview
description: "GitHub 레포지토리 구조와 주요 파일을 분석합니다"
keywords:             # 자동 라우팅 매칭 키워드
  - "overview"
  - "analyze repo"
  - "repository structure"
trigger_patterns:     # 정규식 패턴 (선택적)
  - ".*github.*overview.*"
nodes:                # DAG 노드 정의
  - id: clone
    role: Coder
    task: "레포를 클론하고 기본 구조를 파악합니다"
    instructions: "..."
    dependencies: []
  - id: analyze
    role: Analyzer
    task: "레포 구조를 분석하고 주요 컴포넌트를 정리합니다"
    dependencies: [clone]
  - id: summarize
    role: Summarizer
    task: "분석 결과를 정리합니다"
    dependencies: [analyze]
```

---

## AutoSkillRoute 자동 라우팅

**파일**: `src/orchestrator/mod.rs`

`submit_run()` 시 자동 스킬 매칭:

```
AutoSkillRoute {
  skill_name: String,
  keywords: Vec<String>,
  trigger_patterns: Vec<Regex>,
}

1. 태스크 텍스트를 등록된 스킬의 키워드/패턴과 비교
2. 매칭 스킬 발견 → 해당 스킬의 DAG로 실행
3. 매칭 없음 → 일반 태스크 분류 흐름
```

---

## 커스텀 스킬 등록

### 방법 1: YAML 파일 직접 추가

`data/skills/` 디렉토리에 YAML 파일 작성:

```bash
# 예시: 나만의 분석 스킬
cat > data/skills/my_analysis.yaml << EOF
name: my_custom_analysis
description: "커스텀 분석 워크플로우"
keywords:
  - "내 분석"
  - "custom analyze"
nodes:
  - id: plan
    role: Planner
    task: "분석 계획 수립"
    dependencies: []
  - id: analyze
    role: Analyzer
    task: "데이터 분석"
    dependencies: [plan]
  - id: summarize
    role: Summarizer
    task: "결과 요약"
    dependencies: [analyze]
EOF
```

`SKILLS_DIR=skills` 환경 변수로 내장 스킬 디렉토리 변경 가능.

### 방법 2: API로 워크플로우 등록

```bash
POST /v1/workflows
{
  "name": "my_workflow",
  "description": "...",
  "nodes": [...]
}
```

웹 UI `/workflows` 페이지에서도 관리 가능.

---

## 스케줄 실행 (`src/scheduler.rs`)

등록된 스킬/워크플로우를 Cron 표현식으로 자동 실행:

```
CronScheduler {
  tick_interval: 60s,
  schedules: Vec<ScheduleEntry>
}

매 60초 틱:
  활성화된 스케줄 확인 → 실행 시간 도달 시 orchestrator.submit_run() 호출
```

웹 UI `/schedules` 페이지 또는 REST API (`POST /v1/schedules`)로 스케줄 등록.
