import { test } from "node:test";
import assert from "node:assert/strict";
import { instructionsError, instructionWords, taskCommand } from "../src/appearance/taskTypes.ts";
import type { AgentInfo } from "../src/platform/types.ts";

const agent = (id: string, command = id, custom = false) => ({ id, command, custom, installed: true, enabled: true, default_command: id }) as AgentInfo;

test("an instruction file is a full path or ~/", () => {
  assert.equal(instructionsError(""), null);
  assert.equal(instructionsError(" ~/.skiff/types/review.md "), null);
  assert.equal(instructionsError("/etc/review.md"), null);
  for (const bad of ["review.md", "./review.md", "~review.md"]) assert.notEqual(instructionsError(bad), null, bad);
});

test("the preview gives each agent the file as skiff-core does", () => {
  assert.equal(taskCommand(agent("claude"), "opus", "~/r.md"), "claude --model opus --append-system-prompt-file ~/r.md");
  assert.equal(taskCommand(agent("claude"), "", "~/my review.md"), "claude --append-system-prompt-file '~/my review.md'");
  assert.equal(taskCommand(agent("codex"), "gpt-5", "~/r.md"), "codex -m gpt-5 -c developer_instructions=<text of ~/r.md>");
  // No known way: the preview adds nothing, and the saved row shows the error.
  assert.equal(taskCommand(agent("grok"), "", "~/r.md"), "grok");
  assert.equal(instructionWords(agent("claude", "claude", true), "~/r.md"), null);
  assert.equal(taskCommand(agent("claude"), "opus"), "claude --model opus");
});
