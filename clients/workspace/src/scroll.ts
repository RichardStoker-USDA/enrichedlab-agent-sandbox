// Moves to an element and keeps it in place while content above it finishes drawing, such as
// figures that open as they come near and grow from their placeholder height. It lets go as
// soon as the reader scrolls, taps or presses a key, or after ten seconds. Returns a function
// that lets go early.
export function holdInView(target: HTMLElement, container: HTMLElement, block: ScrollLogicalPosition = "start"): () => void {
  let moving = !matchMedia("(prefers-reduced-motion: reduce)").matches;
  let placed = 0, glide = 0, cap = 0;
  const inputs = ["wheel", "touchstart", "pointerdown", "keydown"] as const;
  const top = () => target.getBoundingClientRect().top;
  const move = () => {
    target.scrollIntoView({ behavior: moving ? "smooth" : "auto", block });
    if (!moving) placed = top();
  };
  const settled = () => { if (moving) { moving = false; placed = top(); } };
  const ended = (event: Event) => { if (event.target === document || (event.target instanceof Node && event.target.contains(target))) settled(); };
  // Once the move is over, a scroll that shifts the element is the reader's, not a correction.
  const scrolled = () => { if (!moving && Math.abs(top() - placed) > 2) release(); };
  const observer = new ResizeObserver(() => { if (target.isConnected) move(); else release(); });
  function release() {
    observer.disconnect();
    clearTimeout(glide); clearTimeout(cap);
    for (const name of inputs) window.removeEventListener(name, release, true);
    window.removeEventListener("scrollend", ended, true);
    window.removeEventListener("scroll", scrolled, true);
  }
  for (const name of inputs) window.addEventListener(name, release, { capture: true, passive: true });
  window.addEventListener("scrollend", ended, { capture: true });
  window.addEventListener("scroll", scrolled, { capture: true, passive: true });
  // Browsers without scrollend: a smooth move is over well within this.
  glide = window.setTimeout(settled, 900);
  cap = window.setTimeout(release, 10000);
  move();
  observer.observe(container);
  return release;
}
