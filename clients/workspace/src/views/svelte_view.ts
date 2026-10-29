import { mount, unmount, type Component } from "svelte";
import type { ViewContext, ViewModule, ViewState } from "./types.ts";

/** A Svelte component as a view module: it mounts with the context as its one prop and keeps its state through
 *  `ctx.keep`, which the module hands back on unmount. */
export function svelteView(component: Component<{ ctx: ViewContext }>): ViewModule {
  let instance: ReturnType<typeof mount> | null = null;
  let kept: ViewState | undefined;
  return {
    mount(el, ctx) {
      kept = ctx.state ?? undefined;
      instance = mount(component, { target: el, props: { ctx: { ...ctx, keep: (state: ViewState) => { kept = state; ctx.keep(state); } } } });
    },
    unmount() {
      if (instance) void unmount(instance);
      instance = null;
      return kept;
    },
  };
}
