"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { useParams, useRouter } from "next/navigation";
import Link from "next/link";
import {
  Background,
  Controls,
  ReactFlow,
  ReactFlowProvider,
  addEdge,
  useEdgesState,
  useNodesState,
  type Connection,
  type Edge,
  type Node,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import { apiGet, apiPost } from "@/lib/api-client";
import type {
  AgentRole,
  WorkflowNodeTemplate,
  WorkflowParameter,
  WorkflowTemplate,
} from "@/lib/types";

const ROLES: AgentRole[] = [
  "planner",
  "extractor",
  "coder",
  "summarizer",
  "fallback",
  "tool_caller",
  "analyzer",
  "reviewer",
  "scheduler",
  "config_manager",
  "validator",
];

const ROLE_COLOR: Record<string, string> = {
  planner: "#3b82f6",
  extractor: "#f59e0b",
  coder: "#a855f7",
  summarizer: "#10b981",
  fallback: "#ef4444",
  tool_caller: "#fb923c",
  analyzer: "#06b6d4",
  reviewer: "#ec4899",
  scheduler: "#6366f1",
  config_manager: "#84cc16",
  validator: "#f43f5e",
};

interface EditorNodeData extends Record<string, unknown> {
  id: string;
  role: AgentRole;
  instructions: string;
  mcpTools: string[];
}

function autoLayout(nodes: WorkflowNodeTemplate[]): Map<string, { x: number; y: number }> {
  // Layered DAG layout — simple BFS depth assignment + per-layer y stacking.
  const depMap = new Map<string, string[]>();
  for (const n of nodes) depMap.set(n.id, n.dependencies);
  const depth = new Map<string, number>();
  const visiting = new Set<string>();
  function getDepth(id: string): number {
    if (depth.has(id)) return depth.get(id)!;
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const deps = depMap.get(id) ?? [];
    const d = deps.length === 0 ? 0 : Math.max(...deps.map(getDepth)) + 1;
    visiting.delete(id);
    depth.set(id, d);
    return d;
  }
  for (const n of nodes) getDepth(n.id);
  const layers = new Map<number, string[]>();
  for (const [id, d] of depth) {
    if (!layers.has(d)) layers.set(d, []);
    layers.get(d)!.push(id);
  }
  const positions = new Map<string, { x: number; y: number }>();
  for (const [d, ids] of layers) {
    ids.sort();
    ids.forEach((id, i) =>
      positions.set(id, { x: 80 + d * 240, y: 80 + i * 110 }),
    );
  }
  return positions;
}

function templateToFlow(
  template: WorkflowTemplate,
): { nodes: Node<EditorNodeData>[]; edges: Edge[] } {
  const positions = autoLayout(template.graph_template.nodes);
  const nodes: Node<EditorNodeData>[] = template.graph_template.nodes.map(
    (n) => ({
      id: n.id,
      type: "default",
      position: positions.get(n.id) ?? { x: 0, y: 0 },
      data: {
        id: n.id,
        role: n.role,
        instructions: n.instructions,
        mcpTools: n.mcp_tools,
      },
      style: {
        background: "#fff",
        border: `2px solid ${ROLE_COLOR[n.role] ?? "#94a3b8"}`,
        borderRadius: 8,
        padding: 8,
        minWidth: 160,
      },
      label: `${n.id} · ${n.role}`,
    }),
  );
  const edges: Edge[] = template.graph_template.nodes.flatMap((n) =>
    n.dependencies.map((dep) => ({
      id: `${dep}->${n.id}`,
      source: dep,
      target: n.id,
      style: { stroke: "#94a3b8" },
    })),
  );
  // react-flow uses `data.label` from node, push the label string in there
  for (const node of nodes) {
    (node as unknown as { data: Record<string, unknown> }).data.label = `${node.data.id} · ${node.data.role}`;
  }
  return { nodes, edges };
}

function flowToTemplate(
  source: WorkflowTemplate,
  nodes: Node<EditorNodeData>[],
  edges: Edge[],
  newName: string,
  newDescription: string,
): WorkflowTemplate {
  const depsByTarget = new Map<string, string[]>();
  for (const e of edges) {
    if (!depsByTarget.has(e.target)) depsByTarget.set(e.target, []);
    depsByTarget.get(e.target)!.push(e.source);
  }
  const nodesOut: WorkflowNodeTemplate[] = nodes.map((n) => ({
    id: n.data.id,
    role: n.data.role,
    instructions: n.data.instructions,
    dependencies: depsByTarget.get(n.id) ?? [],
    mcp_tools: n.data.mcpTools,
    git_commands: [],
    policy: {},
  }));
  return {
    ...source,
    name: newName,
    description: newDescription,
    graph_template: { nodes: nodesOut },
  };
}

function EditorInner({ template }: { template: WorkflowTemplate }) {
  const router = useRouter();
  const initial = useMemo(() => templateToFlow(template), [template]);
  const [nodes, setNodes, onNodesChange] = useNodesState<Node<EditorNodeData>>(
    initial.nodes,
  );
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>(initial.edges);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [name, setName] = useState(`${template.name} (copy)`);
  const [description, setDescription] = useState(template.description);
  const [params, setParams] = useState<WorkflowParameter[]>(template.parameters);
  const [saving, setSaving] = useState(false);

  const onConnect = useCallback(
    (c: Connection) => {
      if (c.source && c.target && c.source !== c.target) {
        setEdges((eds) =>
          addEdge(
            {
              ...c,
              id: `${c.source}->${c.target}`,
              style: { stroke: "#94a3b8" },
            },
            eds,
          ),
        );
      }
    },
    [setEdges],
  );

  const selectedNode = nodes.find((n) => n.id === selectedId) ?? null;

  function addNode(role: AgentRole) {
    const id = `${role}_${Date.now().toString(36).slice(-4)}`;
    setNodes((ns) => [
      ...ns,
      {
        id,
        type: "default",
        position: { x: 120 + ns.length * 30, y: 320 },
        data: {
          id,
          role,
          instructions: `${role} instructions`,
          mcpTools: [],
          label: `${id} · ${role}`,
        },
        style: {
          background: "#fff",
          border: `2px solid ${ROLE_COLOR[role] ?? "#94a3b8"}`,
          borderRadius: 8,
          padding: 8,
          minWidth: 160,
        },
      } as Node<EditorNodeData>,
    ]);
    setSelectedId(id);
  }

  function deleteSelected() {
    if (!selectedId) return;
    setNodes((ns) => ns.filter((n) => n.id !== selectedId));
    setEdges((es) =>
      es.filter((e) => e.source !== selectedId && e.target !== selectedId),
    );
    setSelectedId(null);
  }

  function updateSelected(patch: Partial<EditorNodeData>) {
    if (!selectedId) return;
    setNodes((ns) =>
      ns.map((n) => {
        if (n.id !== selectedId) return n;
        const next: Node<EditorNodeData> = {
          ...n,
          data: {
            ...n.data,
            ...patch,
            label: `${patch.id ?? n.data.id} · ${patch.role ?? n.data.role}`,
          },
          style: {
            ...n.style,
            border: `2px solid ${
              ROLE_COLOR[patch.role ?? n.data.role] ?? "#94a3b8"
            }`,
          },
        };
        return next;
      }),
    );
  }

  async function handleSave() {
    setSaving(true);
    try {
      const next = flowToTemplate(
        { ...template, parameters: params },
        nodes,
        edges,
        name,
        description,
      );
      const created = await apiPost<WorkflowTemplate>("/v1/workflows", {
        name: next.name,
        description: next.description,
        graph_template: next.graph_template,
        parameters: next.parameters,
      });
      router.push(`/workflows/${created.id}`);
    } catch (err) {
      console.error("save workflow:", err);
      alert("Failed to save: " + (err as Error).message);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="grid h-[calc(100vh-180px)] grid-cols-[1fr_320px] gap-4">
      {/* Canvas */}
      <div className="rounded-xl border border-slate-200 bg-white">
        <ReactFlow
          nodes={nodes.map((n) => ({
            ...n,
            data: { ...n.data, label: n.data.label ?? `${n.data.id} · ${n.data.role}` },
          }))}
          edges={edges}
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          onConnect={onConnect}
          onNodeClick={(_, n) => setSelectedId(n.id)}
          fitView
        >
          <Background />
          <Controls />
        </ReactFlow>
      </div>

      {/* Sidebar */}
      <div className="space-y-4 overflow-y-auto rounded-xl border border-slate-200 bg-white p-4 text-xs">
        <div>
          <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
            Workflow
          </h3>
          <label className="mb-1 block text-slate-500">Name</label>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            className="mb-2 w-full rounded border border-slate-200 px-2 py-1 text-xs"
          />
          <label className="mb-1 block text-slate-500">Description</label>
          <textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={2}
            className="w-full rounded border border-slate-200 px-2 py-1 text-xs"
          />
        </div>

        <div>
          <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
            Add Node
          </h3>
          <div className="grid grid-cols-2 gap-1">
            {ROLES.map((r) => (
              <button
                key={r}
                onClick={() => addNode(r)}
                className="rounded border border-slate-200 px-2 py-1 text-left text-[11px] text-slate-600 hover:bg-slate-50"
                style={{ borderLeft: `4px solid ${ROLE_COLOR[r]}` }}
              >
                {r}
              </button>
            ))}
          </div>
        </div>

        {selectedNode ? (
          <div>
            <div className="mb-2 flex items-center justify-between">
              <h3 className="text-[11px] font-semibold uppercase tracking-wider text-slate-400">
                Node
              </h3>
              <button
                onClick={deleteSelected}
                className="text-[11px] text-red-500 hover:underline"
              >
                Delete
              </button>
            </div>
            <label className="mb-1 block text-slate-500">ID</label>
            <input
              value={selectedNode.data.id}
              onChange={(e) => updateSelected({ id: e.target.value })}
              className="mb-2 w-full rounded border border-slate-200 px-2 py-1 font-mono text-xs"
            />
            <label className="mb-1 block text-slate-500">Role</label>
            <select
              value={selectedNode.data.role}
              onChange={(e) =>
                updateSelected({ role: e.target.value as AgentRole })
              }
              className="mb-2 w-full rounded border border-slate-200 px-2 py-1 text-xs"
            >
              {ROLES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
            <label className="mb-1 block text-slate-500">Instructions</label>
            <textarea
              value={selectedNode.data.instructions}
              onChange={(e) => updateSelected({ instructions: e.target.value })}
              rows={5}
              className="mb-2 w-full rounded border border-slate-200 px-2 py-1 text-xs"
            />
            <label className="mb-1 block text-slate-500">
              MCP tools (comma-separated)
            </label>
            <input
              value={selectedNode.data.mcpTools.join(", ")}
              onChange={(e) =>
                updateSelected({
                  mcpTools: e.target.value
                    .split(",")
                    .map((s) => s.trim())
                    .filter(Boolean),
                })
              }
              className="w-full rounded border border-slate-200 px-2 py-1 font-mono text-[11px]"
            />
          </div>
        ) : (
          <div className="rounded-md border border-dashed border-slate-200 p-3 text-center text-[11px] text-slate-400">
            Click a node to edit
          </div>
        )}

        <div>
          <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
            Parameters
          </h3>
          {params.length === 0 ? (
            <div className="text-[11px] text-slate-400">None</div>
          ) : (
            <ul className="space-y-1">
              {params.map((p, i) => (
                <li key={i} className="flex items-center gap-2">
                  <span className="font-mono text-[11px] text-slate-700">
                    {p.name}
                  </span>
                  {p.default_value && (
                    <span className="font-mono text-[10px] text-slate-400">
                      = {p.default_value}
                    </span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="border-t border-slate-100 pt-3">
          <button
            onClick={handleSave}
            disabled={saving}
            className="w-full rounded-md bg-teal-600 px-4 py-2 text-xs font-medium text-white hover:bg-teal-700 disabled:opacity-50"
          >
            {saving ? "Saving..." : "Save as new workflow"}
          </button>
        </div>
      </div>
    </div>
  );
}

export default function WorkflowEditPage() {
  const { id } = useParams<{ id: string }>();
  const [template, setTemplate] = useState<WorkflowTemplate | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!id) return;
    apiGet<WorkflowTemplate>(`/v1/workflows/${id}`)
      .then(setTemplate)
      .catch((e) => setError((e as Error).message));
  }, [id]);

  if (error) {
    return (
      <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
        {error}
      </div>
    );
  }
  if (!template) {
    return (
      <div className="p-6 text-center text-sm text-slate-400">Loading…</div>
    );
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2 text-xs">
        <Link href="/workflows" className="text-teal-600 hover:underline">
          Workflows
        </Link>
        <span className="text-slate-300">/</span>
        <Link
          href={`/workflows/${template.id}`}
          className="text-teal-600 hover:underline"
        >
          {template.name}
        </Link>
        <span className="text-slate-300">/</span>
        <span className="text-slate-600">edit</span>
      </div>
      <ReactFlowProvider>
        <EditorInner template={template} />
      </ReactFlowProvider>
    </div>
  );
}
