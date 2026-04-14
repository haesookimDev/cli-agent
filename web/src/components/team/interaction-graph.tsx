"use client";

import { GitHubActivityItem } from "@/lib/types";

interface InteractionGraphProps {
  activities: GitHubActivityItem[];
  personas: string[];
}

interface InteractionEdge {
  from: string;
  to: string;
  count: number;
  types: string[];
}

function buildInteractions(
  activities: GitHubActivityItem[],
  personas: string[]
): InteractionEdge[] {
  const edgeMap = new Map<string, InteractionEdge>();

  // Group sequential activities to infer interactions.
  // e.g. PM creates issue, then Tech Lead comments -> PM -> Tech Lead edge
  for (let i = 0; i < activities.length - 1; i++) {
    const a = activities[i];
    const b = activities[i + 1];
    if (
      a.persona_name !== b.persona_name &&
      a.target_number === b.target_number &&
      a.target_number !== null
    ) {
      const key = `${a.persona_name}->${b.persona_name}`;
      const existing = edgeMap.get(key);
      if (existing) {
        existing.count++;
        if (!existing.types.includes(b.activity_type)) {
          existing.types.push(b.activity_type);
        }
      } else {
        edgeMap.set(key, {
          from: a.persona_name,
          to: b.persona_name,
          count: 1,
          types: [b.activity_type],
        });
      }
    }
  }

  return Array.from(edgeMap.values());
}

const COLORS = [
  "#60a5fa",
  "#34d399",
  "#fbbf24",
  "#f87171",
  "#a78bfa",
  "#fb923c",
  "#2dd4bf",
];

export default function InteractionGraph({
  activities,
  personas,
}: InteractionGraphProps) {
  const interactions = buildInteractions(activities, personas);
  const width = 600;
  const height = 400;
  const cx = width / 2;
  const cy = height / 2;
  const radius = 150;

  // Position personas in a circle
  const positions = personas.map((name, i) => {
    const angle = (2 * Math.PI * i) / personas.length - Math.PI / 2;
    return {
      name,
      x: cx + radius * Math.cos(angle),
      y: cy + radius * Math.sin(angle),
      color: COLORS[i % COLORS.length],
    };
  });

  const posMap = new Map(positions.map((p) => [p.name, p]));

  if (personas.length === 0) {
    return (
      <div className="text-gray-500 text-sm text-center py-8">
        No team interactions to display.
      </div>
    );
  }

  return (
    <svg viewBox={`0 0 ${width} ${height}`} className="w-full max-w-2xl mx-auto">
      {/* Edges */}
      {interactions.map((edge, i) => {
        const from = posMap.get(edge.from);
        const to = posMap.get(edge.to);
        if (!from || !to) return null;
        const strokeWidth = Math.min(1 + edge.count * 0.8, 5);
        return (
          <line
            key={i}
            x1={from.x}
            y1={from.y}
            x2={to.x}
            y2={to.y}
            stroke="#475569"
            strokeWidth={strokeWidth}
            strokeOpacity={0.6}
            markerEnd="url(#arrowhead)"
          />
        );
      })}

      {/* Arrow marker */}
      <defs>
        <marker
          id="arrowhead"
          markerWidth="8"
          markerHeight="6"
          refX="8"
          refY="3"
          orient="auto"
        >
          <polygon points="0 0, 8 3, 0 6" fill="#475569" />
        </marker>
      </defs>

      {/* Nodes */}
      {positions.map((p) => (
        <g key={p.name}>
          <circle
            cx={p.x}
            cy={p.y}
            r={28}
            fill={p.color}
            fillOpacity={0.15}
            stroke={p.color}
            strokeWidth={2}
          />
          <text
            x={p.x}
            y={p.y - 2}
            textAnchor="middle"
            fill={p.color}
            fontSize={12}
            fontWeight="bold"
          >
            {p.name.charAt(0)}
          </text>
          <text
            x={p.x}
            y={p.y + 44}
            textAnchor="middle"
            fill="#9ca3af"
            fontSize={10}
          >
            {p.name}
          </text>
        </g>
      ))}
    </svg>
  );
}
