export type SyntheticAtlasEventOptions = { startSequence?: number; startIndex?: number; runId?: string; actorPrefix?: string };

export function syntheticAtlasEvents(stepCount = 240, options: SyntheticAtlasEventOptions = {}): Record<string, unknown>[] {
  const actors = ["synthetic-collection", "synthetic-analysis", "synthetic-review"];
  const startSequence = options.startSequence ?? 1;
  const startIndex = options.startIndex ?? 0;
  const events: Record<string, unknown>[] = [];
  for (let offset = 0; offset < stepCount; offset += 1) {
    const index = startIndex + offset;
    const actor = options.actorPrefix ? `${options.actorPrefix}-${actors[index % actors.length]}` : actors[index % actors.length];
    const segment = Math.floor(index / 40);
    const base = {
      run_id: options.runId ?? "synthetic-atlas-run", actor, segment, harness: "fixture", session_id: `synthetic-session-${segment}`,
      source_event_id: `synthetic-call-${index}`, parent_source_event_id: index ? `synthetic-call-${index - 1}` : null,
      metadata: { label: `Synthetic retained step ${String(index + 1).padStart(3, "0")}`, tool: "synthetic_tool", synthetic: true },
    };
    events.push({ ...base, sequence: startSequence + offset * 2, event_id: `synthetic-event-${index}-start`, kind: "tool.started", occurred_at: new Date(Date.UTC(2026, 0, 1, 0, 0, index)).toISOString() });
    events.push({ ...base, sequence: startSequence + offset * 2 + 1, event_id: `synthetic-event-${index}-finish`, kind: "tool.finished", occurred_at: new Date(Date.UTC(2026, 0, 1, 0, 0, index, 500)).toISOString() });
  }
  return events;
}

export function syntheticAtlasExport(stepCount = 240): string {
  return syntheticAtlasEvents(stepCount).map((event) => JSON.stringify(event)).join("\n") + "\n";
}
