import type { RunBehaviorActionCount } from "@/lib/types";

interface Props {
  actionMix: RunBehaviorActionCount[];
}

export function ActionMix({ actionMix }: Props) {
  const top10 = actionMix.slice(0, 10);
  const maxCount = Math.max(...top10.map((a) => a.count), 1);

  if (top10.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-slate-300 p-6 text-center text-sm text-slate-400">
        No actions recorded
      </div>
    );
  }

  return (
    <div className="space-y-1.5">
      {top10.map((item) => (
        <div key={item.action} className="flex items-center gap-2">
          <div className="w-32 shrink-0 truncate text-right text-xs text-slate-500">
            {item.action}
          </div>
          <div className="relative h-5 flex-1 rounded-full bg-slate-100">
            <div
              className="h-full rounded-full bg-gradient-to-r from-orange-400 to-teal-500"
              style={{ width: `${(item.count / maxCount) * 100}%` }}
            />
          </div>
          <div className="w-8 shrink-0 text-right text-xs font-medium text-slate-600">
            {item.count}
          </div>
        </div>
      ))}
    </div>
  );
}
