// The workspace's one ease curve (design_direction.md, "Material, type, color, motion") for Svelte's list moves, and
// the two moves live data uses: an item that arrives while the page is open rises 6 px into place, and an item that
// leaves fades where it stood while the rest close the gap (`animate:flip`). Transform and opacity only; reduced
// motion removes both.
export const reducedMotion = () => typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

// cubic-bezier(.32, .72, 0, 1), solved for x by Newton's method with a bisection fallback.
const X1 = 0.32, Y1 = 0.72, X2 = 0, Y2 = 1;
const curve = (a1: number, a2: number, t: number) => 3 * a1 * t * (1 - t) ** 2 + 3 * a2 * t ** 2 * (1 - t) + t ** 3;
const slope = (a1: number, a2: number, t: number) => 3 * a1 * (1 - t) ** 2 + 6 * (a2 - a1) * t * (1 - t) + 3 * (1 - a2) * t ** 2;
export function ease(x: number): number {
  if (x <= 0) return 0;
  if (x >= 1) return 1;
  let t = x;
  for (let i = 0; i < 8; i++) {
    const miss = curve(X1, X2, t) - x, s = slope(X1, X2, t);
    if (Math.abs(miss) < 1e-7 || Math.abs(s) < 1e-7) break;
    t -= miss / s;
  }
  if (!(t >= 0 && t <= 1) || Math.abs(curve(X1, X2, t) - x) > 1e-5) {
    let lo = 0, hi = 1;
    for (let i = 0; i < 40; i++) { t = (lo + hi) / 2; if (curve(X1, X2, t) < x) lo = t; else hi = t; }
  }
  return curve(Y1, Y2, t);
}

/** An item that arrived while the page was open. `skip` for the page's first answer, which arrives still. */
export function arrive(_node: Element, { skip = false }: { skip?: boolean } = {}) {
  if (skip || reducedMotion()) return { duration: 0 };
  return { duration: 220, easing: ease, css: (t: number) => `opacity: ${t}; transform: translateY(${((1 - t) * 6).toFixed(2)}px)` };
}
/** An item that left: it fades where it stood. */
export function leave(_node: Element) {
  if (reducedMotion()) return { duration: 0 };
  return { duration: 180, easing: ease, css: (t: number) => `opacity: ${t}` };
}
/** For `animate:flip`: the rest of a list closes the gap in the same curve. */
export const closeGap = () => ({ duration: reducedMotion() ? 0 : 220, easing: ease });
