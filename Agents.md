- 이 프로젝트를 개발하기 위한 중요한 사항들 중 영구적으로 보존해야할 부분은 이곳에 추가해라
- 구조화되고 식별이 쉽게 작성해랴한다.

## Runtime Invariants

### Commit Discipline
- 큰 변경을 한 번에 커밋하지 말고, 논리적으로 독립된 작업 단위로 나눠 소커밋한다.
- 코드 수정 중에는 단계별 검증(관련 테스트/빌드/타입체크) 통과 후 즉시 커밋한다.
- 한 커밋은 가능한 한 하나의 목적만 포함해야 하며, 무관한 파일 변경을 섞지 않는다.
- 커밋 메시지는 작업 의도와 영향 범위를 명확하게 작성한다.
- 아래 가이드라인을 기반으로 생성해라
#### Commit Guidelines
1. Git commit message structure:
	- The commit message should follow this structure:
		'''
		type: subject
		
		body(optional)
		
		footer(optional)
		'''
	- The type should be one of the following:
		- feat: A new feature
		- fix: A bug fix
		- docs: Changes to documentation
		- style: Formatting, missing semi colons, etc; no code change
		- refactor: Refactoring production code
		- test: Adding tests, refactoring test; no production code change
		- chore: Updating build tasks, package manager configs, etc; no production code change
		
	- The subject:
		- Must be no longer than 50 characters.
		- Should start with a capital letter.
		- Should not end with a period.
		- Use an imperative tone to describe what a commit does, rather than what it did. For example, use change; not changed or changes.
		
	- The body(optional):
		- Include this section only if the changes require additional explanation.
		- Explain what and why the changes were made in more detail, while the code itself explains how.
		- Ensure that each line in the body does not exceed 72 characters.
		
	- The footer(optional):
		- Only include a footer if the user provides specific information, such as issue tracker IDs.

2. Process:
    - First, summarize the key changes from given output of the **git diff --staged** command.
    - Ensure the message clearly reflects the purpose of the changes.
    - Write the commit message adhering to Git commit message structure.
    - Here's an example commit message to follow:
			style: Enhance button component design
			
			Improved button design to better address user feedback on
			visibility and consistency across different devices. The new
			design aims to create a more cohesive and accessible user
			interface.
			
			- Updated color scheme to improve contrast and ensure compliance
			  with accessibility standards.
			- Increased font size and adjusted font weight for better
			  readability on smaller screens.
			- Standardized button sizes and padding for consistency across
			  all pages.
			- Enhanced hover and active states to provide clearer visual
			  feedback to users.
			
			Resolves: #123
			See also: #456, #789
    - The commit message should be concise, clear, and follow the structure outlined above.
    - Do not include any additional explanations or comments outside of the commit message format.

### Session ID Consistency
- `submit_run`에서 생성/확정한 `session_id`는 `execute_run`까지 동일하게 전달되어야 한다.
- 새 세션 실행에서 `session_id`를 다시 생성하면 사용자 메시지가 다른 세션으로 저장되어 새로고침 시 대화가 사라지는 문제가 발생한다.

### Session Workspace Isolation
- 세션별 파일 산출물, git clone 결과, 기본 작업 디렉터리는 `data/repo/<session_id>/` 아래에서 관리되어야 한다.
- validator/coder/terminal/external repo clone 경로 결정 시 전역 공유 작업 디렉터리를 기본값으로 사용하면 안 되며, 반드시 세션 workspace 기준으로 resolve해야 한다.
- 다른 세션에서 기존 산출물을 재사용하려면 대상 세션 workspace로 명시적으로 복사해야 하며, 세션 간 공유 mutable workspace를 암묵적으로 허용하면 안 된다.

### Session CLI Working Directory
- planner/reviewer/tool-caller를 포함한 모든 CLI-backed LLM 실행은 항상 해당 `session_id`의 workspace 내부에서 시작해야 한다.
- 외부 repo를 clone/pull 한 실행에서는 clone 이후의 CLI 작업 디렉터리를 세션 root가 아니라 그 세션 내부의 분석 대상 repo 경로로 전환해야 한다.
- `std::env::current_dir()`나 서버 프로세스 루트를 세션 실행의 기본 cwd로 사용하면 안 되며, working directory는 run/session 기준으로 매 호출마다 명시적으로 전달해야 한다.

### Follow-up Context Anchoring
- 짧은 후속 발화(예: `로컬에 있어`)도 독립 질의로 처리하지 말고, 직전 사용자 메시지와 최근 run 결과 요약을 실행 컨텍스트(`History`)에 주입해야 한다.
- 세션 메모리 검색 시 후속 발화로 판단되면 검색 쿼리를 `현재 입력 + 직전 사용자 입력`으로 확장해 recall 저하를 방지한다.

### Local-First Tool Routing
- 로컬 파일/폴더/워크스페이스 의도에서는 `filesystem/*` 도구를 우선 선택하고, `github/*` 등 원격 저장소 도구는 사용자가 명시적으로 원격 작업을 요청한 경우에만 사용한다.
- planner/tool-caller 프롬프트에 로컬 우선 정책을 명시해 도구 선택 편향을 줄인다.

### Remote Repo Analysis Routing
- 원격 저장소 URL에 대한 일반적인 "분석/구조 파악/아키텍처 요약" 요청은 기본적으로 세션 workspace로 clone 또는 pull 한 뒤 로컬 기준으로 분석해야 한다.
- `github/*` 기반 원격 overview 경로는 "remote only", "clone 없이", PR/issue/commit 메타데이터처럼 원격 맥락이 명시적으로 필요한 경우에만 기본 경로로 선택한다.
- 원격 repo 분석용 skill을 둘 경우, clone/pull 단계와 이후 로컬 inspection 단계(`filesystem/*`)를 분리해 현재 진행 상황이 세션 로그와 실행 trace에 드러나도록 구성한다.

### Tool Caller Output Contract
- `tool_caller` 노드 프롬프트는 응답 형식을 엄격히 고정해야 하며, 첫 토큰이 `[` 또는 `DONE`가 되도록 요구해야 한다.
- `tool_caller`는 prose, markdown fence, wrapper object(예: `{"tool_calls":[...]}`)를 금지해야 한다.
- 파싱 실패 시 세션 로그만으로 원인을 확인할 수 있도록 raw selector 출력 preview를 오류 메시지에 포함해야 한다.

### Skill Git CLI
- 스킬/워크플로우 노드의 `git_commands`는 `validator` 역할에서만 지원한다.
- `git_commands` 각 항목은 `status --short` 같은 서브커맨드 형식과 `git status --short` 전체 명령 형식을 모두 허용하되, 런타임은 항상 `git` CLI 실행으로 정규화한다.
- 스킬 파라미터 치환은 `instructions`뿐 아니라 `git_commands`에도 동일하게 적용되어야 하며, clone/push 같은 실동작 스킬은 이 치환을 전제로 작성한다.

### CLI Model Routing
- `MODEL_CLI_BACKEND`가 설정되면 coder 전용 경로만이 아니라 planner/reviewer/tool-caller 등 일반 LLM 노드도 동일한 CLI provider를 통해 실행되어야 한다.
- `MODEL_CLI_ONLY=true`일 때는 부팅 시 대상 CLI provider만 활성화하고 나머지 provider는 비활성화해 API/OAuth 우회 목적이 fallback 경로에서 깨지지 않도록 보장해야 한다.

### CLI Feature Parity
- 웹/API에서 가능한 핵심 기능은 CLI/TUI 경로에서도 동일하게 수행 가능해야 하며, UI 전용 플로우에 의존하면 안 된다.
- 새 기능을 추가할 때는 로컬 CLI 기반 실행 경로(오케스트레이션, 서브에이전트, 후속 실행, 검증)가 함께 동작하는지 먼저 검토해야 한다.

### Cross-Platform Web Scripts
- `web/package.json`의 스크립트는 `${VAR:-default}` 같은 POSIX 셸 치환에 의존하면 안 된다.
- 웹 포트는 `AGENT_WEB_PORT`를 읽는 Node 래퍼 등 크로스플랫폼 방식으로 해석한 뒤 `next dev/start`에 전달해야 한다.

### CLI Command Resolution
- `codex`/`claude` 같은 bare CLI 명령은 spawn 전에 절대경로로 해석해야 하며, PATH에만 의존하면 안 된다.
- 특히 Codex는 VS Code 확장 번들 경로(`.vscode/extensions/openai.chatgpt-*/bin/.../codex`)도 fallback 후보로 탐색해야 한다.

### Workflow Continuation
- 워크플로우/그래프 실행은 첫 번째 패스가 끝났다고 바로 성공으로 끝내지 말고, reviewer가 `INCOMPLETE`를 반환하면 남은 요구사항을 기준으로 후속 planner/sub-agent 그래프를 재구성해 이어서 실행해야 한다.
- 후속 planner는 남은 작업을 독립 서브태스크로 분해하고, 병렬 가능한 항목은 별도 노드로 분리해 실행되도록 JSON `SubtaskPlan`을 우선 출력해야 한다.
- reviewer가 실패하거나 응답이 모호한 경우도 자동 성공으로 간주하면 안 되며, 미검증 상태로 처리해 후속 실행 또는 실패로 이어져야 한다.

### Streaming Event Ordering
- `node_token_chunk` 이벤트는 저장 순서가 응답 텍스트 순서와 동일해야 한다.
- 토큰마다 `tokio::spawn`으로 DB insert를 분기하면 순서가 뒤섞일 수 있으므로 직렬 큐(worker)로 저장한다.

### Session Log Integrity
- 세션 JSONL append는 세션 단위 직렬화(락/큐)로 처리해 이벤트 라인 경계가 깨지지 않도록 보장해야 한다.
- replay 시 단일 라인에 JSON 값이 연속으로 붙은 데이터(legacy corruption)가 있어도 역직렬화가 가능해야 한다.

### UTF-8 Safe SSE Parsing
- 모델 스트림 파싱은 바이트 버퍼 기반으로 처리해 UTF-8 경계를 보존해야 한다.
- 청크 단위 `from_utf8_lossy` 누적은 한글/다국어에서 `�` 문자를 유발할 수 있다.

### Memory Model
- 세션 메모리는 `memory_items` 테이블을 사용하며 `session_id`로 격리된다.
- 전역(공통) 메모리는 `knowledge_base` 테이블을 사용하며 모든 세션에서 공유된다.
- 실행 컨텍스트 구성 시 세션 메모리(`retrieve`)와 전역 메모리(`search_knowledge`)를 함께 주입해야 한다.
- 메모리 관리 API 경로:
  - 세션 메모리 조회/추가: `/v1/memory/sessions/:session_id/items`
  - 세션 메모리 수정: `/v1/memory/items/:memory_id`
  - 전역 메모리 조회/추가: `/v1/memory/global/items`
  - 전역 메모리 수정: `/v1/memory/global/items/:knowledge_id`
