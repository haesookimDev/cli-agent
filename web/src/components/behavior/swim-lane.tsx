import type { RunBehaviorLane } from "@/lib/types";

const statusColors: Record<string, string> = {
  running: "bg-blue-400",
  succeeded: "bg-emerald-500",
  failed: "bg-red-500",
  pending: "bg-amber-400",
  skipped: "bg-slate-300",
  cancelled: "bg-slate-400",
};

interface Props {
  lanes: RunBehaviorLane[];
  totalDurationMs: number | null;
}

export function SwimLane({ lanes, totalDurationMs }: Props) {
  const total = totalDurationMs ?? 1;

  if (lanes.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-slate-300 p-6 text-center text-sm text-slate-400">
        No lanes to display
      </div>
    );
  }

  return (
    <div className="max-h-96 overflow-y-auto rounded-lg border border-slate-200 bg-white">
      {lanes.map((lane) => {
        const left =
          lane.start_offset_ms != null ? (lane.start_offset_ms / total) * 100 : 0;
        const width =
          lane.duration_ms != null ? (lane.duration_ms / total) * 100 : 0;
        const color = statusColors[lane.status] ?? "bg-slate-300";

        return (
          <div
            key={lane.node_id}
            className="flex items-center border-b border-slate-50 px-3 py-1.5"
          >
            <div className="w-28 shrink-0 truncate pr-2 text-xs text-slate-600">
              <span className="font-medium">{lane.node_id}</span>
              {lane.retries > 0 && (
                <span className="ml-1 text-[10px] text-amber-600">
                  r{lane.retries}
                </span>
              )}
            </div>
            <div className="relative h-5 flex-1 rounded-full bg-slate-100">
              {width > 0 && (
                <div
                  className={`absolute top-0 h-full rounded-full ${color}`}
                  style={{
                    left: `${left}%`,
                    width: `${Math.max(width, 0.5)}%`,
                  }}
                />
              )}
            </div>
            <div className="w-16 shrink-0 pl-2 text-right text-[10px] text-slate-400">
              {lane.duration_ms != null ? `${lane.duration_ms}ms` : "-"}
            </div>
          </div>
        );
      })}
    </div>
  );
}
