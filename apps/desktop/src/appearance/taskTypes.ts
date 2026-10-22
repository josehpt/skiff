import type { AgentInfo } from "../platform/types";

/**
 * Each known agent's model flag, from its `--help` (2026-10-15). Keep in step
 * with `model_flag` in crates/skiff-core/src/tasks.rs, which builds the line
 * that runs. Settings only shows a hint from this.
 */
const MODEL_FLAGS: Record<string, string> = { claude: "--model", codex: "-m", grok: "-m" };

/**
 * How each known agent takes an instruction file, from its CLI docs
 * (2026-10-22). Keep in step with `delivery` in crates/skiff-core/src/tasks.rs.
 * codex takes text, not a path: Skiff reads the file at start.
 */
const INSTRUCTION_FLAGS: Record<string, (path: string) => string> = {
  claude: (p) => `--append-system-prompt-file ${shellWord(p)}`,
  codex: (p) => `-c developer_instructions=<text of ${p}>`,
};

/** The rule `check_name` in skiff-core keeps too. */
export const TASK_TYPE_NAME = /^[a-z0-9][a-z0-9-]*$/;

export const BAD_TASK_TYPE_NAME = "Use lowercase letters, digits and -, as in code-review.";
export const BAD_MODEL = "A model is one word, as in opus.";
export const BAD_INSTRUCTIONS = "Name the instruction file with a full path or ~/, as in ~/.skiff/types/review.md.";

/** A path as one shell word, as skiff-core's preview quotes it. */
function shellWord(s: string): string {
  return /^[A-Za-z0-9/~._+-]+$/.test(s) ? s : `'${s.replace(/'/g, "'\\''")}'`;
}

/** The words that give the agent its instruction file. Null: it has no known way. */
export function instructionWords(agent: AgentInfo | undefined, path: string): string | null {
  const f = agent && !agent.custom ? INSTRUCTION_FLAGS[agent.id] : undefined;
  return f ? f(path) : null;
}

/** The agent's model flag. Null: it takes its model in its command, as a custom agent does. */
export function modelFlag(agent: AgentInfo | undefined): string | null {
  return agent && !agent.custom ? (MODEL_FLAGS[agent.id] ?? null) : null;
}

/**
 * The command line a task type starts, as skiffd builds it. An empty model is
 * the agent's default. An empty file adds nothing; a file the agent cannot
 * take adds nothing here, and the saved row shows why.
 */
export function taskCommand(agent: AgentInfo | undefined, model: string, instructions = ""): string {
  if (!agent) return "";
  const cmd = agent.command.trim();
  const flag = modelFlag(agent);
  const m = model.trim();
  const line = flag && m ? `${cmd} ${flag} ${m}` : cmd;
  const f = instructions.trim();
  const words = f ? instructionWords(agent, f) : null;
  return words ? `${line} ${words}` : line;
}

/** Why an instruction file cannot be saved, or null. Empty is none. */
export function instructionsError(path: string): string | null {
  const p = path.trim();
  return !p || ((p.startsWith("/") || p.startsWith("~/")) && !/[\r\n]/.test(p)) ? null : BAD_INSTRUCTIONS;
}

/** Why a model cannot be saved, or null. */
export function modelError(model: string): string | null {
  return /\s/.test(model.trim()) ? BAD_MODEL : null;
}
