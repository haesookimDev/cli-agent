const PREFIX = "agent-orch:";

export function getLastRunId(page: string): string {
  if (typeof window === "undefined") return "";
  return sessionStorage.getItem(`${PREFIX}${page}:runId`) ?? "";
}

export function setLastRunId(page: string, runId: string): void {
  if (typeof window === "undefined") return;
  sessionStorage.setItem(`${PREFIX}${page}:runId`, runId);
}

export function getLastSessionId(): string | null {
  if (typeof window === "undefined") return null;
  return sessionStorage.getItem(`${PREFIX}chat:sessionId`) || null;
}

export function setLastSessionId(sid: string | null): void {
  if (typeof window === "undefined") return;
  if (sid) {
    sessionStorage.setItem(`${PREFIX}chat:sessionId`, sid);
  } else {
    sessionStorage.removeItem(`${PREFIX}chat:sessionId`);
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
