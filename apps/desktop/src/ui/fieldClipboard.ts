/**
 * Cut, copy, paste and select all in a text field, for screens that hold the
 * keys, such as Settings. The copy and paste keys are app actions with menu
 * accelerators, so the webview never does them in a field on its own. On
 * macOS no Edit menu item gives ⌘X or ⌘A to the field either.
 *
 * An edit goes in as typing does (`insertText`): the field's undo, its input
 * event and its save on Enter or blur all work as for typed text.
 */
import { copyText, pasteText } from "../platform/clipboard";
import { showError } from "./alerts";

export type FieldEdit = "copy" | "cut" | "paste" | "select-all";
export type TextField = HTMLInputElement | HTMLTextAreaElement;

/** Input types that hold plain text. A password field does not give its text to the clipboard. */
const TEXT_INPUTS = new Set(["text", "search", "url", "email", "tel"]);

/** The element as a text field, or null. xterm's hidden textarea is the terminal's, not a field. */
export function textField(el: Element | null): TextField | null {
  if (el instanceof HTMLTextAreaElement) return el.classList.contains("xterm-helper-textarea") ? null : el;
  if (el instanceof HTMLInputElement && TEXT_INPUTS.has(el.type)) return el;
  return null;
}

/** Text for a one-line field: a trailing line break goes, and the other line breaks become spaces. */
export function oneLine(text: string): string {
  return text.replace(/(\r\n|\r|\n)+$/, "").replace(/\r\n|\r|\n/g, " ");
}

/**
 * The cut and select-all keys on macOS: ⌘X and ⌘A. Other platforms keep the
 * webview's own Ctrl+X and Ctrl+A. Copy and paste are the `copy` and `paste` actions.
 */
export function fieldEditKey(e: KeyboardEvent, mac: boolean): FieldEdit | null {
  if (!mac || !e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return null;
  if (e.code === "KeyX") return "cut";
  if (e.code === "KeyA") return "select-all";
  return null;
}

/** Does the edit on the field. Cut and paste do nothing in a read-only or disabled field. */
export async function editField(field: TextField, edit: FieldEdit): Promise<void> {
  const at = field.selectionStart ?? 0;
  const to = field.selectionEnd ?? at;
  const locked = field.readOnly || field.disabled;
  try {
    if (edit === "select-all") return field.select();
    if (edit === "copy" || edit === "cut") {
      if (at === to || (edit === "cut" && locked)) return;
      await copyText(field.value.slice(at, to));
      // The clipboard write is async: the field can lose focus or change while it runs.
      if (edit === "cut" && document.activeElement === field) replace(field, at, to, "");
      return;
    }
    if (locked) return;
    const text = await pasteText();
    if (!text || document.activeElement !== field) return;
    replace(field, field.selectionStart ?? at, field.selectionEnd ?? to, field instanceof HTMLInputElement ? oneLine(text) : text);
  } catch (e) {
    showError(e);
  }
}

function replace(field: TextField, at: number, to: number, text: string) {
  field.setSelectionRange(at, to);
  // insertText keeps the field's undo. Where it is missing, set the text and send the input event by hand.
  const done = text ? document.execCommand("insertText", false, text) : document.execCommand("delete");
  if (done) return;
  field.setRangeText(text, at, to, "end");
  field.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: text ? "insertFromPaste" : "deleteByCut", data: text || null }));
}
