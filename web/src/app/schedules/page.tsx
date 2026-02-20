"use client";

import { useEffect, useState } from "react";
import { apiGet, apiPost, apiPatch, apiDelete } from "@/lib/api-client";
import type { CronSchedule, WorkflowTemplate } from "@/lib/types";

export default function SchedulesPage() {
  const [schedules, setSchedules] = useState<CronSchedule[]>([]);
  const [workflows, setWorkflows] = useState<WorkflowTemplate[]>([]);
  const [loading, setLoading] = useState(true);

  // Form state
  const [workflowId, setWorkflowId] = useState("");
  const [cronExpr, setCronExpr] = useState("0 0 9 * * *");
  const [creating, setCreating] = useState(false);

  function loadAll() {
    Promise.all([
      apiGet<CronSchedule[]>("/v1/schedules"),
      apiGet<WorkflowTemplate[]>("/v1/workflows"),
    ])
      .then(([s, w]) => {
        setSchedules(s);
        setWorkflows(w);
        if (w.length > 0 && !workflowId) setWorkflowId(w[0].id);
      })
      .catch((err) => console.error("load:", err))
      .finally(() => setLoading(false));
  }

  useEffect(() => {
    loadAll();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function createSchedule(e: React.FormEvent) {
    e.preventDefault();
    if (!workflowId || !cronExpr) return;
    setCreating(true);
    try {
      await apiPost("/v1/schedules", {
        workflow_id: workflowId,
        cron_expr: cronExpr,
        enabled: true,
      });
      loadAll();
    } catch (err) {
      console.error("create schedule:", err);
      alert(String(err));
    } finally {
      setCreating(false);
    }
  }

  async function toggleSchedule(id: string, currentEnabled: boolean) {
    try {
      await apiPatch(`/v1/schedules/${id}`, {
        enabled: !currentEnabled,
      });
      loadAll();
    } catch (err) {
      console.error("toggle schedule:", err);
    }
  }

  async function removeSchedule(id: string) {
    try {
      await apiDelete(`/v1/schedules/${id}`);
      loadAll();
    } catch (err) {
      console.error("delete schedule:", err);
    }
  }

  if (loading) {
    return (
      <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
    );
  }

  function workflowName(id: string) {
    return workflows.find((w) => w.id === id)?.name ?? id;
  }

  function fmtDate(s: string | null) {
    if (!s) return "-";
    return new Date(s).toLocaleString();
  }

  return (
    <div className="space-y-6">
      {/* Create form */}
      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <h2 className="mb-4 text-sm font-semibold text-slate-700">
          New Schedule
        </h2>
        <form onSubmit={createSchedule} className="flex flex-wrap items-end gap-3">
          <div className="flex-1 min-w-[180px]">
            <label className="mb-1 block text-xs text-slate-500">
              Workflow
            </label>
            <select
              value={workflowId}
              onChange={(e) => setWorkflowId(e.target.value)}
              className="w-full rounded-lg border border-slate-200 px-3 py-2 text-sm"
            >
              {workflows.length === 0 && (
                <option value="">No workflows</option>
              )}
              {workflows.map((w) => (
                <option key={w.id} value={w.id}>
                  {w.name}
                </option>
              ))}
            </select>
          </div>
          <div className="min-w-[200px]">
            <label className="mb-1 block text-xs text-slate-500">
              Cron Expression (6-field)
            </label>
            <input
              type="text"
              value={cronExpr}
              onChange={(e) => setCronExpr(e.target.value)}
              placeholder="0 0 9 * * *"
              className="w-full rounded-lg border border-slate-200 px-3 py-2 text-sm font-mono"
            />
          </div>
          <button
            type="submit"
            disabled={creating || !workflowId}
            className="rounded-lg bg-teal-600 px-4 py-2 text-sm font-medium text-white hover:bg-teal-700 disabled:opacity-50"
          >
            {creating ? "Creating..." : "Create"}
          </button>
        </form>
      </div>

      {/* Schedule list */}
      <div className="rounded-xl border border-slate-200 bg-white p-5">
        <h2 className="mb-4 text-sm font-semibold text-slate-700">
          Schedules
        </h2>

        {schedules.length === 0 ? (
          <p className="text-center text-sm text-slate-400">
            No schedules yet. Create one above.
          </p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="border-b border-slate-100 text-left text-slate-500">
                <tr>
                  <th className="px-3 py-2 font-medium">Workflow</th>
                  <th className="px-3 py-2 font-medium">Cron</th>
                  <th className="px-3 py-2 font-medium">Enabled</th>
                  <th className="px-3 py-2 font-medium">Last Run</th>
                  <th className="px-3 py-2 font-medium">Next Run</th>
                  <th className="px-3 py-2 font-medium text-center">
                    Actions
                  </th>
                </tr>
              </thead>
              <tbody className="font-mono">
                {schedules.map((s) => (
                  <tr
                    key={s.id}
                    className={`border-t border-slate-50 ${!s.enabled ? "opacity-40" : ""}`}
                  >
                    <td className="px-3 py-2 font-medium text-slate-700">
                      {workflowName(s.workflow_id)}
                    </td>
                    <td className="px-3 py-2 text-slate-500">{s.cron_expr}</td>
                    <td className="px-3 py-2">
                      <button
                        onClick={() => toggleSchedule(s.id, s.enabled)}
                        className={`inline-block h-5 w-9 rounded-full transition-colors ${
                          s.enabled ? "bg-teal-500" : "bg-slate-200"
                        }`}
                      >
                        <span
                          className={`block h-4 w-4 transform rounded-full bg-white shadow transition-transform ${
                            s.enabled ? "translate-x-4" : "translate-x-0.5"
                          }`}
                        />
                      </button>
                    </td>
                    <td className="px-3 py-2 text-slate-500">
                      {fmtDate(s.last_run_at)}
                    </td>
                    <td className="px-3 py-2 text-slate-500">
                      {fmtDate(s.next_run_at)}
                    </td>
                    <td className="px-3 py-2 text-center">
                      <button
                        onClick={() => removeSchedule(s.id)}
                        className="rounded px-2 py-1 text-xs text-red-500 hover:bg-red-50"
                      >
                        Delete
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
