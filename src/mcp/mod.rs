use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, warn, error};

use crate::types::{McpToolCallResult, McpToolDefinition};

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Option<u64>,
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
}

pub struct McpClient {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    stdout: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
    _child: Arc<Mutex<Child>>,
    next_id: AtomicU64,
    tools: Arc<RwLock<Vec<McpToolDefinition>>>,
}

impl McpClient {
    pub async fn spawn(
        command: &str,
        args: &[&str],
        env: HashMap<String, String>,
    ) -> anyhow::Result<Self> {
        let mut cmd = if cfg!(target_os = "windows") {
            let full_args = format!(
                "{} {}",
                command,
                args.iter()
                    .map(|a| if a.contains(' ') { format!("\"{}\"", a) } else { a.to_string() })
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let mut c = Command::new("cmd.exe");
            c.args(["/C", &full_args]);
            c
        } else {
            let mut c = Command::new(command);
            c.args(args);
            c
        };

        // Inherit parent environment then overlay config-specific vars
        cmd.envs(std::env::vars());
        cmd.envs(env);

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Drain stderr in background for diagnostics
        if let Some(stderr) = child.stderr.take() {
            let mut reader = BufReader::new(stderr);
            tokio::spawn(async move {
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                debug!("MCP stderr: {}", trimmed);
                            }
                        }
                        Err(e) => {
                            error!("MCP stderr read error: {e}");
                            break;
                        }
                    }
                }
            });
        }

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture MCP server stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture MCP server stdout"))?;

        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            _child: Arc::new(Mutex::new(child)),
            next_id: AtomicU64::new(1),
            tools: Arc::new(RwLock::new(Vec::new())),
        })
    }

    async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        let mut line = serde_json::to_string(&request)?;
        line.push('\n');

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }

        let mut buf = String::new();
        {
            let mut stdout = self.stdout.lock().await;
            loop {
                buf.clear();
                let n = stdout.read_line(&mut buf).await?;
                if n == 0 {
                    return Err(anyhow::anyhow!("MCP server closed stdout"));
                }
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    continue;
                }
                break;
            }
        }

        let response: JsonRpcResponse = serde_json::from_str(buf.trim())?;
        if let Some(err) = response.error {
            return Err(anyhow::anyhow!("MCP error: {}", err));
        }
        response
            .result
            .ok_or_else(|| anyhow::anyhow!("MCP response missing result"))
    }

    pub async fn initialize(&self) -> anyhow::Result<serde_json::Value> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "cli-agent",
                "version": "0.1.0"
            }
        });
        let result = self.send_request("initialize", Some(params)).await?;
        debug!("MCP initialized: {}", result);

        // Send initialized notification (no id, no response expected)
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let mut line = serde_json::to_string(&notification)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;

        Ok(result)
    }

    pub async fn discover_tools(&self) -> anyhow::Result<Vec<McpToolDefinition>> {
        let result = self.send_request("tools/list", None).await?;

        let tools_value = result
            .get("tools")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));

        let raw_tools: Vec<serde_json::Value> =
            serde_json::from_value(tools_value).unwrap_or_default();

        let tools: Vec<McpToolDefinition> = raw_tools
            .into_iter()
            .map(|t| McpToolDefinition {
                name: t
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: t
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                input_schema: t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or(serde_json::json!({})),
                server_name: None,
            })
            .collect();

        debug!("MCP discovered {} tools", tools.len());
        *self.tools.write().await = tools.clone();
        Ok(tools)
    }

    pub async fn list_tools(&self) -> Vec<McpToolDefinition> {
        self.tools.read().await.clone()
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<McpToolCallResult> {
        let started = Instant::now();
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });

        match self.send_request("tools/call", Some(params)).await {
            Ok(result) => {
                let content_arr = result
                    .get("content")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                let content = content_arr
                    .iter()
                    .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");

                let is_error = result
                    .get("isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                Ok(McpToolCallResult {
                    tool_name: name.to_string(),
                    succeeded: !is_error,
                    content: content.clone(),
                    error: if is_error { Some(content) } else { None },
                    duration_ms: started.elapsed().as_millis(),
                })
            }
            Err(err) => {
                warn!("MCP tool call failed: {name}: {err}");
                Ok(McpToolCallResult {
                    tool_name: name.to_string(),
                    succeeded: false,
                    content: String::new(),
                    error: Some(err.to_string()),
                    duration_ms: started.elapsed().as_millis(),
                })
            }
        }
    }

    pub async fn shutdown(&self) {
        let mut child = self._child.lock().await;
        let _ = child.kill().await;
        debug!("MCP server process terminated");
    }
}

// ---------------------------------------------------------------------------
// McpRegistry — routes tool calls across multiple McpClient instances
// ---------------------------------------------------------------------------

pub struct McpRegistry {
    clients: HashMap<String, Arc<McpClient>>,
    /// Maps tool name → server name. Both "server/tool" and bare "tool" forms.
    tool_index: RwLock<HashMap<String, String>>,
    /// Server-level descriptions for LLM tool selection guidance.
    server_descriptions: HashMap<String, String>,
}

impl McpRegistry {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            tool_index: RwLock::new(HashMap::new()),
            server_descriptions: HashMap::new(),
        }
    }

    /// Register a client and index its tools.
    pub async fn register(&mut self, name: String, description: String, client: Arc<McpClient>) {
        if !description.is_empty() {
            self.server_descriptions.insert(name.clone(), description);
        }
        let tools = client.list_tools().await;
        let mut index = self.tool_index.write().await;
        for tool in &tools {
            // Qualified form always registered
            let qualified = format!("{}/{}", name, tool.name);
            index.insert(qualified, name.clone());
            // Bare form only if no collision
            if !index.contains_key(&tool.name) {
                index.insert(tool.name.clone(), name.clone());
            }
        }
        drop(index);
        self.clients.insert(name, client);
    }

    /// Aggregate tools from all servers with qualified names.
    pub async fn list_all_tools(&self) -> Vec<McpToolDefinition> {
        let mut all = Vec::new();
        for (name, client) in &self.clients {
            let tools = client.list_tools().await;
            for tool in tools {
                all.push(McpToolDefinition {
                    name: format!("{}/{}", name, tool.name),
                    description: tool.description,
                    input_schema: tool.input_schema,
                    server_name: Some(name.clone()),
                });
            }
        }
        all
    }

    /// Route a tool call to the correct server.
    /// Accepts both "server/tool" and bare "tool" (if unambiguous).
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<McpToolCallResult> {
        let index = self.tool_index.read().await;
        let server_name = index
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("No MCP server registered for tool '{name}'"))?
            .clone();
        drop(index);

        let client = self
            .clients
            .get(&server_name)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{server_name}' not found"))?;

        // Strip server prefix for the actual RPC call
        let raw_name = name
            .strip_prefix(&format!("{}/", server_name))
            .unwrap_or(name);

        client.call_tool(raw_name, arguments).await
    }

    pub fn server_names(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }

    /// Return server-level descriptions for LLM guidance.
    pub fn server_descriptions(&self) -> &HashMap<String, String> {
        &self.server_descriptions
    }

    pub async fn shutdown_all(&self) {
        for (name, client) in &self.clients {
            debug!("Shutting down MCP server '{name}'");
            client.shutdown().await;
        }
    }
}
