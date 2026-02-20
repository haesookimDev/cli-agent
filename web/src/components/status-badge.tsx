import type { RunStatus } from "@/lib/types";

const styles: Record<RunStatus, string> = {
  succeeded: "bg-emerald-100 text-emerald-800",
  failed: "bg-red-100 text-red-800",
  cancelled: "bg-red-100 text-red-700",
  running: "bg-blue-100 text-blue-800",
  cancelling: "bg-blue-100 text-blue-700",
  queued: "bg-amber-100 text-amber-800",
  paused: "bg-amber-100 text-amber-700",
};

export function StatusBadge({ status }: { status: RunStatus }) {
  return (
    <span
      className={`inline-block rounded-full px-2.5 py-0.5 text-xs font-medium ${styles[status] ?? "bg-slate-100 text-slate-700"}`}
    >
      {status}
    </span>
  );
}
