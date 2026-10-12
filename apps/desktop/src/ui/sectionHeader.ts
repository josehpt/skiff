import { button, h } from "./dom";
import { chevron } from "./icons";
export function sectionHeader(key: string, label: string, open: boolean, glyph: SVGSVGElement, toggle: () => void): HTMLButtonElement {
  const head = button("wt-count files-head", "", toggle);
  head.dataset.key = head.dataset.fold = key;
  head.setAttribute("aria-expanded", String(open));
  const tgl = h("span", "files-toggle");
  tgl.appendChild(chevron(open));
  glyph.classList.add("files-icon");
  head.append(tgl, glyph, h("span", "", label));
  return head;
}
