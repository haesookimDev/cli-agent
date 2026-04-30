"use client";

import { useMemo, useState } from "react";
import {
  buildPersonaTimelines,
  PersonaSegment,
  PersonaTimeline,
} from "@/lib/persona-timeline";
import type { RunActionEvent } from "@/lib/types";

interface Props {
  events: RunActionEvent[];
}

export function PersonaSwimlane({ events }: Props) {
  const lanes = useMemo(() => buildPersonaTimelines(events), [events]);
  const [filter, setFilter] = useState("");

  const visibleLanes = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return lanes;
    return lanes.filter((l) => l.persona_name.toLowerCase().includes(q));
  }, [lanes, filter]);

  if (lanes.length === 0) {
    return (
      <div className="rounded border border-dashed border-slate-300 p-4 text-sm text-slate-500 text-center">
        No persona-tagged events yet. Trace will populate once the run starts
        producing NodeStarted/NodeCompleted events.
      </div>
    );
  }

  // Compute a shared time axis so segments line up across lanes.
  const allStarts = lanes
    .flatMap((l) => l.segments.map((s) => (s.started_at ? Date.parse(s.started_at) : null)))
    .filter((t): t is number => t != null);
  const allEnds = lanes
    .flatMap((l) => l.segments.map((s) => (s.ended_at ? Date.parse(s.ended_at) : null)))
    .filter((t): t is number => t != null);
  const t0 = allStarts.length ? Math.min(...allStarts) : 0;
  const t1 = allEnds.length ? Math.max(...allEnds) : t0 + 1000;
  const span = Math.max(1, t1 - t0);

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-3">
        <input
          type="text"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter by persona name..."
          className="flex-1 px-2 py-1 border border-slate-300 rounded text-sm"
        />
        <span className="text-xs text-slate-400">
          {visibleLanes.length} of {lanes.length} lanes
        </span>
      </div>

      <div className="space-y-2">
        {visibleLanes.map((lane) => (
          <PersonaLane key={lane.persona_name} lane={lane} t0={t0} span={span} />
        ))}
      </div>
    </div>
  );
}

function PersonaLane({
  lane,
  t0,
  span,
}: {
  lane: PersonaTimeline;
  t0: number;
  span: number;
}) {
  const [expanded, setExpanded] = useState<string | null>(null);
  const orchestrator = lane.persona_name === "orchestrator";

  return (
    <div className="rounded-xl border border-slate-200 bg-white">
      <div className="flex items-center justify-between px-3 py-2 border-b border-slate-100">
        <div className="flex items-center gap-2 min-w-0">
          {!orchestrator && (
            <div className="w-7 h-7 rounded-full bg-gradient-to-br from-blue-500 to-purple-600 flex items-center justify-center text-white text-[11px] font-bold shrink-0">
              {lane.persona_name.charAt(0)}
            </div>
          )}
          <div className="min-w-0">
            <p className="text-sm font-medium text-slate-800 truncate">
              {lane.persona_name}
            </p>
            <p className="text-[10px] text-slate-400">
              {lane.role ?? "—"} · {lane.totals.nodes} nodes ·{" "}
              {lane.totals.failures} failed · {lane.totals.tool_calls} tools ·{" "}
              {lane.totals.duration_ms}ms
            </p>
          </div>
        </div>
      </div>

      <div className="relative h-10 bg-slate-50 mx-3 my-2 rounded">
        {lane.segments.map((seg) => {
          const start = seg.started_at ? Date.parse(seg.started_at) : t0;
          const end = seg.ended_at ? Date.parse(seg.ended_at) : start + 100;
          const left = ((start - t0) / span) * 100;
          const width = Math.max(1, ((end - start) / span) * 100);
          const isOpen = expanded === seg.node_id;
          const color =
            seg.status === "succeeded"
              ? "bg-emerald-400"
              : seg.status === "failed"
                ? "bg-red-400"
                : seg.status === "skipped"
                  ? "bg-slate-300"
                  : "bg-amber-300";
          return (
            <button
              key={seg.node_id}
              onClick={() => setExpanded(isOpen ? null : seg.node_id)}
              style={{ left: `${left}%`, width: `${width}%` }}
              className={`absolute top-1 h-8 rounded text-[10px] text-white px-1 truncate ${color} hover:opacity-80`}
              title={`${seg.node_id} · ${seg.status}`}
            >
              {seg.node_id}
            </button>
          );
        })}
      </div>

      {expanded && (
        <SegmentDetail
          segment={lane.segments.find((s) => s.node_id === expanded) ?? null}
          onClose={() => setExpanded(null)}
        />
      )}
    </div>
  );
}

function SegmentDetail({
  segment,
  onClose,
}: {
  segment: PersonaSegment | null;
  onClose: () => void;
}) {
  if (!segment) return null;
  return (
    <div className="border-t border-slate-100 px-3 py-2 bg-slate-50">
      <div className="flex items-center justify-between mb-2">
        <p className="text-xs font-medium text-slate-700">
          {segment.node_id}{" "}
          <span className="text-slate-400">· {segment.role ?? "—"}</span>
        </p>
        <button
          onClick={onClose}
          className="text-[10px] text-slate-500 hover:text-slate-700"
        >
          close
        </button>
      </div>
      <p className="text-[10px] text-slate-500">
        status: {segment.status} · duration:{" "}
        {segment.duration_ms != null ? `${segment.duration_ms}ms` : "—"} ·
        tools: {segment.summary.tool_calls} · file changes:{" "}
        {segment.summary.file_changes} · memory writes:{" "}
        {segment.summary.memory_writes} · messages: {segment.summary.messages}
      </p>
      <details className="mt-2 text-[10px] text-slate-500">
        <summary className="cursor-pointer">
          {segment.events.length} events
        </summary>
        <ul className="mt-1 space-y-0.5 max-h-48 overflow-y-auto">
          {segment.events.map((ev) => (
            <li key={ev.event_id} className="font-mono">
              {ev.action}{" "}
              <span className="text-slate-400">
                {new Date(ev.timestamp).toLocaleTimeString()}
              </span>
            </li>
          ))}
        </ul>
      </details>
    </div>
  );
}
