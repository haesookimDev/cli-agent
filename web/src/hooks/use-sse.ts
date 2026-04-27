"use client";

import { useEffect, useRef, useCallback, useState } from "react";
import type { RunActionEvent } from "@/lib/types";

export type SseConnectionState = "connecting" | "open" | "reconnecting" | "closed";

export function useRunSSE(runId: string | null) {
  const [events, setEvents] = useState<RunActionEvent[]>([]);
  const [terminalStatus, setTerminalStatus] = useState<string | null>(null);
  const [sseError, setSseError] = useState<string | null>(null);
  const [connectionState, setConnectionState] =
    useState<SseConnectionState>("closed");
  const controllerRef = useRef<AbortController | null>(null);

  const stop = useCallback(() => {
    controllerRef.current?.abort();
    controllerRef.current = null;
    setConnectionState("closed");
  }, []);

  useEffect(() => {
    if (!runId) return;
    setEvents([]);
    setTerminalStatus(null);
    setSseError(null);
    setConnectionState("connecting");

    let cancelled = false;
    let lastSeq = 0;
    let attempt = 0;

    const run = async () => {
      while (!cancelled) {
        const controller = new AbortController();
        controllerRef.current = controller;
        try {
          const url = new URL(`/api/stream/${runId}`, window.location.origin);
          url.searchParams.set("poll_ms", "400");
          url.searchParams.set("behavior", "false");
          if (lastSeq > 0) {
            url.searchParams.set("after_seq", String(lastSeq));
          }
          const resp = await fetch(url.pathname + url.search, {
            signal: controller.signal,
            headers: { Accept: "text/event-stream" },
          });

          if (!resp.ok || !resp.body) {
            const text = await resp.text().catch(() => "");
            throw new Error(`stream failed: ${resp.status} ${text}`);
          }

          attempt = 0;
          setConnectionState("open");
          setSseError(null);

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
                if (ev.seq > lastSeq) lastSeq = ev.seq;
                setEvents((prev) => {
                  // Server emits events in seq order, so the common case is
                  // append. Avoid a full O(n log n) sort on every chunk by
                  // binary-inserting only when the new event is out of order.
                  const last = prev.length === 0 ? null : prev[prev.length - 1];
                  if (last == null || ev.seq > last.seq) {
                    return [...prev, ev];
                  }
                  if (ev.seq === last.seq) {
                    return prev;
                  }
                  let lo = 0;
                  let hi = prev.length;
                  while (lo < hi) {
                    const mid = (lo + hi) >>> 1;
                    if (prev[mid].seq <= ev.seq) lo = mid + 1;
                    else hi = mid;
                  }
                  // Skip exact duplicates by seq.
                  if (lo > 0 && prev[lo - 1].seq === ev.seq) return prev;
                  const next = prev.slice();
                  next.splice(lo, 0, ev);
                  return next;
                });
              } else if (eventType === "run_terminal") {
                const ev = JSON.parse(data);
                setTerminalStatus(ev.status);
                cancelled = true;
              } else if (eventType === "error") {
                setSseError(data);
              }
            }
          }
          // Stream ended cleanly. If the run isn't terminal yet, reconnect.
          if (!cancelled) {
            setConnectionState("reconnecting");
          }
        } catch (err) {
          if ((err as Error).name === "AbortError" || cancelled) break;
          setConnectionState("reconnecting");
          setSseError((err as Error).message);
        }

        if (cancelled) break;
        // Exponential backoff: 500ms, 1s, 2s, 4s, capped at 8s.
        attempt = Math.min(attempt + 1, 5);
        const delayMs = 250 * 2 ** attempt;
        await new Promise((r) => setTimeout(r, delayMs));
      }
      setConnectionState("closed");
    };

    void run();

    return () => {
      cancelled = true;
      controllerRef.current?.abort();
      controllerRef.current = null;
    };
  }, [runId]);

  return { events, terminalStatus, sseError, connectionState, stop };
}
