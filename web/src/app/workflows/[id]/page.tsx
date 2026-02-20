"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { useParams, useRouter } from "next/navigation";
import { apiGet, apiPost } from "@/lib/api-client";
import { DagGraph } from "@/components/trace/dag-graph";
import type {
  WorkflowTemplate,
  RunSubmission,
  NodeTraceState,
  TraceEdge,
} from "@/lib/types";

export default function WorkflowDetailPage() {
  const { id } = useParams<{ id: string }>();
  const router = useRouter();
  const [wf, setWf] = useState<WorkflowTemplate | null>(null);
  const [loading, setLoading] = useState(true);
  const [executing, setExecuting] = useState(false);

  useEffect(() => {
    if (!id) return;
    setLoading(true);
    apiGet<WorkflowTemplate>(`/v1/workflows/${id}`)
      .then(setWf)
      .catch((err) => console.error("load workflow:", err))
      .finally(() => setLoading(false));
  }, [id]);

  async function handleExecute() {
    if (!wf) return;
    setExecuting(true);
    try {
      const sub = await apiPost<RunSubmission>(
        `/v1/workflows/${wf.id}/execute`,
        {},
      );
      router.push(`/runs/${sub.run_id}`);
    } catch (err) {
      console.error("execute workflow:", err);
    } finally {
      setExecuting(false);
    }
  }

  if (loading) {
    return (
      <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
    );
  }

  if (!wf) {
    return (
      <div className="p-6 text-center text-sm text-red-500">
        Workflow not found
      </div>
    );
  }

  // Convert workflow nodes to trace-compatible format for DagGraph
  const traceNodes: NodeTraceState[] = wf.graph_template.nodes.map((n) => ({
    node_id: n.id,
    role: n.role,
    dependencies: n.dependencies,
    status: "pending",
    started_at: null,
    finished_at: null,
    duration_ms: null,
    retries: 0,
    model: null,
  }));

  const traceEdges: TraceEdge[] = wf.graph_template.nodes.flatMap((n) =>
    n.dependencies.map((dep) => ({ from: dep, to: n.id })),
  );

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <Link
          href="/workflows"
          className="text-xs text-teal-600 hover:underline"
        >
          Workflows
        </Link>
        <span className="text-xs text-slate-400">/</span>
        <span className="text-xs text-slate-600">{wf.name}</span>
      </div>

      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-sm font-semibold text-slate-700">{wf.name}</h2>
          <button
            onClick={handleExecute}
            disabled={executing}
            className="rounded-md bg-teal-600 px-4 py-1.5 text-xs font-medium text-white hover:bg-teal-700 disabled:opacity-50"
          >
            {executing ? "Executing..." : "Execute"}
          </button>
        </div>

        {wf.description && (
          <p className="mb-4 text-sm text-slate-600">{wf.description}</p>
        )}

        <dl className="mb-4 grid grid-cols-2 gap-x-6 gap-y-2 text-xs sm:grid-cols-4">
          <div>
            <dt className="text-slate-400">ID</dt>
            <dd className="font-mono text-slate-700">
              {wf.id.slice(0, 8)}
            </dd>
          </div>
          <div>
            <dt className="text-slate-400">Nodes</dt>
            <dd className="text-slate-700">
              {wf.graph_template.nodes.length}
            </dd>
          </div>
          <div>
            <dt className="text-slate-400">Parameters</dt>
            <dd className="text-slate-700">{wf.parameters.length}</dd>
          </div>
          <div>
            <dt className="text-slate-400">Created</dt>
            <dd className="text-slate-700">
              {new Date(wf.created_at).toLocaleString()}
            </dd>
          </div>
        </dl>

        {wf.source_run_id && (
          <div className="text-xs text-slate-400">
            Source run:{" "}
            <Link
              href={`/runs/${wf.source_run_id}`}
              className="font-mono text-teal-600 hover:underline"
            >
              {wf.source_run_id.slice(0, 8)}
            </Link>
          </div>
        )}
      </div>

      {/* Graph Visualization */}
      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
          Workflow Graph
        </h3>
        <DagGraph nodes={traceNodes} edges={traceEdges} activeNodes={[]} />
      </div>

      {/* Node Details */}
      {wf.graph_template.nodes.length > 0 && (
        <div className="rounded-xl border border-slate-200 bg-white">
          <h3 className="border-b border-slate-100 px-5 py-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
            Nodes ({wf.graph_template.nodes.length})
          </h3>
          <div className="overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="border-b border-slate-100 text-left text-slate-500">
                <tr>
                  <th className="px-4 py-2 font-medium">ID</th>
                  <th className="px-4 py-2 font-medium">Role</th>
                  <th className="px-4 py-2 font-medium">Dependencies</th>
                  <th className="px-4 py-2 font-medium">MCP Tools</th>
                  <th className="px-4 py-2 font-medium">Instructions</th>
                </tr>
              </thead>
              <tbody className="font-mono">
                {wf.graph_template.nodes.map((n) => (
                  <tr key={n.id} className="border-t border-slate-50">
                    <td className="px-4 py-2 font-medium text-slate-700">
                      {n.id}
                    </td>
                    <td className="px-4 py-2 text-slate-500">{n.role}</td>
                    <td className="px-4 py-2 text-slate-400">
                      {n.dependencies.length > 0
                        ? n.dependencies.join(", ")
                        : "-"}
                    </td>
                    <td className="px-4 py-2 text-slate-400">
                      {n.mcp_tools.length > 0
                        ? n.mcp_tools.join(", ")
                        : "-"}
                    </td>
                    <td className="max-w-xs truncate px-4 py-2 text-slate-600">
                      {n.instructions || "-"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}

      {/* Parameters */}
      {wf.parameters.length > 0 && (
        <div className="rounded-xl border border-slate-200 bg-white">
          <h3 className="border-b border-slate-100 px-5 py-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
            Parameters ({wf.parameters.length})
          </h3>
          <div className="overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="border-b border-slate-100 text-left text-slate-500">
                <tr>
                  <th className="px-4 py-2 font-medium">Name</th>
                  <th className="px-4 py-2 font-medium">Description</th>
                  <th className="px-4 py-2 font-medium">Default</th>
                </tr>
              </thead>
              <tbody>
                {wf.parameters.map((p) => (
                  <tr key={p.name} className="border-t border-slate-50">
                    <td className="px-4 py-2 font-mono text-slate-700">
                      {p.name}
                    </td>
                    <td className="px-4 py-2 text-slate-500">
                      {p.description || "-"}
                    </td>
                    <td className="px-4 py-2 font-mono text-slate-400">
                      {p.default_value ?? "-"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}
