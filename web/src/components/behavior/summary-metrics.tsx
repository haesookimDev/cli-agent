import type { RunBehaviorSummary, RunStatus } from "@/lib/types";

interface Props {
  summary: RunBehaviorSummary;
  status: RunStatus | null;
}

export function SummaryMetrics({ summary, status }: Props) {
  const items = [
    { label: "Status", value: status ?? "unknown" },
    {
      label: "Duration",
      value: summary.total_duration_ms != null
        ? `${summary.total_duration_ms}ms`
        : "-",
    },
    { label: "Peak Parallelism", value: summary.peak_parallelism },
    { label: "Failed Nodes", value: summary.failed_nodes },
    {
      label: "Critical Path",
      value: summary.critical_path_nodes.length > 0
        ? summary.critical_path_nodes.join(" → ")
        : "-",
    },
    {
      label: "Bottleneck",
      value: summary.bottleneck_node_id
        ? `${summary.bottleneck_node_id} (${summary.bottleneck_duration_ms}ms)`
        : "-",
    },
  ];

  return (
    <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
      {items.map((item) => (
        <div
          key={item.label}
          className="rounded-lg border border-slate-200 bg-slate-50 px-3 py-2"
        >
          <p className="text-[10px] font-medium uppercase tracking-wider text-slate-400">
            {item.label}
          </p>
          <p className="mt-0.5 truncate text-sm font-semibold text-slate-700">
            {item.value}
          </p>
        </div>
      ))}
    </div>
  );
}
