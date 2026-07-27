import type { AgentKind } from "./api";

export const AGENT_LABELS: Record<AgentKind, string> = {
  codex: "Codex",
  claude: "Claude",
  agy: "AGY",
};

export const AGENT_DESCRIPTIONS: Record<AgentKind, string> = {
  codex: "OpenAI Codex CLI",
  claude: "Anthropic Claude Code",
  agy: "Google Antigravity CLI",
};

export const AGENT_SHORT_LABELS: Record<AgentKind, string> = {
  codex: "Cx",
  claude: "Cl",
  agy: "A",
};

export function agentLabel(agent: AgentKind): string {
  return AGENT_LABELS[agent];
}
