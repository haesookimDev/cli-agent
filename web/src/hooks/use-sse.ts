"use client";

import { useEffect, useRef, useCallback, useState } from "react";
import type { RunActionEvent } from "@/lib/types";

export function useRunSSE(runId: string | null) {
  const [events, setEvents] = useState<RunActionEvent[]>([]);
  const [terminalStatus, setTerminalStatus] = useState<string | null>(null);
  const [sseError, setSseError] = useState<string | null>(null);
  const controllerRef = useRef<AbortController | null>(null);

  const stop = useCallback(() => {
    controllerRef.current?.abort();
    controllerRef.current = null;
  }, []);

  useEffect(() => {
    if (!runId) return;
    setEvents([]);
    setTerminalStatus(null);
    setSseError(null);

    const controller = new AbortController();
    controllerRef.current = controller;

    (async () => {
      try {
        const resp = await fetch(
          `/api/stream/${runId}?poll_ms=400&behavior=false`,
          {
            signal: controller.signal,
            headers: { Accept: "text/event-stream" },
          },
        );

        if (!resp.ok || !resp.body) {
          const text = await resp.text().catch(() => "");
          setSseError(`Stream failed: ${resp.status} ${text}`);
          return;
        }

        const reader = resp.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";

        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });

          const parts = buffer.split("\n\n");
          buffer = parts.pop() ?? "";

          for (const part of parts) {
            let eventType = "message";
            let data = "";
            for (const line of part.split("\n")) {
              if (line.startsWith("event:"))
                eventType = line.slice(6).trim();
              else if (line.startsWith("data:"))
                data = line.slice(5).trim();
            }
            if (!data) continue;

            if (eventType === "action_event") {
              const ev = JSON.parse(data) as RunActionEvent;
              setEvents((prev) => {
                // Server emits events in seq order, so the common case is
                // append. Avoid a full O(n log n) sort on every chunk by
                // binary-inserting only when the new event is out of order.
                const last = prev.length === 0 ? null : prev[prev.length - 1];
                if (last == null || ev.seq >= last.seq) {
                  return [...prev, ev];
                }
                let lo = 0;
                let hi = prev.length;
                while (lo < hi) {
                  const mid = (lo + hi) >>> 1;
                  if (prev[mid].seq <= ev.seq) lo = mid + 1;
                  else hi = mid;
                }
                const next = prev.slice();
                next.splice(lo, 0, ev);
                return next;
              });
            } else if (eventType === "run_terminal") {
              const ev = JSON.parse(data);
              setTerminalStatus(ev.status);
            } else if (eventType === "error") {
              setSseError(data);
            }
          }
        }
      } catch (err) {
        if ((err as Error).name !== "AbortError") {
          setSseError((err as Error).message);
        }
      }
    })();

    return stop;
  }, [runId, stop]);

  return { events, terminalStatus, sseError, stop };
}
