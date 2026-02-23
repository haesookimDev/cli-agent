"use client";

import { TerminalPanel } from "@/components/terminal-panel";

export default function TerminalPage() {
  return (
    <div className="overflow-hidden rounded-xl border border-slate-200 bg-white">
      <TerminalPanel compact={false} />
    </div>
  );
}
