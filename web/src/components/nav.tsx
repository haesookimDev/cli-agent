"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useActiveRuns } from "@/hooks/use-active-runs";

const tabs = [
  { href: "/", label: "Runner" },
  { href: "/chat", label: "Chat" },
  { href: "/sessions", label: "Sessions" },
  { href: "/memory", label: "Memory" },
  { href: "/runs", label: "Runs" },
  { href: "/behavior", label: "Behavior" },
  { href: "/results", label: "Results" },
  { href: "/trace", label: "Trace" },
  { href: "/workflows", label: "Workflows" },
  { href: "/schedules", label: "Schedules" },
  { href: "/terminal", label: "Terminal" },
  { href: "/tools", label: "Tools" },
  { href: "/settings", label: "Settings" },
];

export function Nav() {
  const pathname = usePathname();
  const { count } = useActiveRuns();

  return (
    <nav className="mb-4 rounded-xl border border-black/10 bg-white/60 p-1 backdrop-blur">
      <div className="flex flex-nowrap gap-1 overflow-x-auto pb-1 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
        {tabs.map((tab) => {
          const active =
            tab.href === "/"
              ? pathname === "/"
              : pathname.startsWith(tab.href);
          return (
            <Link
              key={tab.href}
              href={tab.href}
              className={`relative shrink-0 rounded-lg px-4 py-2 text-sm font-medium transition-colors ${
                active
                  ? "bg-teal-600 text-white"
                  : "text-slate-600 hover:bg-slate-100 hover:text-slate-900"
              }`}
            >
              {tab.label}
              {tab.href === "/runs" && count > 0 && (
                <span className="absolute -right-1 -top-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-orange-500 px-1 text-[10px] font-bold text-white">
                  {count}
                </span>
              )}
            </Link>
          );
        })}
      </div>
    </nav>
  );
}
