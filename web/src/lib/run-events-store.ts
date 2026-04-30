/**
 * IndexedDB cache for `runEventsMap`. Without this the chat timeline goes
 * blank for a few seconds whenever the user navigates away and back, since
 * SSE state is React-only and re-fetching `/v1/runs/:id/trace` for every
 * historical run is slow. Keyed by chat mode so general/team don't share
 * a namespace.
 */
import { get, set, del, keys } from "idb-keyval";
import type { RunActionEvent } from "@/lib/types";

interface CachedEntry {
  events: RunActionEvent[];
  savedAt: number;
}

const TTL_MS = 1000 * 60 * 60 * 24 * 7; // 7 days

function entryKey(mode: string, runId: string): string {
  return `${mode}:run-events:${runId}`;
}

export async function loadRunEvents(
  mode: string,
  runId: string,
): Promise<RunActionEvent[] | null> {
  if (typeof window === "undefined") return null;
  try {
    const entry = await get<CachedEntry>(entryKey(mode, runId));
    if (!entry) return null;
    if (Date.now() - entry.savedAt > TTL_MS) {
      await del(entryKey(mode, runId));
      return null;
    }
    return entry.events;
  } catch {
    return null;
  }
}

export async function saveRunEvents(
  mode: string,
  runId: string,
  events: RunActionEvent[],
): Promise<void> {
  if (typeof window === "undefined") return;
  try {
    const entry: CachedEntry = { events, savedAt: Date.now() };
    await set(entryKey(mode, runId), entry);
  } catch {
    // Quota errors are non-fatal — the data is also re-fetchable from
    // /v1/runs/:id/trace as a fallback.
  }
}

export async function deleteRunEvents(
  mode: string,
  runId: string,
): Promise<void> {
  if (typeof window === "undefined") return;
  try {
    await del(entryKey(mode, runId));
  } catch {
    // ignore
  }
}

/**
 * Best-effort GC. Call once at app start (or on a long-running tab) to
 * drop entries older than `beforeMs`.
 */
export async function pruneOldRunEvents(
  mode: string,
  beforeMs: number = Date.now() - TTL_MS,
): Promise<void> {
  if (typeof window === "undefined") return;
  try {
    const all = await keys();
    const prefix = `${mode}:run-events:`;
    for (const k of all) {
      if (typeof k !== "string" || !k.startsWith(prefix)) continue;
      const entry = await get<CachedEntry>(k);
      if (!entry || entry.savedAt < beforeMs) {
        await del(k);
      }
    }
  } catch {
    // ignore
  }
}
