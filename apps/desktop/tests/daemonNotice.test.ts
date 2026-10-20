// The page's reports for the launch's own daemon steps. Run: pnpm test
import { test } from "node:test";
import assert from "node:assert/strict";
import { opensAtOnce, reloadNote } from "../src/app/daemonNotice.ts";

const KINDS = ["outdated", "protocol", "newer", "hung", "failed"] as const;

test("a warning from the launch opens no panel", () => {
  for (const kind of KINDS) assert.equal(opensAtOnce(kind, false), false, kind);
});

test("a restart the user asked for opens the panel of a daemon it cannot use", () => {
  for (const kind of ["protocol", "hung", "failed"] as const) assert.equal(opensAtOnce(kind, true), true, kind);
  for (const kind of ["outdated", "newer"] as const) assert.equal(opensAtOnce(kind, true), false, kind);
});

test("a finished reload leaves no note", () => {
  assert.equal(reloadNote({ error: null, missing: [], lost: [] }), null);
});

test("an unfinished reload leaves one line", () => {
  const note = reloadNote({ error: "skiffd did not come back after the reload", missing: [], lost: ["Skipper"] });
  assert.equal(note, "skiffd did not come back after the reload. Input typed during the reload did not reach Skipper.");
  assert.equal(reloadNote({ error: "1 of 2 live sessions did not come through the reload", missing: ["a"], lost: [] }), "1 of 2 live sessions did not come through the reload: a.");
});
