// Reading a notebook the way both scorers (evals/journal, evals/agent-native) do.

/** A cell's source as one string. */
export function cellText(cell) {
  return Array.isArray(cell.source) ? cell.source.join("") : String(cell.source ?? "");
}

/** `metadata.ganymede.agent`, or null for a cell a person wrote. */
export function agentOf(cell) {
  const a = cell.metadata?.ganymede?.agent;
  return a && typeof a === "object" ? a : null;
}
