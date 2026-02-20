"use client";

import { useState, useCallback } from "react";
import { apiGet } from "@/lib/api-client";
import { useInterval } from "@/hooks/use-interval";
import type { RunRecord } from "@/lib/types";

export function useActiveRuns(pollMs: number = 3000) {
  const [activeRuns, setActiveRuns] = useState<RunRecord[]>([]);

  const poll = useCallback(async () => {
    try {
      const runs = await apiGet<RunRecord[]>("/v1/runs/active");
      setActiveRuns(runs);
    } catch {
      // Ignore polling errors silently
    }
  }, []);

  useInterval(poll, pollMs);

  return { activeRuns, count: activeRuns.length };
}
