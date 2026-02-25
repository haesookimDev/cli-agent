import type { Metadata } from "next";
import "./globals.css";
import { Nav } from "@/components/nav";

export const metadata: Metadata = {
  title: "Agent Orchestrator",
  description: "Multi-agent orchestration dashboard",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body className="h-screen overflow-hidden bg-stone-100 text-slate-900 antialiased">
        <div className="mx-auto flex h-full max-w-6xl flex-col px-4 py-4">
          <header className="mb-4 flex items-center justify-between rounded-2xl border border-black/10 bg-white/80 px-5 py-3 backdrop-blur">
            <h1 className="text-xl font-bold tracking-tight">
              Agent Orchestrator
            </h1>
            <span className="text-xs text-slate-400">v0.1.0</span>
          </header>
          <Nav />
          <main className="min-h-0 flex-1 overflow-y-auto pb-4">
            {children}
          </main>
        </div>
      </body>
    </html>
  );
}
