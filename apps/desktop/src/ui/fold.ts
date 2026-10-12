/**
 * Open and fold animation for every row in the list that opens: fleets,
 * projects, worktrees, Changes, Files and folders. The rows under a head
 * carry `data-kids=<key>`; the head carries `data-fold=<key>`. They grow
 * from nothing or shrink into the head in 180ms. Reduced motion: at once.
 */

/** The rows that come in on the next sidebar render, to grow from nothing. */
let opening: string | null = null;

const still = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const find = (attr: string, key: string) => document.querySelector<HTMLElement>(`#sidebar-scroll [${attr}="${CSS.escape(key)}"]`);

/** `commit` changes the state and renders. Folding runs it once the rows are gone. */
export function animateFold(key: string, open: boolean, commit: () => void) {
  const kids = find("data-kids", key);
  if (kids?.classList.contains("tree-folding")) return;
  if (open) {
    opening = still() ? null : key;
    return commit();
  }
  if (!kids || still()) return commit();
  kids.style.setProperty("--h", `${kids.offsetHeight}px`);
  kids.classList.add("tree-folding");
  find("data-fold", key)?.querySelector(".chev")?.classList.add("closed");
  let fired = false;
  const done = () => {
    if (fired) return;
    fired = true;
    commit();
  };
  kids.addEventListener("animationend", (e) => e.target === kids && done());
  // A render that rebuilds the rows mid-fold drops them and their animationend.
  window.setTimeout(done, 260);
}

/** After a sidebar render: the rows that just opened grow, and their chevron turns. */
export function growOpened() {
  const key = opening;
  opening = null;
  const kids = key ? find("data-kids", key) : null;
  if (!key || !kids) return;
  kids.style.setProperty("--h", `${kids.offsetHeight}px`);
  kids.classList.add("tree-opening");
  kids.addEventListener("animationend", (e) => e.target === kids && kids.classList.remove("tree-opening"));
  const chev = find("data-fold", key)?.querySelector(".chev");
  if (!chev) return;
  chev.classList.add("closed");
  requestAnimationFrame(() => requestAnimationFrame(() => chev.classList.remove("closed")));
}
