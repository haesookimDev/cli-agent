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
