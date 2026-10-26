export const AGENT_KINDS = ["codex", "claude", "agy", "kilo"] as const;

export type AgentKind = (typeof AGENT_KINDS)[number];

export const UNSUPPORTED_AGENT_LABEL = "Unknown agent";

const MAX_UNSUPPORTED_AGENT_LABEL_LENGTH = 32;

export function isAgentKind(value: unknown): value is AgentKind {
  return (
    typeof value === "string" &&
    (AGENT_KINDS as readonly string[]).includes(value)
  );
}

export const AGENT_LABELS: Record<AgentKind, string> = {
  codex: "Codex",
  claude: "Claude",
  agy: "AGY",
  kilo: "Kilo",
};

export const AGENT_DESCRIPTIONS: Record<AgentKind, string> = {
  codex: "OpenAI Codex CLI",
  claude: "Anthropic Claude Code",
  agy: "Google Antigravity CLI",
  kilo: "Kilo Code CLI",
};

export const AGENT_SHORT_LABELS: Record<AgentKind, string> = {
  codex: "Cx",
  claude: "Cl",
  agy: "A",
  kilo: "K",
};

export function agentLabel(agent: AgentKind): string {
  return AGENT_LABELS[agent];
}

export interface SessionAgentIdentity {
  /** Runnable agent kind, or null when this bundle does not recognize it. */
  agent: AgentKind | null;
  /** Display label; for unknown agents a sanitized copy of the raw value. */
  agentLabel: string;
}

export function sessionAgentLabel(session: SessionAgentIdentity): string {
  return session.agent ? AGENT_LABELS[session.agent] : session.agentLabel;
}

export function sessionAgentShortLabel(session: SessionAgentIdentity): string {
  return session.agent ? AGENT_SHORT_LABELS[session.agent] : "?";
}

/** CSS modifier for an agent, including the display-only unsupported case. */
export function agentClassName(agent: AgentKind | null): string {
  return agent ?? "unsupported";
}

/**
 * Builds a bounded, printable display label for an agent value this bundle
 * cannot run. The raw value is never used for launch decisions.
 */
export function unsupportedAgentLabel(raw: unknown): string {
  if (typeof raw !== "string") {
    return UNSUPPORTED_AGENT_LABEL;
  }
  const printable = raw
    .replace(/[^\x20-\x7e]/g, "")
    .trim()
    .slice(0, MAX_UNSUPPORTED_AGENT_LABEL_LENGTH);
  return printable.length > 0 ? printable : UNSUPPORTED_AGENT_LABEL;
}
