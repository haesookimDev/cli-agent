# Task Classification & DAG Building

## 관련 파일

- `src/types.rs` — `TaskType` enum 정의
- `src/orchestrator/mod.rs` — 분류 로직 (lines 1982-2101), DAG 빌딩 (lines 2187-2415)

---

## TaskType 9가지

```rust
pub enum TaskType {
    SimpleQuery,       // 단순 Q&A, 설명 요청
    Analysis,          // 데이터 분석, 패턴 추출
    CodeGeneration,    // 코드 작성, 수정, 디버깅
    Configuration,     // 시스템 설정 변경
    ConfigQuery,       // 설정 조회 (읽기 전용)
    ToolOperation,     // MCP 툴 호출 필요
    ExternalProject,   // repo_url 포함, 외부 레포 작업
    Interactive,       // 멀티스텝 탐색, 반복 확인
    Complex,           // 여러 카테고리 복합
}
```

---

## 분류 흐름

```
Planner 에이전트 → LLM 호출:
  "다음 태스크를 분류하세요: [task]
   가능한 타입: SimpleQuery|Analysis|CodeGeneration|..."

LLM 응답 파싱 실패 또는 LLM 미설정 시 → 키워드 폴백:

키워드 폴백 규칙 (src/orchestrator/mod.rs lines 2090-2101):
  "code" | "implement" | "write" | "fix" | "debug"  → CodeGeneration
  "analyze" | "extract" | "pattern" | "summarize"   → Analysis
  "config" | "set" | "update" | "configure"          → Configuration
  "tool" | "call" | "invoke" | "mcp"                 → ToolOperation
  repo_url 있음                                       → ExternalProject
  기타                                                → SimpleQuery
```

---

## 태스크 타입별 DAG 구성

### SimpleQuery (2 노드)
```
plan (Planner)
  └── summarize (Summarizer)
```
빠른 응답이 목적. LLM 직접 답변을 요약.

### Analysis (4 노드)
```
plan (Planner)
  └── extract (Extractor)
        └── analyze (Analyzer)
              └── summarize (Summarizer)
```
데이터 추출 후 패턴 분석, 인사이트 요약.

### ConfigQuery (2 노드)
```
plan (Planner) [시스템 상태 컨텍스트 포함]
  └── summarize (Summarizer)
```
현재 설정값 조회. 시스템 스냅샷을 컨텍스트로 주입.

### Configuration (3 노드)
```
plan (Planner)
  └── config_manage (ConfigManager)
        └── summarize (Summarizer)
```
설정 변경 작업. ConfigManager가 실제 변경 수행.

### ToolOperation (3 노드)
```
plan (Planner)
  └── tool_call (ToolCaller)
        └── summarize (Summarizer)
```
MCP 툴 직접 호출. ToolCaller가 McpRegistry를 통해 실행.

### CodeGeneration (최대 7 노드)
```
plan (Planner)
  └── extract (Extractor)         [코드베이스 관련 정보 추출]
        └── context_probe (ToolCaller) [MCP 툴로 추가 컨텍스트]
              └── code (Coder)         [코드 생성/수정]
                    ├── review (Reviewer) [선택적, 코드 리뷰]
                    └── fallback (Fallback) [실패 시 대안 시도]
                          └── summarize (Summarizer)
```

### ExternalProject
CodeGeneration과 유사하나 첫 단계에서 레포 클론 & 분석 수행.

### Interactive
```
plan (Planner) → [동적 서브태스크 노드]
```
Planner가 런타임에 사용자 의도를 파악하여 필요한 노드를 동적 생성.

### Complex
```
plan (Planner) → [혼합 노드 그래프]
```
여러 타입이 혼합된 경우 Planner가 조합 결정.

---

## AgentNode 구조

```rust
AgentNode {
  id: String,               // "plan", "extract", "code", ...
  role: AgentRole,          // Planner, Extractor, Coder, ...
  task: String,             // 이 노드의 구체적 태스크
  instructions: String,     // 추가 지시사항
  dependencies: Vec<String>, // 선행 노드 id 목록
  policy: ExecutionPolicy {
    timeout_ms: u64,
    on_failure: FailureAction, // Retry | Skip | Fail
    allow_parallel: bool,
  }
}
```
