import { h } from "../ui/dom";
import { rune } from "../ui/runes";

/**
 * A Settings table with a draft row last: Settings › Agents and Settings ›
 * Skiff agent › Task types. One implementation for both, so they add, edit,
 * remove and report errors the same way.
 *
 * - The draft row is always last. Enter in it, or its +, adds. An empty
 *   required cell gets the focus and no message. A bad value shows the error
 *   line, marks the cell and keeps the text. A good one adds the row, clears
 *   the draft and puts focus back in its first field.
 * - In a row, a text cell saves on Enter or blur, Esc puts the old value
 *   back, and a select saves on change. A rename to an empty or taken name
 *   reverts, and the error line says why.
 * - − removes at once. Focus goes to the − now at the same place, else the
 *   last one, else the draft.
 * - The draft and the error live in the caller's {@link RowTableState}, so a
 *   redraw for any other reason keeps what the person typed.
 *
 * Every control is a native input, select or button, so the Settings arrow
 * keys reach each one. The table adds no key.
 */

export type Draft = Record<string, string>;

/** One column. `key` names the field in the draft and in edits. */
export interface RowCol<R> {
  key: string;
  /** Column header text. Empty for the lead column. */
  head: string;
  /** The word in aria-labels: "name", "command", "model". */
  name: string;
  /** lead: a cell the caller draws (the switch). key: the name, required, renames. preview: read-only text. */
  kind: "lead" | "key" | "text" | "select" | "preview";
  mono?: boolean;
  /** The draft cannot add while this cell is empty. */
  required?: boolean;
  placeholder?: (row: R | null) => string;
  /** select: [value, label] pairs for a row, or for the draft (null). */
  options?: (row: R | null) => [string, string][];
  /** select: the one option shown, off, when there are none. */
  empty?: string;
  /** The draft's starting value. Absent: empty. */
  initial?: () => string;
  /** Text in place of the field, such as "in the command". Null: the field shows. */
  note?: (row: R | null, draft: Draft) => string | null;
  get?: (row: R) => string;
  /** A preview cell that shows an error, not a value. */
  bad?: (row: R) => boolean;
  /** lead: the caller's control for the row. The draft's lead cell is blank. */
  draw?: (row: R) => HTMLElement;
  /** Why a value cannot be saved, checked before the daemon. Null: good. */
  check?: (value: string) => string | null;
}

export interface RowTableSpec<R> {
  /** Grid class: "g-agents", "g-tasks". Also names the table in the DOM. */
  grid: string;
  /** "agent", "task type": in "Add agent" and "New agent name". */
  noun: string;
  /** "An agent", "A task type": the start of the error lines. */
  article: string;
  /** After "An agent named x exists." on a duplicate add. */
  dupHint: string;
  /** The id of the section's heading. */
  labelledBy: string;
  /** Header of the end column. Empty: "Remove" for screen readers only. */
  endHead?: string;
  cols: RowCol<R>[];
  rows: R[];
  id: (row: R) => string;
  rowClass?: (row: R) => string;
  /** What shows in the end cell before the −, such as the agent's status. */
  status?: (row: R) => HTMLElement;
  /** The draft's preview column, as the person types. */
  preview?: (draft: Draft) => string;
  /** Why the draft cannot add at all, such as no agent being on. */
  blocked?: () => string | null;
  /** Why a row's new value cannot be saved, past the column's own check. */
  checkEdit?: (row: R, key: string, value: string) => string | null;
  /** Each saves and answers with the error to show, or null. They do not redraw. */
  add: (draft: Draft) => Promise<string | null>;
  edit: (row: R, key: string, value: string) => Promise<string | null>;
  remove: (row: R) => Promise<string | null>;
}

/** What survives a redraw. The caller keeps one per table. */
export interface RowTableState {
  draft: Draft;
  error: string | null;
  /** The draft column the error is about. */
  bad: string | null;
}

export const tableState = (): RowTableState => ({ draft: {}, error: null, bad: null });

const FIELDS = new Set(["key", "text", "select"]);

export function rowTable<R>(spec: RowTableSpec<R>, state: RowTableState, redraw: () => void): HTMLElement {
  const errId = `${spec.grid}-error`;
  const table = h("div", `set-table ${spec.grid}`);
  table.dataset.rowTable = spec.grid;
  table.setAttribute("role", "table");
  table.setAttribute("aria-labelledby", spec.labelledBy);

  const initial = (c: RowCol<R>) => c.initial?.() ?? "";
  const value = (c: RowCol<R>) => {
    const v = state.draft[c.key] ?? initial(c);
    // A draft select keeps a value only while it is an option.
    if (c.kind === "select" && !(c.options?.(null) ?? []).some(([o]) => o === v)) return initial(c);
    return v;
  };
  const current = (): Draft => Object.fromEntries(spec.cols.map((c) => [c.key, value(c)]));
  const draftLabel = (c: RowCol<R>) => `New ${spec.noun} ${c.name}`;
  const firstField = spec.cols.find((c) => FIELDS.has(c.kind));

  /** Focuses a control of this table by its label, after a redraw. */
  function focus(label: string) {
    const el = document.querySelector<HTMLElement>(`[data-row-table="${spec.grid}"] [aria-label="${CSS.escape(label)}"]`);
    el?.focus();
    if (el instanceof HTMLInputElement) el.setSelectionRange(el.value.length, el.value.length);
  }
  const focusDraft = () => firstField && focus(draftLabel(firstField));

  /** Shows the outcome of a save. True when it worked. */
  function settle(error: string | null, bad: string | null = null): boolean {
    state.error = error;
    state.bad = error ? bad : null;
    redraw();
    return !error;
  }

  // Header.
  const head = h("div", "set-row set-cols");
  head.setAttribute("role", "row");
  for (const c of spec.cols) {
    const cell = h("span", cellClass(c), c.head);
    cell.setAttribute("role", "columnheader");
    if (!c.head) cell.setAttribute("aria-label", c.name);
    head.appendChild(cell);
  }
  const endHead = h("span", "c-status c-end", spec.endHead ?? "");
  endHead.setAttribute("role", "columnheader");
  if (!spec.endHead) endHead.setAttribute("aria-label", "Remove");
  head.appendChild(endHead);
  table.appendChild(head);

  // Rows.
  spec.rows.forEach((r, at) => {
    const id = spec.id(r);
    const row = h("div", ("set-row " + (spec.rowClass?.(r) ?? "")).trim());
    row.setAttribute("role", "row");
    for (const c of spec.cols) {
      const cell = h("span", cellClass(c));
      cell.setAttribute("role", "cell");
      const label = `${id} ${c.name}`;
      const note = c.note?.(r, {});
      if (c.kind === "lead") cell.appendChild(c.draw!(r));
      else if (c.kind === "preview") fillPreview(cell, c.get?.(r) ?? "", !!c.bad?.(r));
      else if (note !== null && note !== undefined) fillNote(cell, note);
      else if (c.kind === "select") {
        const old = c.get?.(r) ?? "";
        const pick = select(c, c.options?.(r) ?? [], old, label);
        pick.addEventListener("change", () => void commit(r, c, pick.value, old, label));
        cell.appendChild(pick);
      } else {
        const old = c.get?.(r) ?? "";
        const input = field(c, old, c.placeholder?.(r) ?? "", label);
        input.addEventListener("keydown", (e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            input.blur();
          } else if (e.key === "Escape") {
            // As every Settings field: the old value comes back, and Esc goes on to close Settings.
            input.value = old;
            input.blur();
          }
        });
        input.addEventListener("blur", () => void commit(r, c, input.value.trim(), old, label));
        cell.appendChild(input);
      }
      row.appendChild(cell);
    }
    const end = h("span", "c-status c-end");
    end.setAttribute("role", "cell");
    if (spec.status) end.appendChild(spec.status(r));
    const rm = iconButton("indicators-minus", "Remove", `Remove ${id}`);
    rm.classList.add("c-remove");
    rm.addEventListener("click", () => void remove(r, at));
    end.appendChild(rm);
    row.appendChild(end);
    table.appendChild(row);
  });

  // The draft row.
  const draft = h("div", "set-row set-draft");
  draft.setAttribute("role", "row");
  let preview: HTMLElement | null = null;
  const values = current();
  for (const c of spec.cols) {
    const cell = h("span", cellClass(c));
    cell.setAttribute("role", "cell");
    const label = draftLabel(c);
    const note = c.note?.(null, values);
    if (c.kind === "lead") {
      // Blank: the draft has no switch.
    } else if (c.kind === "preview") {
      preview = fillPreview(cell, spec.preview?.(values) ?? "", false);
    } else if (note !== null && note !== undefined) {
      fillNote(cell, note);
    } else {
      const el =
        c.kind === "select"
          ? select(c, c.options?.(null) ?? [], value(c), label)
          : field(c, value(c), c.placeholder?.(null) ?? (c.kind === "key" ? `Add ${spec.noun}` : ""), label);
      if (state.bad === c.key && state.error) {
        el.setAttribute("aria-invalid", "true");
        el.setAttribute("aria-describedby", errId);
      }
      if (el instanceof HTMLSelectElement) {
        el.addEventListener("change", () => {
          state.draft[c.key] = el.value;
          // The agent decides whether the model is a field.
          redraw();
          focus(label);
        });
      } else {
        el.addEventListener("input", () => {
          state.draft[c.key] = el.value;
          if (preview) preview.textContent = preview.title = spec.preview?.(current()) ?? "";
        });
      }
      el.addEventListener("keydown", (e) => {
        const k = (e as KeyboardEvent).key;
        if (k === "Enter") {
          e.preventDefault();
          void submit();
        } else if (k === "Escape" && dirty()) {
          // Clears the draft. Settings stays open: Esc in an empty draft closes it.
          e.stopPropagation();
          e.preventDefault();
          state.draft = {};
          settle(null);
          focusDraft();
        }
      });
      cell.appendChild(el);
    }
    draft.appendChild(cell);
  }
  const end = h("span", "c-status c-end");
  end.setAttribute("role", "cell");
  // Never off: the + always says what is missing.
  const add = iconButton("indicators-plus", "Add", `Add ${spec.noun}`);
  add.addEventListener("click", () => void submit());
  end.appendChild(add);
  draft.appendChild(end);
  table.appendChild(draft);

  if (state.error) {
    const err = h("div", "set-error", state.error);
    err.id = errId;
    err.setAttribute("role", "alert");
    table.appendChild(err);
  }
  return table;

  function dirty(): boolean {
    return spec.cols.some((c) => FIELDS.has(c.kind) && value(c) !== initial(c));
  }

  async function submit() {
    const blocked = spec.blocked?.();
    if (blocked) {
      settle(blocked, firstField?.key ?? null);
      return focusDraft();
    }
    const d = current();
    for (const c of spec.cols) d[c.key] = d[c.key].trim();
    const shown = (c: RowCol<R>) => c.note?.(null, d) == null;
    // A cell that shows a note has no field: what was typed there before does not count.
    for (const c of spec.cols) if (!shown(c)) d[c.key] = "";
    // An empty required cell: the keys go there, and nothing is wrong yet.
    const empty = spec.cols.find((c) => c.required && shown(c) && !d[c.key]);
    if (empty) return focus(draftLabel(empty));
    for (const c of spec.cols.filter((c) => FIELDS.has(c.kind) && shown(c) && d[c.key])) {
      const why =
        c.check?.(d[c.key]) ??
        (c.kind === "key" && spec.rows.some((r) => spec.id(r) === d[c.key]) ? `${spec.article} named ${d[c.key]} exists. ${spec.dupHint}` : null);
      if (why) {
        settle(why, c.key);
        return focus(draftLabel(c));
      }
    }
    const error = await spec.add(d);
    if (!error) state.draft = {};
    settle(error);
    focusDraft();
  }

  async function commit(r: R, c: RowCol<R>, v: string, old: string, label: string) {
    if (v === old.trim()) return;
    const id = spec.id(r);
    if (c.kind === "key") {
      const why = !v
        ? `${spec.article} needs a name.`
        : (c.check?.(v) ?? (spec.rows.some((x) => spec.id(x) === v) ? `${spec.article} named ${v} exists.` : null));
      if (why) return void settle(`${why} ${id} keeps its name.`);
    } else {
      const why = c.check?.(v) ?? spec.checkEdit?.(r, c.key, v);
      if (why) return void settle(why);
    }
    settle(await spec.edit(r, c.key, v));
    // Settings puts back a focused input itself. A select, or a renamed row, needs it here.
    if (c.kind === "select") focus(label);
  }

  async function remove(r: R, at: number) {
    settle(await spec.remove(r));
    const t = document.querySelector(`[data-row-table="${spec.grid}"]`);
    const rms = t ? [...t.querySelectorAll<HTMLElement>(".c-remove")] : [];
    const next = rms[Math.min(at, rms.length - 1)];
    if (next) next.focus();
    else focusDraft();
  }
}

function cellClass<R>(c: RowCol<R>): string {
  if (c.kind === "lead") return "c-lead";
  if (c.kind === "preview") return "c-preview";
  return `c-field c-${c.key}`;
}

function field<R>(c: RowCol<R>, value: string, placeholder: string, label: string): HTMLInputElement {
  const input = h("input", (c.kind === "key" ? "c-name" : "c-cmd") + (c.mono ? " mono" : ""));
  input.value = value;
  input.placeholder = placeholder;
  input.spellcheck = false;
  input.setAttribute("aria-label", label);
  return input;
}

function select<R>(c: RowCol<R>, options: [string, string][], value: string, label: string): HTMLSelectElement {
  const pick = h("select", "c-cmd");
  pick.setAttribute("aria-label", label);
  if (!options.length) pick.append(h("option", "", c.empty ?? ""));
  for (const [v, text] of options) {
    const o = h("option", "", text);
    o.value = v;
    pick.append(o);
  }
  if (options.length) pick.value = value;
  pick.disabled = !options.length;
  return pick;
}

function fillPreview(cell: HTMLElement, text: string, bad: boolean): HTMLElement {
  const span = h("span", "mono" + (bad ? " bad" : ""), text);
  span.title = text;
  cell.appendChild(span);
  return span;
}

function fillNote(cell: HTMLElement, text: string) {
  cell.classList.add("c-note");
  cell.textContent = text;
}

function iconButton(icon: Parameters<typeof rune>[0], title: string, label: string): HTMLButtonElement {
  const b = h("button", "set-rm set-ico");
  b.type = "button";
  b.title = title;
  b.setAttribute("aria-label", label);
  b.appendChild(rune(icon));
  return b;
}
