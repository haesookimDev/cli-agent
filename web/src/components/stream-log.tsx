"use client";

import { useRef, useEffect } from "react";
import type { RunActionEvent } from "@/lib/types";

export function StreamLog({ events }: { events: RunActionEvent[] }) {
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [events.length]);

  if (events.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-slate-300 p-6 text-center text-sm text-slate-400">
        Waiting for events...
      </div>
    );
  }

  return (
    <div className="max-h-80 overflow-y-auto rounded-lg border border-slate-200 bg-white">
      <table className="w-full text-xs">
        <thead className="sticky top-0 bg-slate-50 text-left text-slate-500">
          <tr>
            <th className="px-3 py-2 font-medium">#</th>
            <th className="px-3 py-2 font-medium">Action</th>
            <th className="px-3 py-2 font-medium">Actor</th>
            <th className="px-3 py-2 font-medium">Time</th>
          </tr>
        </thead>
        <tbody className="font-mono">
          {events.map((ev) => (
            <tr key={ev.seq} className="border-t border-slate-100">
              <td className="px-3 py-1.5 text-slate-400">{ev.seq}</td>
              <td className="px-3 py-1.5 font-medium text-slate-800">
                {ev.action}
              </td>
              <td className="px-3 py-1.5 text-slate-500">
                {ev.actor_id ?? "-"}
              </td>
              <td className="px-3 py-1.5 text-slate-400">
                {new Date(ev.timestamp).toLocaleTimeString()}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <div ref={endRef} />
    </div>
  );
}
