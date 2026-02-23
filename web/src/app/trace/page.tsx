"use client";

import { Suspense, useState, useCallback, useEffect } from "react";
import { useSearchParams } from "next/navigation";
import Link from "next/link";
import { apiGet } from "@/lib/api-client";
import { useInterval } from "@/hooks/use-interval";
import { getLastRunId, setLastRunId } from "@/lib/session-store";
import { StatusBadge } from "@/components/status-badge";
import { DagGraph } from "@/components/trace/dag-graph";
import { EventTimeline } from "@/components/trace/event-timeline";
import type { RunTrace } from "@/lib/types";

function TraceContent() {
  const searchParams = useSearchParams();
  const initialRunId = searchParams.get("run") || getLastRunId("trace");

  const [runId, setRunId] = useState(initialRunId);
  const [inputValue, setInputValue] = useState(initialRunId);
  const [data, setData] = useState<RunTrace | null>(null);
  const [loading, setLoading] = useState(false);
  const [live, setLive] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    if (!runId) return;
    try {
      const result = await apiGet<RunTrace>(`/v1/runs/${runId}/trace`);
      setData(result);
      setError(null);
      if (
        result.status === "succeeded" ||
        result.status === "failed" ||
        result.status === "cancelled"
      ) {
        setLive(false);
      }
    } catch (err) {
      setError((err as Error).message);
    }
  }, [runId]);

  function handleLoad() {
    setRunId(inputValue.trim());
  }

  useEffect(() => {
    if (runId) {
      setLastRunId("trace", runId);
      const url = new URL(window.location.href);
      url.searchParams.set("run", runId);
      window.history.replaceState(null, "", url.toString());
      setLoading(true);
      fetchData().finally(() => setLoading(false));
    }
  }, [runId, fetchData]);

  useInterval(fetchData, live ? 1200 : null);

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <input
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleLoad()}
          placeholder="Enter run ID..."
          className="flex-1 rounded-lg border border-slate-200 bg-white px-3 py-2 text-sm placeholder:text-slate-400 focus:border-teal-500 focus:outline-none"
        />
        <button
          onClick={handleLoad}
          disabled={!inputValue.trim()}
          className="rounded-lg bg-teal-600 px-4 py-2 text-sm font-medium text-white hover:bg-teal-700 disabled:opacity-50"
        >
          Load
        </button>
        <button
          onClick={() => setLive(!live)}
          className={`rounded-lg border px-4 py-2 text-sm font-medium ${
            live
              ? "border-teal-600 bg-teal-50 text-teal-700"
              : "border-slate-300 text-slate-600 hover:bg-slate-50"
          }`}
        >
          {live ? "Live ON" : "Live OFF"}
        </button>
      </div>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
          {error}
        </div>
      )}

      {loading && !data && (
        <div className="p-6 text-center text-sm text-slate-400">
          Loading trace...
        </div>
      )}

      {data && (
        <>
          {/* Graph Summary */}
          <div className="rounded-xl border border-slate-200 bg-white p-5">
            <div className="mb-3 flex items-center justify-between">
              <h3 className="text-xs font-semibold uppercase tracking-wider text-slate-400">
                Graph Summary
              </h3>
              <div className="flex items-center gap-2">
                {data.status && <StatusBadge status={data.status} />}
                <Link
                  href={`/results?run=${data.run_id}`}
                  className="text-xs text-teal-600 hover:underline"
                >
                  Results
                </Link>
              </div>
            </div>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-5">
              {[
                { label: "Total Nodes", value: data.graph.nodes.length },
                { label: "Completed", value: data.graph.completed_nodes },
                { label: "Failed", value: data.graph.failed_nodes },
                {
                  label: "Active",
                  value: data.graph.active_nodes.length,
                },
                { label: "Events", value: data.events.length },
              ].map((item) => (
                <div
                  key={item.label}
                  className="rounded-lg border border-slate-200 bg-slate-50 px-3 py-2"
                >
                  <p className="text-[10px] font-medium uppercase tracking-wider text-slate-400">
                    {item.label}
                  </p>
                  <p className="mt-0.5 text-sm font-semibold text-slate-700">
                    {item.value}
                  </p>
                </div>
              ))}
            </div>
          </div>

          {/* DAG Graph */}
          <div className="rounded-xl border border-slate-200 bg-white p-5">
            <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
              Dependency Graph
            </h3>
            <DagGraph
              nodes={data.graph.nodes}
              edges={data.graph.edges}
              activeNodes={data.graph.active_nodes}
            />
          </div>

          {/* Node Details */}
          <div className="rounded-xl border border-slate-200 bg-white">
            <h3 className="border-b border-slate-100 px-5 py-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
              Nodes ({data.graph.nodes.length})
            </h3>
            <div className="overflow-x-auto">
              <table className="w-full text-xs">
                <thead className="border-b border-slate-100 text-left text-slate-500">
                  <tr>
                    <th className="px-4 py-2 font-medium">Node</th>
                    <th className="px-4 py-2 font-medium">Role</th>
                    <th className="px-4 py-2 font-medium">Status</th>
                    <th className="px-4 py-2 font-medium">Model</th>
                    <th className="px-4 py-2 font-medium">Duration</th>
                    <th className="px-4 py-2 font-medium">Retries</th>
                    <th className="px-4 py-2 font-medium">Dependencies</th>
                  </tr>
                </thead>
                <tbody className="font-mono">
                  {data.graph.nodes.map((n) => (
                    <tr
                      key={n.node_id}
                      className="border-t border-slate-50"
                    >
                      <td className="px-4 py-2 font-medium text-slate-700">
                        {n.node_id}
                      </td>
                      <td className="px-4 py-2 text-slate-500">
                        {n.role ?? "-"}
                      </td>
                      <td className="px-4 py-2">
                        <span
                          className={
                            n.status === "succeeded"
                              ? "text-emerald-600"
                              : n.status === "failed"
                                ? "text-red-600"
                                : n.status === "running"
                                  ? "text-blue-600"
                                  : "text-slate-500"
                          }
                        >
                          {n.status}
                        </span>
                      </td>
                      <td className="px-4 py-2 text-slate-500">
                        {n.model ?? "-"}
                      </td>
                      <td className="px-4 py-2 text-slate-500">
                        {n.duration_ms != null ? `${n.duration_ms}ms` : "-"}
                      </td>
                      <td className="px-4 py-2 text-slate-500">
                        {n.retries > 0 ? n.retries : "-"}
                      </td>
                      <td className="px-4 py-2 text-slate-400">
                        {n.dependencies.length > 0
                          ? n.dependencies.join(", ")
                          : "-"}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>

          {/* Event Timeline */}
          <div className="rounded-xl border border-slate-200 bg-white p-5">
            <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
              Event Timeline ({data.events.length})
            </h3>
            <EventTimeline events={data.events} />
          </div>
        </>
      )}
    </div>
  );
}

export default function TracePage() {
  return (
    <Suspense
      fallback={
        <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
      }
    >
      <TraceContent />
    </Suspense>
  );
}
