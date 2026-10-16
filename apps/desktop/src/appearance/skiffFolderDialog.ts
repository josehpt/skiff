import { invoke } from "@tauri-apps/api/core";
import { open as openFolder } from "@tauri-apps/plugin-dialog";
import { isMac } from "../app/keys";
import { reveal } from "../platform/fileActions";
import { confirmAction } from "../ui/confirm";
import { dialogFrame } from "../ui/dialogParts";
import { h } from "../ui/dom";
import { editField, fieldEditKey, textField } from "../ui/fieldClipboard";

/** A folder's contents, as `agent_folder::Counts` sends them. */
export interface FolderCounts {
  tasks: number;
  open: number;
  done: number;
  results: number;
  bytes: number;
  agents_edited: boolean;
  other: string[];
}

type Kind = "current" | "new" | "empty" | "skiff" | "refused";
type Mode = "copy" | "empty" | "switch";

/** `preview_skiff_folder`: what a change to the chosen folder would do. */
interface Preview {
  from: string;
  to: string;
  kind: Kind;
  refused: string | null;
  can_copy: boolean;
  copy_refused: string | null;
  from_counts: FolderCounts | null;
  to_counts: FolderCounts | null;
  moved: string | null;
  running: { uid: string; task: string; pane_id: string }[];
  ledger_env: string | null;
  skipper_runs: boolean;
}

/** `change_skiff_folder`: what the change did. */
interface Done {
  from: string;
  to: string;
  mode: Mode;
  copied: FolderCounts | null;
  moved_note: string | null;
  warning: string | null;
}

const KIND: Record<Kind, string> = {
  current: "In use now",
  new: "New folder",
  empty: "Empty folder",
  skiff: "Skiff folder",
  refused: "Refused",
};

/** Bytes, as the Rust side shows them. */
export function size(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** "3 tasks, 2 open · 3 results, 1.2 KB". */
export function countsLine(c: FolderCounts): string {
  return `${plural(c.tasks, "task")}, ${c.open} open · ${plural(c.results, "result")}, ${size(c.bytes)}`;
}

/** The "Skiff will" line for a preview. */
function verdict(p: Preview): string {
  switch (p.kind) {
    case "current":
      return "Skipper uses this folder now. There is nothing to change.";
    case "refused":
      return p.refused ?? "Skiff cannot use this folder.";
    case "skiff":
      return "Switch to this folder. No copy, no merge.";
    default:
      return p.can_copy ? "Copy the current folder here, check the copy, then switch. Or start empty." : "Set up a new folder here, then switch.";
  }
}

/** The preview's sections, one paragraph each. */
function details(p: Preview): string[] {
  const out = [`From  ${p.from}\nTo    ${p.to}`];
  if (p.kind === "new" || p.kind === "empty") {
    const c = p.from_counts;
    if (p.can_copy && c) {
      const other = c.other.length ? `, and your files: ${c.other.join(", ")}` : "";
      out.push(
        `Copy and switch copies AGENTS.md${c.agents_edited ? " (edited)" : ""}, the ledger (${plural(c.tasks, "task")}: ${c.open} open, ${c.done} done) and results/ (${plural(c.results, "file")}, ${size(c.bytes)})${other}. Result paths move to the new folder.`,
        "Setup makes the CLAUDE.md, GEMINI.md and skill links again.",
        "The old folder stays, with every file. Skiff adds MOVED.md and deletes nothing.",
      );
    }
    out.push("Start empty sets up a new ledger. The current ledger stays in the old folder.");
    if (p.copy_refused) out.push(`Copy is off: ${p.copy_refused}`);
  }
  if (p.kind === "skiff" && p.to_counts) {
    const now = p.from_counts;
    out.push(`This folder: ${countsLine(p.to_counts)}.${now ? `\nThe current folder: ${countsLine(now)}.` : ""}`);
    if (now?.open) out.push(`${plural(now.open, "open task")} stay in the current ledger. Skiff does not merge two ledgers. Change back to finish them.`);
    if (p.moved) out.push("Skipper moved away from this folder before. Tasks added since then are only in the other folder. Skiff removes MOVED.md here.");
  }
  if (p.kind !== "current" && p.kind !== "refused") {
    out.push("The conversation does not move. Skipper starts a new conversation in the new folder when you select its row.");
    if (p.skipper_runs) out.push(`Skipper runs in ${p.from}. Its session ends before the change.`);
    if (p.ledger_env) out.push(`SKIFF_LEDGER is set to ${p.ledger_env}. The change does not touch it.`);
  }
  return out;
}

/**
 * Settings › Skiff agent › Folder › Change…: the person picks where, and the
 * folder there decides what Skiff offers. Nothing changes before the action.
 * `current` is the folder now; the picker opens beside it. `changed` runs
 * after a change, so Settings reads the folder again. The keys
 * go back to what opened the dialog.
 */
export function changeSkiffFolder(o: { current: string; changed: () => void }) {
  const back = document.activeElement as HTMLElement | null;
  const { overlay, panel } = dialogFrame("confirm-panel folder-panel", "dialog", "Change the Skiff agent folder");
  overlay.appendChild(panel);
  document.body.appendChild(overlay);

  let preview: Preview | null = null;
  let busy = false;
  let open = true;

  const close = () => {
    if (!open || busy) return;
    open = false;
    overlay.remove();
    back?.focus?.();
  };

  const title = h("div", "confirm-title", "Change the Skiff agent folder");
  const choices = h("div", "folder-choices");
  const typed = h("input", "prompt-input");
  typed.type = "text";
  typed.spellcheck = false;
  typed.placeholder = "Type a path, then press Enter";
  typed.setAttribute("aria-label", "Folder path");
  const body = h("div", "folder-preview");
  const error = h("div", "prompt-error");
  error.setAttribute("role", "alert");
  const foot = h("div", "confirm-foot");

  const pick = (label: string, path: string, hint: string) => {
    const b = h("button", "folder-choice");
    b.type = "button";
    b.append(h("span", "folder-choice-name mono", label), h("span", "folder-choice-hint", hint));
    b.addEventListener("click", () => void show(path));
    return b;
  };
  const chooser = h("button", "folder-choice");
  chooser.type = "button";
  chooser.append(
    h("span", "folder-choice-name", "Choose…"),
    h("span", "folder-choice-hint", isMac ? "The system picker. Press ⌘⇧. there to show hidden folders." : "The system picker."),
  );
  chooser.addEventListener("click", async () => {
    const dir = await openFolder({ directory: true, title: "Choose the Skiff agent folder", defaultPath: o.current.replace(/\/[^/]+\/?$/, "") || undefined }).catch(() => null);
    if (typeof dir === "string") await show(dir);
  });
  choices.append(
    pick("~/skiff-agent", "~/skiff-agent", "Visible agent workspace in your home folder."),
    pick("~/.skiff-agent", "~/.skiff-agent", "Hidden agent workspace in your home folder."),
    chooser,
  );
  // A typed, pasted or cut path is a new choice: the preview and its actions go
  // until Enter reads the folder there. An action never runs on a path the field no longer shows.
  typed.addEventListener("input", () => {
    if (!preview || busy) return;
    preview = null;
    error.textContent = "";
    draw();
  });
  typed.addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    e.preventDefault();
    e.stopPropagation();
    if (typed.value.trim()) void show(typed.value);
  });

  /** Reads the chosen folder and draws what Skiff would do. */
  async function show(path: string) {
    if (busy) return;
    error.textContent = "";
    try {
      preview = await invoke<Preview>("preview_skiff_folder", { path });
    } catch (e) {
      preview = null;
      error.textContent = String(e);
    }
    draw();
  }

  function draw() {
    body.replaceChildren();
    foot.replaceChildren();
    const cancel = h("button", "confirm-cancel", preview ? "Cancel" : "Close");
    cancel.type = "button";
    cancel.addEventListener("click", close);
    foot.append(cancel);
    if (!preview) return;
    const p = preview;
    const head = h("div", "folder-resolved");
    head.append(h("span", "folder-kind" + (p.kind === "refused" ? " bad" : ""), KIND[p.kind]), h("span", "mono", p.to));
    body.append(head, h("div", "folder-verdict" + (p.kind === "refused" ? " bad" : ""), verdict(p)));
    const text = h("div", "confirm-body", details(p).join("\n\n"));
    body.append(text);
    if (p.kind === "current" || p.kind === "refused") return;

    // Open tasks whose pane still runs report to the old ledger. Wait by default.
    const running = p.running.length;
    if (running) {
      const list = p.running.map((r) => `${r.uid}  ${r.task}`).join("\n");
      body.append(h("div", "folder-wait", `${plural(running, "task")} still ${running === 1 ? "runs" : "run"}. Switch when ${running === 1 ? "it is" : "they are"} done.\n${list}`));
    }
    const act = (label: string, mode: Mode, primary: boolean) => {
      const b = h("button", primary ? "confirm-act go" : "confirm-cancel", label);
      b.type = "button";
      b.disabled = !!running;
      b.dataset.mode = mode;
      b.addEventListener("click", () => void run(mode, false));
      return b;
    };
    const acts: HTMLButtonElement[] = [];
    if (p.kind === "skiff") acts.push(act("Switch to this folder", "switch", true));
    else {
      acts.push(act("Start empty", "empty", !p.can_copy || !!p.copy_refused));
      if (p.can_copy && !p.copy_refused) acts.push(act("Copy and switch", "copy", true));
    }
    if (running) {
      const primary = acts[acts.length - 1];
      const anyway = h("button", "confirm-cancel", "Change anyway…");
      anyway.type = "button";
      anyway.addEventListener("click", async () => {
        const ok = await confirmAction({
          title: "Change while tasks run?",
          body: `${plural(running, "task")} still ${running === 1 ? "runs" : "run"} in ${p.from}. ${running === 1 ? "Its" : "Their"} results stay in the old folder, and the new Skipper may not see ${running === 1 ? "it" : "them"} finish.`,
          action: "Change anyway",
        });
        if (ok) void run(primary.dataset.mode as Mode, true);
      });
      foot.append(anyway);
    }
    foot.append(...acts);
  }

  /** Runs the change. A failure says that nothing changed and keeps the dialog. */
  async function run(mode: Mode, anyway: boolean) {
    if (!preview || busy) return;
    busy = true;
    error.textContent = "";
    for (const b of foot.querySelectorAll("button")) b.disabled = true;
    const lit = foot.querySelector<HTMLButtonElement>(`[data-mode="${mode}"]`);
    if (lit) lit.textContent = "…";
    try {
      const done = await invoke<Done>("change_skiff_folder", { path: preview.to, mode, endSkipper: preview.skipper_runs, anyway });
      busy = false;
      o.changed();
      finished(done);
    } catch (e) {
      busy = false;
      // Read the folders again, so Try again offers what is valid now.
      const from = preview.from;
      await show(preview.to);
      error.textContent = `The change stopped. Nothing changed: Skipper stays on ${from}.\n${String(e)}`;
    }
  }

  /** The done step: where Skipper is now, what was copied, what stays. */
  function finished(d: Done) {
    title.textContent = `Skipper now uses ${d.to}`;
    choices.remove();
    typed.remove();
    const lines: string[] = [];
    if (d.copied) lines.push(`Copied and checked: ${countsLine(d.copied)}.`);
    if (d.mode === "empty") lines.push("The new folder has a new, empty ledger.");
    if (d.mode === "switch") lines.push("Skiff switched without a copy. Both folders stay as they were.");
    if (d.moved_note) lines.push(`The old folder stays, with every file and MOVED.md. Skiff deleted nothing. You can delete it when you choose.`);
    lines.push("Skipper starts in the new folder when you select its row, with a new conversation.");
    lines.push(`To go back, choose ${d.from} again. Tasks added from now on are only in the new folder.`);
    if (d.warning) lines.push(d.warning);
    body.replaceChildren(h("div", "confirm-body", lines.join("\n\n")));
    foot.replaceChildren();
    if (d.mode !== "switch") {
      const old = h("button", "confirm-cancel", "Show old folder");
      old.type = "button";
      old.addEventListener("click", () => reveal(d.from));
      foot.append(old);
    }
    const ok = h("button", "confirm-act go", "Done");
    ok.type = "button";
    ok.addEventListener("click", close);
    foot.append(ok);
    ok.focus();
  }

  panel.append(title, choices, typed, body, error, foot);
  draw();

  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });
  overlay.addEventListener("keydown", (e) => {
    e.stopPropagation();
    // ⌘X and ⌘A in the path field on macOS, as Settings does them. Copy and paste come through the menu.
    const edit = fieldEditKey(e, isMac);
    const field = edit && textField(e.target as Element);
    if (edit && field) {
      e.preventDefault();
      return void editField(field, edit);
    }
    if (e.key === "Escape") close();
    else if (e.key === "Enter") {
      // Enter runs the lit action, never Cancel and never a disabled one.
      const t = document.activeElement as HTMLElement | null;
      if (t instanceof HTMLButtonElement) return;
      foot.querySelector<HTMLButtonElement>(".confirm-act:not(:disabled)")?.click();
    } else if (e.key === "Tab") {
      const stops = [...panel.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)")];
      const i = stops.indexOf(document.activeElement as HTMLElement);
      stops[(i + (e.shiftKey ? -1 : 1) + stops.length) % stops.length]?.focus();
    } else return;
    e.preventDefault();
  });
  choices.querySelector<HTMLButtonElement>("button")?.focus();
}
