/**
 * A remote Host's diff in the peek: the plain text its git answers
 * (`git_diff@1`), drawn here line by line. Nothing runs on this Mac: no diff
 * command, no opener, and the Host's paths are never read here. Every state
 * says what it is: loading, no change, binary, untracked, cut, or why not.
 */
import { h } from "../ui/dom";
import type { HostDiff } from "../platform/types";

/** The Host's limit on diff text, for the cut note. */
const LIMIT_KIB = 512;

export type DiffLine = { cls: string; text: string };

/** Each line of `text` and how it shows. Text only: never parsed as HTML. */
export function diffLines(text: string): DiffLine[] {
  const lines = text.split("\n");
  if (lines.at(-1) === "") lines.pop();
  let inHunk = false;
  return lines.map((line) => {
    if (line.startsWith("diff --git ")) {
      inHunk = false;
      return { cls: "dl-file", text: line };
    }
    if (line.startsWith("@@")) {
      inHunk = true;
      return { cls: "dl-hunk", text: line };
    }
    // Before the first hunk, `---` and `+++` name the files.
    if (!inHunk) return { cls: "dl-meta", text: line };
    if (line.startsWith("+")) return { cls: "dl-add", text: line };
    if (line.startsWith("-")) return { cls: "dl-del", text: line };
    if (line.startsWith("\\")) return { cls: "dl-meta", text: line };
    return { cls: "dl-ctx", text: line };
  });
}

/** What the viewer says above the text, for `file`, or the whole worktree without one. */
export function diffNotes(d: HostDiff, file?: string): string[] {
  const notes: string[] = [];
  if (d.untracked) notes.push("Untracked file. It is compared with an empty file.");
  if (d.binary) notes.push("Binary file. There is no text to show.");
  if (!d.text && !d.binary) notes.push("No change against HEAD.");
  if (d.truncated) notes.push(`The diff is longer than ${LIMIT_KIB} KiB. Only its start shows.`);
  if (!file) notes.push("Untracked files are not in this view. Select one to see it.");
  return notes;
}

/** The viewer's body for a diff. */
export function diffBody(d: HostDiff, file?: string): HTMLElement {
  const body = h("div", "diff-view-body");
  body.tabIndex = -1;
  for (const n of diffNotes(d, file)) body.append(h("div", "diff-view-note", n));
  if (d.text && !d.binary) {
    const pre = h("pre", "diff-text mono");
    for (const l of diffLines(d.text)) pre.append(h("span", l.cls, `${l.text}\n`));
    body.append(pre);
  } else if (d.binary && d.text) {
    body.append(h("pre", "diff-text mono", d.text));
  }
  return body;
}

/** The viewer's body while it waits, or when the read failed. */
export function diffState(text: string, error = false): HTMLElement {
  const body = h("div", "diff-view-body");
  body.tabIndex = -1;
  body.append(h("div", error ? "diff-view-note diff-view-error" : "diff-view-note", text));
  if (error) body.firstElementChild!.setAttribute("role", "alert");
  return body;
}
