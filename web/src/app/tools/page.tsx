"use client";

import { useEffect, useState } from "react";
import { apiGet } from "@/lib/api-client";
import type { McpToolDefinition } from "@/lib/types";

export default function ToolsPage() {
  const [tools, setTools] = useState<McpToolDefinition[]>([]);
  const [servers, setServers] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    Promise.all([
      apiGet<McpToolDefinition[]>("/v1/mcp/tools"),
      apiGet<string[]>("/v1/mcp/servers"),
    ])
      .then(([t, s]) => {
        setTools(t);
        setServers(s);
      })
      .catch((err) => console.error("load mcp:", err))
      .finally(() => setLoading(false));
  }, []);

  if (loading) {
    return (
      <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
    );
  }

  // Group tools by server
  const byServer: Record<string, McpToolDefinition[]> = {};
  for (const tool of tools) {
    const server = tool.server_name ?? tool.name.split("/")[0] ?? "unknown";
    if (!byServer[server]) byServer[server] = [];
    byServer[server].push(tool);
  }

  return (
    <div className="space-y-6">
      {/* Server overview */}
      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <h2 className="mb-3 text-sm font-semibold text-slate-700">
          MCP Servers ({servers.length})
        </h2>
        <div className="flex flex-wrap gap-2">
          {servers.map((s) => (
            <span
              key={s}
              className="rounded-full border border-teal-200 bg-teal-50 px-3 py-1 text-xs font-medium text-teal-700"
            >
              {s}
            </span>
          ))}
          {servers.length === 0 && (
            <span className="text-xs text-slate-400">
              No MCP servers registered
            </span>
          )}
        </div>
      </div>

      {/* Tools grouped by server */}
      {Object.entries(byServer).map(([server, serverTools]) => (
        <div
          key={server}
          className="rounded-xl border border-slate-200 bg-white p-5"
        >
          <h3 className="mb-3 text-sm font-semibold text-slate-700">
            {server}{" "}
            <span className="font-normal text-slate-400">
              ({serverTools.length} tools)
            </span>
          </h3>
          <div className="overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="border-b border-slate-100 text-left text-slate-500">
                <tr>
                  <th className="px-3 py-2 font-medium">Tool</th>
                  <th className="px-3 py-2 font-medium">Description</th>
                  <th className="px-3 py-2 font-medium">Input Schema</th>
                </tr>
              </thead>
              <tbody className="font-mono">
                {serverTools.map((tool) => (
                  <tr key={tool.name} className="border-t border-slate-50">
                    <td className="whitespace-nowrap px-3 py-2 font-medium text-slate-700">
                      {tool.name.includes("/")
                        ? tool.name.split("/").slice(1).join("/")
                        : tool.name}
                    </td>
                    <td className="max-w-xs truncate px-3 py-2 text-slate-500">
                      {tool.description}
                    </td>
                    <td className="px-3 py-2 text-slate-400">
                      <details>
                        <summary className="cursor-pointer text-teal-600 hover:text-teal-800">
                          schema
                        </summary>
                        <pre className="mt-1 max-h-40 overflow-auto rounded bg-slate-50 p-2 text-[10px]">
                          {JSON.stringify(tool.input_schema, null, 2)}
                        </pre>
                      </details>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      ))}

      {tools.length === 0 && (
        <div className="rounded-xl border border-slate-200 bg-white p-8 text-center text-sm text-slate-400">
          No MCP tools available. Configure MCP_SERVERS in your .env file.
        </div>
      )}
    </div>
  );
}
