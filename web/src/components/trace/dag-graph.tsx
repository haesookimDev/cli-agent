import type { NodeTraceState, TraceEdge } from "@/lib/types";

const statusFill: Record<string, string> = {
  running: "#60a5fa",
  succeeded: "#10b981",
  failed: "#ef4444",
  pending: "#fbbf24",
  skipped: "#cbd5e1",
  cancelled: "#94a3b8",
};

const NODE_W = 150;
const NODE_H = 54;
const GAP_X = 200;
const GAP_Y = 80;
const PAD = 40;

interface Props {
  nodes: NodeTraceState[];
  edges: TraceEdge[];
  activeNodes: string[];
}

function computeLayers(
  nodes: NodeTraceState[],
): Map<string, number> {
  const depMap = new Map<string, string[]>();
  for (const n of nodes) depMap.set(n.node_id, n.dependencies);

  const layers = new Map<string, number>();
  const visiting = new Set<string>();

  function getLayer(id: string): number {
    if (layers.has(id)) return layers.get(id)!;
    if (visiting.has(id)) return 0; // cycle guard
    visiting.add(id);
    const deps = depMap.get(id) ?? [];
    const layer =
      deps.length === 0 ? 0 : Math.max(...deps.map(getLayer)) + 1;
    visiting.delete(id);
    layers.set(id, layer);
    return layer;
  }

  for (const n of nodes) getLayer(n.node_id);
  return layers;
}

export function DagGraph({ nodes, edges, activeNodes }: Props) {
  if (nodes.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-slate-300 p-6 text-center text-sm text-slate-400">
        No graph nodes
      </div>
    );
  }

  const layerMap = computeLayers(nodes);
  const activeSet = new Set(activeNodes);

  // Group nodes by layer
  const layerGroups = new Map<number, NodeTraceState[]>();
  for (const n of nodes) {
    const l = layerMap.get(n.node_id) ?? 0;
    if (!layerGroups.has(l)) layerGroups.set(l, []);
    layerGroups.get(l)!.push(n);
  }

  const maxLayer = Math.max(...layerMap.values(), 0);
  const maxPerLayer = Math.max(
    ...[...layerGroups.values()].map((g) => g.length),
    1,
  );

  const svgW = (maxLayer + 1) * GAP_X + PAD * 2;
  const svgH = maxPerLayer * GAP_Y + PAD * 2;

  // Compute positions
  const positions = new Map<string, { x: number; y: number }>();
  for (const [layer, group] of layerGroups) {
    const totalH = group.length * GAP_Y;
    const startY = (svgH - totalH) / 2 + GAP_Y / 2;
    group.forEach((n, i) => {
      positions.set(n.node_id, {
        x: PAD + layer * GAP_X,
        y: startY + i * GAP_Y,
      });
    });
  }

  return (
    <div className="overflow-x-auto rounded-lg border border-slate-200 bg-white">
      <svg
        width={svgW}
        height={svgH}
        viewBox={`0 0 ${svgW} ${svgH}`}
        className="min-w-full"
      >
        <defs>
          <marker
            id="arrow"
            viewBox="0 0 10 6"
            refX="10"
            refY="3"
            markerWidth="8"
            markerHeight="6"
            orient="auto"
          >
            <path d="M0,0 L10,3 L0,6 Z" fill="#94a3b8" />
          </marker>
          <style>{`
            @keyframes pulse {
              0%, 100% { stroke-opacity: 1; }
              50% { stroke-opacity: 0.3; }
            }
            .node-active { animation: pulse 1.5s ease-in-out infinite; }
          `}</style>
        </defs>

        {/* Edges */}
        {edges.map((e, i) => {
          const from = positions.get(e.from);
          const to = positions.get(e.to);
          if (!from || !to) return null;
          const x1 = from.x + NODE_W;
          const y1 = from.y + NODE_H / 2;
          const x2 = to.x;
          const y2 = to.y + NODE_H / 2;
          const cx = (x1 + x2) / 2;
          return (
            <path
              key={i}
              d={`M${x1},${y1} C${cx},${y1} ${cx},${y2} ${x2},${y2}`}
              fill="none"
              stroke="#cbd5e1"
              strokeWidth={1.5}
              markerEnd="url(#arrow)"
            />
          );
        })}

        {/* Nodes */}
        {nodes.map((n) => {
          const pos = positions.get(n.node_id);
          if (!pos) return null;
          const fill = statusFill[n.status] ?? "#e2e8f0";
          const isActive = activeSet.has(n.node_id);
          return (
            <g key={n.node_id}>
              <rect
                x={pos.x}
                y={pos.y}
                width={NODE_W}
                height={NODE_H}
                rx={8}
                fill={fill}
                fillOpacity={0.15}
                stroke={fill}
                strokeWidth={isActive ? 2.5 : 1.5}
                className={isActive ? "node-active" : ""}
              />
              <text
                x={pos.x + NODE_W / 2}
                y={pos.y + 18}
                textAnchor="middle"
                fontSize={12}
                fontWeight={600}
                fill="#334155"
              >
                {n.node_id}
              </text>
              {n.role && (
                <text
                  x={pos.x + NODE_W / 2}
                  y={pos.y + 32}
                  textAnchor="middle"
                  fontSize={10}
                  fill="#64748b"
                >
                  {n.role}
                </text>
              )}
              {n.model && (
                <text
                  x={pos.x + NODE_W / 2}
                  y={pos.y + 44}
                  textAnchor="middle"
                  fontSize={9}
                  fill="#94a3b8"
                >
                  {n.model.length > 20
                    ? n.model.slice(0, 20) + "..."
                    : n.model}
                </text>
              )}
              <title>
                {`${n.node_id} (${n.status})${n.duration_ms != null ? ` — ${n.duration_ms}ms` : ""}${n.retries > 0 ? ` — retries: ${n.retries}` : ""}${n.dependencies.length > 0 ? `\ndeps: ${n.dependencies.join(", ")}` : ""}`}
              </title>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
