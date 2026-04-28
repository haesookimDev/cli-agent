"use client";

import { useMemo } from "react";
import type { NodeTraceState, RunActionEvent, TraceEdge } from "@/lib/types";

/* ------------------------------------------------------------------ */
/*  Hierarchy derivation                                               */
/* ------------------------------------------------------------------ */

interface TreeNode {
  state: NodeTraceState;
  /// "static" = present in the original graph; "dynamic" = injected via
  /// SubtaskPlan after the run started.
  origin: "static" | "dynamic";
  /// Parent that spawned this node. For dynamic nodes that's the node whose
  /// completion produced the SubtaskPlan; for static nodes it's null (they
  /// hang off the root level).
  parentId: string | null;
  children: TreeNode[];
}

/**
 * Build a tree where each layer represents one round of dynamic
 * subtask spawning. Static nodes are top-level; nodes added via
 * `dynamic_node_added` events become children of the node they
 * spawned from.
 */
function buildSubtaskTree(
  nodes: NodeTraceState[],
  edges: TraceEdge[],
  events: RunActionEvent[],
): TreeNode[] {
  const byId = new Map<string, TreeNode>();

  for (const n of nodes) {
    byId.set(n.node_id, {
      state: n,
      origin: "static",
      parentId: null,
      children: [],
    });
  }

  // Walk events in order so dynamic spawns get attributed to whichever node
  // produced them. The orchestrator emits dynamic_node_added with a
  // `from` field naming the parent; fall back to dependencies if missing.
  for (const ev of events) {
    if (ev.action !== "dynamic_node_added") continue;
    const p = ev.payload as Record<string, unknown>;
    const nodeId =
      (p.node_id as string | undefined) ?? (ev.actor_id ?? undefined);
    if (!nodeId) continue;
    const fromNode = (p.from as string | undefined) ?? null;
    let entry = byId.get(nodeId);
    if (entry) {
      entry.origin = "dynamic";
      entry.parentId = fromNode ?? entry.parentId;
    } else {
      // Node not yet in the static graph snapshot — synthesize a placeholder.
      const placeholder: NodeTraceState = {
        node_id: nodeId,
        role: null,
        status: "pending",
        started_at: null,
        finished_at: null,
        duration_ms: null,
        retries: 0,
        model: null,
        dependencies: [],
      };
      entry = {
        state: placeholder,
        origin: "dynamic",
        parentId: fromNode,
        children: [],
      };
      byId.set(nodeId, entry);
    }
  }

  // For dynamic nodes whose `from` is unknown, fall back to the first
  // dependency that exists in the tree.
  for (const node of byId.values()) {
    if (node.origin === "dynamic" && !node.parentId) {
      const dep = node.state.dependencies.find((d) => byId.has(d));
      if (dep) node.parentId = dep;
    }
  }

  // Wire children pointers and gather roots.
  const roots: TreeNode[] = [];
  for (const node of byId.values()) {
    if (node.parentId && byId.has(node.parentId)) {
      byId.get(node.parentId)!.children.push(node);
    } else {
      roots.push(node);
    }
  }

  // Stable order: by node_id within each level so the layout doesn't jump
  // between renders. Edges are unused today but kept on the API in case a
  // future iteration wants to draw cross-tree dependency lines.
  const sortRecursive = (n: TreeNode) => {
    n.children.sort((a, b) => a.state.node_id.localeCompare(b.state.node_id));
    n.children.forEach(sortRecursive);
  };
  roots.sort((a, b) => a.state.node_id.localeCompare(b.state.node_id));
  roots.forEach(sortRecursive);
  void edges;
  return roots;
}

/* ------------------------------------------------------------------ */
/*  Rendering                                                          */
/* ------------------------------------------------------------------ */

const statusColor: Record<string, string> = {
  running: "text-blue-600",
  succeeded: "text-emerald-600",
  failed: "text-red-600",
  pending: "text-amber-600",
  skipped: "text-slate-400",
  cancelled: "text-slate-400",
};

interface Props {
  nodes: NodeTraceState[];
  edges: TraceEdge[];
  events: RunActionEvent[];
}

export function SubtaskTree({ nodes, edges, events }: Props) {
  const roots = useMemo(
    () => buildSubtaskTree(nodes, edges, events),
    [nodes, edges, events],
  );

  if (roots.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-slate-300 p-6 text-center text-sm text-slate-400">
        No subtasks yet
      </div>
    );
  }

  return (
    <ul className="space-y-1.5 text-sm">
      {roots.map((root) => (
        <SubtaskRow key={root.state.node_id} node={root} depth={0} />
      ))}
    </ul>
  );
}

function SubtaskRow({ node, depth }: { node: TreeNode; depth: number }) {
  const indent = depth * 16;
  const dynamicTag =
    node.origin === "dynamic" ? (
      <span className="rounded bg-purple-100 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wider text-purple-700">
        sub
      </span>
    ) : null;

  return (
    <li>
      <div
        className="flex items-center gap-2 rounded-md border border-slate-100 bg-white px-2 py-1.5"
        style={{ marginLeft: indent }}
      >
        {dynamicTag}
        <span className="font-mono text-xs text-slate-700">
          {node.state.node_id}
        </span>
        {node.state.role && (
          <span className="text-[11px] text-slate-500">{node.state.role}</span>
        )}
        <span
          className={`ml-auto text-[11px] font-medium ${statusColor[node.state.status] ?? "text-slate-500"}`}
        >
          {node.state.status}
        </span>
        {node.state.duration_ms != null && (
          <span className="font-mono text-[10px] text-slate-400">
            {node.state.duration_ms}ms
          </span>
        )}
      </div>
      {node.children.length > 0 && (
        <ul className="mt-1 space-y-1.5">
          {node.children.map((child) => (
            <SubtaskRow
              key={child.state.node_id}
              node={child}
              depth={depth + 1}
            />
          ))}
        </ul>
      )}
    </li>
  );
}
