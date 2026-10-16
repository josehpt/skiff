import type { AgentInfo } from "../platform/types";

/**
 * Each known agent's model flag, from its `--help` (2026-10-15). Keep in step
 * with `model_flag` in crates/skiff-core/src/tasks.rs, which builds the line
 * that runs. Settings only shows a hint from this.
 */
const MODEL_FLAGS: Record<string, string> = { claude: "--model", codex: "-m", grok: "-m" };

/** The rule `check_name` in skiff-core keeps too. */
export const TASK_TYPE_NAME = /^[a-z0-9][a-z0-9-]*$/;

export const BAD_TASK_TYPE_NAME = "Use lowercase letters, digits and -, as in code-review.";
export const BAD_MODEL = "A model is one word, as in opus.";

/** The agent's model flag. Null: it takes its model in its command, as a custom agent does. */
export function modelFlag(agent: AgentInfo | undefined): string | null {
  return agent && !agent.custom ? (MODEL_FLAGS[agent.id] ?? null) : null;
}

/** The command line a task type starts, as skiffd builds it. An empty model is the agent's default. */
export function taskCommand(agent: AgentInfo | undefined, model: string): string {
  if (!agent) return "";
  const cmd = agent.command.trim();
  const flag = modelFlag(agent);
  const m = model.trim();
  return flag && m ? `${cmd} ${flag} ${m}` : cmd;
}

/** Why a model cannot be saved, or null. */
export function modelError(model: string): string | null {
  return /\s/.test(model.trim()) ? BAD_MODEL : null;
}
