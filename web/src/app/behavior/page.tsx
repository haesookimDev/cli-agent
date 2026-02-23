"use client";

import { Suspense, useState, useCallback, useEffect } from "react";
import { useSearchParams } from "next/navigation";
import { apiGet } from "@/lib/api-client";
import { useInterval } from "@/hooks/use-interval";
import { getLastRunId, setLastRunId } from "@/lib/session-store";
import { SummaryMetrics } from "@/components/behavior/summary-metrics";
import { SwimLane } from "@/components/behavior/swim-lane";
import { ActionMix } from "@/components/behavior/action-mix";
import type { RunBehaviorView } from "@/lib/types";

function BehaviorContent() {
  const searchParams = useSearchParams();
  const initialRunId = searchParams.get("run") || getLastRunId("behavior");

  const [runId, setRunId] = useState(initialRunId);
  const [inputValue, setInputValue] = useState(initialRunId);
  const [data, setData] = useState<RunBehaviorView | null>(null);
  const [loading, setLoading] = useState(false);
  const [live, setLive] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchBehavior = useCallback(async () => {
    if (!runId) return;
    try {
      const result = await apiGet<RunBehaviorView>(
        `/v1/runs/${runId}/behavior?limit=2000`,
      );
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
      setLastRunId("behavior", runId);
      const url = new URL(window.location.href);
      url.searchParams.set("run", runId);
      window.history.replaceState(null, "", url.toString());
      setLoading(true);
      fetchBehavior().finally(() => setLoading(false));
    }
  }, [runId, fetchBehavior]);

  useInterval(fetchBehavior, live ? 1200 : null);

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
          Loading behavior...
        </div>
      )}

      {data && (
        <>
          <div className="rounded-xl border border-slate-200 bg-white p-5">
            <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
              Summary
            </h3>
            <SummaryMetrics summary={data.summary} status={data.status} />
          </div>

          <div className="rounded-xl border border-slate-200 bg-white p-5">
            <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
              Execution Timeline
            </h3>
            <SwimLane
              lanes={data.lanes}
              totalDurationMs={data.summary.total_duration_ms}
            />
          </div>

          <div className="rounded-xl border border-slate-200 bg-white p-5">
            <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
              Action Mix (Top 10)
            </h3>
            <ActionMix actionMix={data.action_mix} />
          </div>
        </>
      )}
    </div>
  );
}

export default function BehaviorPage() {
  return (
    <Suspense
      fallback={
        <div className="p-6 text-center text-sm text-slate-400">Loading...</div>
      }
    >
      <BehaviorContent />
    </Suspense>
  );
}
