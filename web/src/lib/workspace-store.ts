/**
 * Active workspace pointer. Survives reloads via IndexedDB so a chat that
 * opened workspace A picks back up in A after a navigation away.
 */
import { get, set, del } from "idb-keyval";

const KEY_BY_KIND = (kind: "general" | "team") =>
  `workspace-store:${kind}:active`;

export async function getActiveWorkspaceId(
  kind: "general" | "team",
): Promise<string | null> {
  if (typeof window === "undefined") return null;
  try {
    const v = await get<string>(KEY_BY_KIND(kind));
    return v ?? null;
  } catch {
    return null;
  }
}

export async function setActiveWorkspaceId(
  kind: "general" | "team",
  id: string | null,
): Promise<void> {
  if (typeof window === "undefined") return;
  try {
    if (id) await set(KEY_BY_KIND(kind), id);
    else await del(KEY_BY_KIND(kind));
  } catch {
    // ignore
  }
}
