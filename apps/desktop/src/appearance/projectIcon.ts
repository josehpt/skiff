import { h } from "../ui/dom";
import type { Project } from "../platform/types";

const trimmed = new Map<string, Promise<string>>();

/**
 * The icon with its transparent border cut off, centered in a square at 2x
 * the shown size. All icons then fill the same box. Done once per image.
 */
function trimIcon(src: string, size: number): Promise<string> {
  const key = size + " " + src;
  let done = trimmed.get(key);
  if (!done) {
    done = new Promise<string>((resolve) => {
      const img = new Image();
      img.onload = () => {
        try {
          const w = img.naturalWidth || 128;
          const ht = img.naturalHeight || 128;
          const c = document.createElement("canvas");
          c.width = w;
          c.height = ht;
          const g = c.getContext("2d", { willReadFrequently: true })!;
          g.drawImage(img, 0, 0, w, ht);
          const px = g.getImageData(0, 0, w, ht).data;
          let x0 = w, y0 = ht, x1 = -1, y1 = -1;
          for (let y = 0; y < ht; y++)
            for (let x = 0; x < w; x++)
              if (px[(y * w + x) * 4 + 3] > 16) {
                if (x < x0) x0 = x;
                if (x > x1) x1 = x;
                if (y < y0) y0 = y;
                if (y > y1) y1 = y;
              }
          if (x1 < 0) return resolve(src);
          const bw = x1 - x0 + 1, bh = y1 - y0 + 1;
          const out = document.createElement("canvas");
          out.width = out.height = size * 2;
          const k = (size * 2) / Math.max(bw, bh);
          const o = out.getContext("2d")!;
          o.imageSmoothingQuality = "high";
          o.drawImage(c, x0, y0, bw, bh, (size * 2 - bw * k) / 2, (size * 2 - bh * k) / 2, bw * k, bh * k);
          resolve(out.toDataURL("image/png"));
        } catch {
          resolve(src);
        }
      };
      img.onerror = () => resolve(src);
      img.src = src;
    });
    trimmed.set(key, done);
  }
  return done;
}

export function projectIcon(p: Project, size: number): HTMLImageElement {
  const img = h("img", "project-icon");
  img.width = size;
  img.height = size;
  img.alt = "";
  img.draggable = false;
  // Hidden until trimmed, so the row does not jump from one size to the next.
  img.style.visibility = "hidden";
  void trimIcon(p.icon!, size).then((src) => {
    img.src = src;
    img.style.visibility = "";
  });
  return img;
}
