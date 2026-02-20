"use client";

import { Suspense, useState, useCallback, useEffect } from "react";
import { useSearchParams } from "next/navigation";
import Link from "next/link";
import { apiGet } from "@/lib/api-client";
import { useInterval } from "@/hooks/use-interval";
import { StatusBadge } from "@/components/status-badge";
import type { RunRecord } from "@/lib/types";

const roleColors: Record<string, string> = {
  planner: "bg-violet-100 text-violet-700",
  extractor: "bg-sky-100 text-sky-700",
  coder: "bg-amber-100 text-amber-700",
  summarizer: "bg-emerald-100 text-emerald-700",
  fallback: "bg-slate-100 text-slate-600",
};

function ResultsContent() {
  const searchParams = useSearchParams();
  const initialRunId = searchParams.get("run") ?? "";

  const [runId, setRunId] = useState(initialRunId);
  const [inputValue, setInputValue] = useState(initialRunId);
  const [data, setData] = useState<RunRecord | null>(null);
  const [loading, setLoading] = useState(false);
  const [live, setLive] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    if (!runId) return;
    try {
      const result = await apiGet<RunRecord>(`/v1/runs/${runId}`);
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
          Loading results...
        </div>
      )}

      {data && (
        <>
          <div className="rounded-xl border border-slate-200 bg-white p-5">
            <div className="mb-3 flex items-center justify-between">
              <div className="flex items-center gap-3">
                <h3 className="text-sm font-semibold text-slate-700">
                  Run {data.run_id.slice(0, 8)}
                </h3>
                <StatusBadge status={data.status} />
              </div>
              <div className="flex gap-2">
                <Link
                  href={`/runs/${data.run_id}`}
                  className="text-xs text-teal-600 hover:underline"
                >
                  Detail
                </Link>
                <Link
                  href={`/trace?run=${data.run_id}`}
                  className="text-xs text-teal-600 hover:underline"
                >
                  Trace
                </Link>
              </div>
            </div>
            <dl className="mb-3 grid grid-cols-2 gap-x-6 gap-y-2 text-xs sm:grid-cols-4">
              <div>
                <dt className="text-slate-400">Session</dt>
                <dd className="font-mono text-teal-600">
                  <Link href={`/sessions/${data.session_id}`} className="hover:underline">
                    {data.session_id.slice(0, 8)}
                  </Link>
                </dd>
              </div>
              <div>
                <dt className="text-slate-400">Profile</dt>
                <dd className="text-slate-700">{data.profile}</dd>
              </div>
              <div>
                <dt className="text-slate-400">Created</dt>
                <dd className="text-slate-700">
                  {new Date(data.created_at).toLocaleString()}
                </dd>
              </div>
              <div>
                <dt className="text-slate-400">Duration</dt>
                <dd className="text-slate-700">
                  {data.started_at && data.finished_at
                    ? `${new Date(data.finished_at).getTime() - new Date(data.started_at).getTime()}ms`
                    : "-"}
                </dd>
              </div>
            </dl>
            <div className="rounded-lg bg-slate-50 px-3 py-2 text-sm text-slate-700">
              {data.task}
            </div>
            {data.error && (
              <div className="mt-3 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
                {data.error}
              </div>
            )}
          </div>

          <div className="rounded-xl border border-slate-200 bg-white">
            <h3 className="border-b border-slate-100 px-5 py-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
              Agent Outputs ({data.outputs.length})
            </h3>
            {data.outputs.length === 0 ? (
              <div className="p-6 text-center text-sm text-slate-400">
                No outputs yet
              </div>
            ) : (
              data.outputs.map((o, i) => (
                <details
                  key={i}
                  className="border-b border-slate-100 last:border-b-0"
                >
                  <summary className="flex cursor-pointer items-center gap-3 px-5 py-3 text-xs hover:bg-slate-50">
                    <span className="font-mono font-medium text-slate-700">
                      {o.node_id}
                    </span>
                    <span
                      className={`rounded-full px-2 py-0.5 text-[10px] font-medium ${roleColors[o.role] ?? "bg-slate-100 text-slate-600"}`}
                    >
                      {o.role}
                    </span>
                    <span className="text-slate-400">{o.model}</span>
                    <span
                      className={
                        o.succeeded ? "text-emerald-600" : "text-red-600"
                      }
                    >
                      {o.succeeded ? "OK" : "FAILED"}
                    </span>
                    <span className="ml-auto text-slate-400">
                      {o.duration_ms}ms
                    </span>
                  </summary>
                  <div className="px-5 pb-4">
                    {o.output ? (
                      <pre className="max-h-96 overflow-y-auto whitespace-pre-wrap rounded-lg bg-slate-50 p-3 font-mono text-xs text-slate-700">
                        {o.output}
                      </pre>
                    ) : (
                      <p className="text-xs text-slate-400">No output</p>
                    )}
                    {o.error && (
                      <div className="mt-2 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
                        {o.error}
                      </div>
                    )}
                  </div>
                </details>
              ))
            )}
          </div>
        </>
      )}
    </div>
  );
}

export default function ResultsPage() {
  return (
    <Suspense
      fallback={
        <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
      }
    >
      <ResultsContent />
    </Suspense>
  );
}
