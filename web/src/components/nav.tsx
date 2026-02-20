"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

const tabs = [
  { href: "/", label: "Runner" },
  { href: "/sessions", label: "Sessions" },
  { href: "/runs", label: "Runs" },
  { href: "/behavior", label: "Behavior" },
  { href: "/results", label: "Results" },
  { href: "/trace", label: "Trace" },
];

export function Nav() {
  const pathname = usePathname();

  return (
    <nav className="mb-4 flex gap-1 rounded-xl border border-black/10 bg-white/60 p-1 backdrop-blur">
      {tabs.map((tab) => {
        const active =
          tab.href === "/"
            ? pathname === "/"
            : pathname.startsWith(tab.href);
        return (
          <Link
            key={tab.href}
            href={tab.href}
            className={`rounded-lg px-4 py-2 text-sm font-medium transition-colors ${
              active
                ? "bg-teal-600 text-white"
                : "text-slate-600 hover:bg-slate-100 hover:text-slate-900"
            }`}
          >
            {tab.label}
          </Link>
        );
      })}
    </nav>
  );
}
