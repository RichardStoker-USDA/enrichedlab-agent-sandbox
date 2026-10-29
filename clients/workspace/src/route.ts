// Hash routes carry only identifiers. Drafts, answers and research content never enter a URL.
const UUID = "[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}";

export type Route =
  | { name: "home"; lab?: string }
  | { name: "project"; project: string; section?: string }
  | { name: "analysis"; task: string; project?: string }
  | { name: "dataset"; project: string; dataset: string }
  | { name: "analyses" }
  | { name: "administration" }
  | { name: "notes" }
  | { name: "waiting"; labs?: string[] }
  | { name: "working" }
  | { name: "library" }
  | { name: "commons" }
  | { name: "tools" }
  | { name: "paper"; paper: string }
  | { name: "workbench"; project?: string };

export function parseRoute(hash: string): Route {
  const value = hash.replace(/^#/, "");
  let match: RegExpExecArray | null;
  // Links saved before this layout pointed at a bare task identifier.
  if ((match = new RegExp(`^(${UUID})$`, "i").exec(value))) return { name: "analysis", task: match[1].toLowerCase() };
  if ((match = new RegExp(`^/projects/(${UUID})/analyses/(${UUID})$`, "i").exec(value)))
    return { name: "analysis", project: match[1].toLowerCase(), task: match[2].toLowerCase() };
  // A table the lab sent to the project, on its own page.
  if ((match = new RegExp(`^/projects/(${UUID})/data/(${UUID})$`, "i").exec(value)))
    return { name: "dataset", project: match[1].toLowerCase(), dataset: match[2].toLowerCase() };
  if ((match = new RegExp(`^/projects/(${UUID})(?:/([a-z]+))?$`, "i").exec(value)))
    return match[2] ? { name: "project", project: match[1].toLowerCase(), section: match[2] } : { name: "project", project: match[1].toLowerCase() };
  if ((match = new RegExp(`^/analyses/(${UUID})$`, "i").exec(value))) return { name: "analysis", task: match[1].toLowerCase() };
  if ((match = new RegExp(`^/workbench/projects/(${UUID})$`, "i").exec(value))) return { name: "workbench", project: match[1].toLowerCase() };
  if (value === "/workbench") return { name: "workbench" };
  if (value === "/analyses") return { name: "analyses" };
  if (value === "/administration") return { name: "administration" };
  if (value === "/notes") return { name: "notes" };
  if (value === "/waiting") return { name: "waiting" };
  // What waits in the labs a widened index showed, so its "All in Waiting for you" opens the same labs.
  if ((match = /^\/waiting\/labs\/([a-z0-9][a-z0-9_-]{0,62}(?:,[a-z0-9][a-z0-9_-]{0,62}){0,15})$/.exec(value)))
    return { name: "waiting", labs: [...new Set(match[1].split(","))] };
  if (value === "/working") return { name: "working" };
  if (value === "/library") return { name: "library" };
  if (value === "/commons") return { name: "commons" };
  if (value === "/tools") return { name: "tools" };
  if ((match = new RegExp(`^/library/papers/(${UUID})$`, "i").exec(value))) return { name: "paper", paper: match[1].toLowerCase() };
  // A lab's own page, by the key the broker gives the lab.
  if ((match = /^\/labs\/([a-z0-9][a-z0-9_-]{0,62})$/.exec(value))) return { name: "home", lab: match[1] };
  return { name: "home" };
}

export function href(route: Route): string {
  switch (route.name) {
    case "home": return route.lab ? `#/labs/${route.lab}` : "#/";
    case "project": return route.section ? `#/projects/${route.project}/${route.section}` : `#/projects/${route.project}`;
    case "analysis": return route.project ? `#/projects/${route.project}/analyses/${route.task}` : `#/analyses/${route.task}`;
    case "dataset": return `#/projects/${route.project}/data/${route.dataset}`;
    case "analyses": return "#/analyses";
    case "administration": return "#/administration";
    case "notes": return "#/notes";
    case "waiting": return route.labs?.length ? `#/waiting/labs/${route.labs.join(",")}` : "#/waiting";
    case "working": return "#/working";
    case "library": return "#/library";
    case "commons": return "#/commons";
    case "tools": return "#/tools";
    case "paper": return `#/library/papers/${route.paper}`;
    case "workbench": return route.project ? `#/workbench/projects/${route.project}` : "#/workbench";
  }
}

// Change the page inside a view transition when the browser supports one and the
// person has not asked for reduced motion. The update itself never waits on animation.
// The transition runs its update a moment later; if another navigation has happened by
// then, the older one stands down instead of taking the reader back.
let latest = 0;
export function navigate(route: Route, update: () => void | Promise<void>) {
  const target = href(route);
  const mine = ++latest;
  const from = location.hash;
  const apply = () => {
    if (mine !== latest || location.hash !== from) return;
    if (location.hash !== target) history.pushState(null, "", target);
    return update();
  };
  const reduced = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  const start = (document as Document & { startViewTransition?: ViewTransitionStart }).startViewTransition;
  // A hidden tab gets no transition. One skipped later (a newer one began) still runs its update; only the
  // animation is dropped, so its rejected `ready` is not an error.
  if (!reduced && document.visibilityState !== "hidden" && typeof start === "function") start.call(document, apply)?.ready?.catch(() => {});
  else void apply();
}

export type ViewTransitionStart = (update: () => void | Promise<void>) => { ready?: Promise<unknown> } | undefined;
