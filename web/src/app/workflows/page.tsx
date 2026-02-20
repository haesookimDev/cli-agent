"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { apiGet, apiPost, apiDelete } from "@/lib/api-client";
import type { WorkflowTemplate, RunSubmission } from "@/lib/types";

export default function WorkflowsPage() {
  const router = useRouter();
  const [workflows, setWorkflows] = useState<WorkflowTemplate[]>([]);
  const [loading, setLoading] = useState(true);

  async function load() {
    setLoading(true);
    try {
      const data = await apiGet<WorkflowTemplate[]>("/v1/workflows?limit=50");
      setWorkflows(data);
    } catch (err) {
      console.error("load workflows:", err);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    load();
  }, []);

  async function handleExecute(wfId: string) {
    try {
      const sub = await apiPost<RunSubmission>(
        `/v1/workflows/${wfId}/execute`,
        {},
      );
      router.push(`/runs/${sub.run_id}`);
    } catch (err) {
      console.error("execute workflow:", err);
    }
  }

  async function handleDelete(wfId: string) {
    try {
      await apiDelete(`/v1/workflows/${wfId}`);
      load();
    } catch (err) {
      console.error("delete workflow:", err);
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-slate-700">Workflows</h2>
        <button
          onClick={load}
          className="rounded-lg border border-slate-300 px-3 py-1.5 text-xs font-medium text-slate-600 hover:bg-slate-50"
        >
          Refresh
        </button>
      </div>

      <div className="rounded-xl border border-slate-200 bg-white">
        {loading ? (
          <div className="p-6 text-center text-sm text-slate-400">
            Loading...
          </div>
        ) : workflows.length === 0 ? (
          <div className="p-6 text-center text-sm text-slate-400">
            No workflows saved yet. Run a task and save it as a workflow from
            the run detail page.
          </div>
        ) : (
          <table className="w-full text-sm">
            <thead className="border-b border-slate-100 text-left text-xs text-slate-500">
              <tr>
                <th className="px-4 py-3 font-medium">Name</th>
                <th className="px-4 py-3 font-medium">Description</th>
                <th className="px-4 py-3 font-medium">Nodes</th>
                <th className="px-4 py-3 font-medium">Params</th>
                <th className="px-4 py-3 font-medium">Created</th>
                <th className="px-4 py-3 font-medium">Actions</th>
              </tr>
            </thead>
            <tbody>
              {workflows.map((wf) => (
                <tr
                  key={wf.id}
                  className="border-t border-slate-50 hover:bg-slate-50"
                >
                  <td className="px-4 py-3">
                    <Link
                      href={`/workflows/${wf.id}`}
                      className="text-xs font-medium text-teal-600 hover:underline"
                    >
                      {wf.name}
                    </Link>
                  </td>
                  <td className="max-w-xs truncate px-4 py-3 text-xs text-slate-500">
                    {wf.description || "-"}
                  </td>
                  <td className="px-4 py-3 text-xs text-slate-500">
                    {wf.graph_template.nodes.length}
                  </td>
                  <td className="px-4 py-3 text-xs text-slate-500">
                    {wf.parameters.length}
                  </td>
                  <td className="px-4 py-3 text-xs text-slate-400">
                    {new Date(wf.created_at).toLocaleString()}
                  </td>
                  <td className="px-4 py-3">
                    <div className="flex gap-1">
                      <button
                        onClick={() => handleExecute(wf.id)}
                        className="rounded-md bg-teal-600 px-2 py-1 text-xs font-medium text-white hover:bg-teal-700"
                      >
                        Execute
                      </button>
                      <button
                        onClick={() => handleDelete(wf.id)}
                        className="rounded-md border border-red-300 px-2 py-1 text-xs font-medium text-red-600 hover:bg-red-50"
                      >
                        Delete
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
