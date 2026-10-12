/** DOM queries and element builders. */
export const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

/**
 * True while a menu, the palette, Settings, a dialog or a visible preview is up.
 * A session terminal must not take its focus. App navigation checks dialogs separately,
 * so it can leave the peek without sending its key to the tool.
 */
export function modalOpen(includePeek = true): boolean {
  const dialogs = "#ctx-menu:not([hidden]), #launch-menu:not([hidden]), #switcher:not([hidden]), #settings:not([hidden]), .confirm-overlay, .fleet-name";
  return !!document.querySelector(includePeek ? `${dialogs}, .peek:not([hidden])` : dialogs);
}

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text !== undefined) el.textContent = text;
  return el;
}

export function button(cls: string, text: string, onClick: () => void): HTMLButtonElement {
  const b = h("button", cls, text);
  b.type = "button";
  b.addEventListener("click", onClick);
  return b;
}
