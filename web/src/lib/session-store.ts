const PREFIX = "agent-orch:";

export function getLastRunId(page: string): string {
  if (typeof window === "undefined") return "";
  return sessionStorage.getItem(`${PREFIX}${page}:runId`) ?? "";
}

export function setLastRunId(page: string, runId: string): void {
  if (typeof window === "undefined") return;
  sessionStorage.setItem(`${PREFIX}${page}:runId`, runId);
}

function sessionIdKey(mode: string): string {
  return `${PREFIX}${mode}-chat:sessionId`;
}

/**
 * Read the last session id for a chat mode. The legacy single-key store
 * (`agent-orch:chat:sessionId`) is migrated to the `general` slot on first
 * read so existing users don't lose their session pointer.
 */
export function getLastSessionId(mode: "general" | "team" = "general"): string | null {
  if (typeof window === "undefined") return null;
  const stored = sessionStorage.getItem(sessionIdKey(mode));
  if (stored) return stored;
  if (mode === "general") {
    const legacy = sessionStorage.getItem(`${PREFIX}chat:sessionId`);
    if (legacy) {
      sessionStorage.setItem(sessionIdKey("general"), legacy);
      sessionStorage.removeItem(`${PREFIX}chat:sessionId`);
      return legacy;
    }
  }
  return null;
}

export function setLastSessionId(
  mode: "general" | "team",
  sid: string | null,
): void {
  if (typeof window === "undefined") return;
  if (sid) {
    sessionStorage.setItem(sessionIdKey(mode), sid);
  } else {
    sessionStorage.removeItem(sessionIdKey(mode));
  }
}

/**
 * Remember the run_id of an in-flight chat run so the SSE stream can be
 * re-established after the user navigates away and returns. Keyed by chat
 * mode (general / team) so the two chat pages don't overwrite each other.
 */
export interface PersistedActiveRun {
  runId: string;
  sessionId: string;
}

function activeRunKey(mode: string): string {
  return `${PREFIX}${mode}-chat:activeRun`;
}

export function getLastActiveRun(mode: string): PersistedActiveRun | null {
  if (typeof window === "undefined") return null;
  const raw = sessionStorage.getItem(activeRunKey(mode));
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as PersistedActiveRun;
    if (!parsed.runId || !parsed.sessionId) return null;
    return parsed;
  } catch {
    return null;
  }
}

export function setLastActiveRun(
  mode: string,
  value: PersistedActiveRun | null,
): void {
  if (typeof window === "undefined") return;
  if (value) {
    sessionStorage.setItem(activeRunKey(mode), JSON.stringify(value));
  } else {
    sessionStorage.removeItem(activeRunKey(mode));
  }
}
