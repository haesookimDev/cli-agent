# MCP Integration

## 관련 파일

- `src/mcp/mod.rs` — McpRegistry
- `mcp_servers.json` — MCP 서버 설정

---

## McpRegistry 구조

```rust
McpRegistry {
  servers: HashMap<String, McpServerHandle>,
  tools: HashMap<String, McpTool>,  // 전체 툴 목록 (서버명:툴명)
}

McpServerHandle {
  name: String,
  process: Child,        // stdio 서버 프로세스
  stdin: PipeWriter,
  stdout: BufReader,
  capabilities: Vec<McpTool>,
}

McpTool {
  name: String,
  description: String,
  input_schema: serde_json::Value,  // JSON Schema
  server_name: String,
}
```

---

## mcp_servers.json 설정 포맷

```json
{
  "servers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "${AGENT_WORKSPACE_DIR}"],
      "env": {}
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "${GITHUB_PERSONAL_ACCESS_TOKEN}"
      }
    },
    "tavily": {
      "command": "npx",
      "args": ["-y", "tavily-mcp"],
      "env": {
        "TAVILY_API_KEY": "${TAVILY_API_KEY}"
      }
    }
  }
}
```

`${VAR}` 형식으로 환경 변수 참조. `McpRegistry` 초기화 시 치환됨.

---

## 툴 호출 흐름 (JSON-RPC over stdio)

```
ToolCaller 에이전트 → McpRegistry.call_tool(tool_name, arguments)
  ↓
1. tool_name으로 서버 조회 (예: "filesystem:read_file")
2. JSON-RPC 요청 구성:
   {
     "jsonrpc": "2.0",
     "id": <uuid>,
     "method": "tools/call",
     "params": {
       "name": "read_file",
       "arguments": { "path": "/workspace/src/main.rs" }
     }
   }
3. 해당 서버의 stdin에 JSON 라인 전송
4. stdout에서 JSON-RPC 응답 수신:
   {
     "jsonrpc": "2.0",
     "id": <uuid>,
     "result": {
       "content": [{ "type": "text", "text": "fn main() {...}" }]
     }
   }
5. content 추출 → ToolCaller 에이전트 출력으로 반환
```

---

## MCP 툴이 에이전트에 주입되는 시점

```
Orchestrator.build_graph() 시:
  McpRegistry.list_tools() → 사용 가능한 툴 목록
  ↓
  Planner 노드 instructions에 툴 목록 주입:
  "사용 가능한 MCP 툴:\n- filesystem:read_file\n- filesystem:write_file\n- github:..."
  ↓
  Planner가 필요한 툴을 판단 → tool_call 노드 생성
  ↓
  ToolCaller 노드 instructions에 구체적 툴명 & 파라미터 지시
```

`context_probe` 노드 (CodeGeneration DAG):
- MCP 툴로 코드베이스 탐색 (파일 읽기, 구조 파악)
- 결과를 Coder 노드의 컨텍스트로 전달

---

## Local-First 정책

툴 선택 시 로컬 파일시스템 툴 우선:

```
priority order:
  1. filesystem/* (로컬 파일 읽기/쓰기)
  2. git/* (로컬 git 작업)
  3. github/* (외부 GitHub API)
  4. tavily/* (외부 검색)
  5. 기타 외부 서비스
```

---

## MCP 활성화 설정

```bash
# .env
MCP_ENABLED=true
MCP_CONFIG_PATH=mcp_servers.json

# MCP 서버 자격증명
GITHUB_PERSONAL_ACCESS_TOKEN=ghp_...
TAVILY_API_KEY=tvly-...
AGENT_WORKSPACE_DIR=/path/to/project
```

`MCP_ENABLED=false` 시 McpRegistry는 빈 툴 목록 반환 (에이전트에 MCP 툴 미주입).

---

## 웹 UI에서 툴 확인

`/tools` 페이지에서 등록된 MCP 툴 전체 목록 및 상태 확인 가능.
