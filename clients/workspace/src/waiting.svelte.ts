import { inbox, itemKey, type InboxItem } from "./inbox.ts";
import { serial } from "./live.ts";

// What waits for the reader, read once for the whole workspace: the sidebar count, the tab title, home's list and the
// Waiting for you page show the same answer. The shell reads it again when the inbox changes (live.ts); a read while
// one runs waits for it and reads once more, so an older answer never replaces a newer one. A failed read keeps the
// list the broker last gave.
class Waiting {
  items = $state<InboxItem[]>([]);
  canAcknowledge = $state(true);
  phase = $state<"idle" | "ready" | "failed">("idle");
  /** When the list was last read, for a page that opens on a copy that may be old (no change stream). */
  readAt = 0;

  readonly load = serial(async () => {
    try {
      const next = await inbox.list();
      this.items = next.items;
      this.canAcknowledge = next.can_acknowledge;
      this.phase = "ready";
      this.readAt = Date.now();
    } catch (cause) {
      if (this.phase !== "ready") this.phase = "failed";
      throw cause;
    }
  });

  /** The reader marked an item as seen; the list shows it at once. */
  seen(item: InboxItem, at: string) {
    const key = itemKey(item);
    this.items = this.items.map((i) => (itemKey(i) === key ? { ...i, seen_at: at } : i));
  }
}

export const waiting = new Waiting();
