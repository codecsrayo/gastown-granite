// Open-terminal-tabs store (hq-fe-view.11).
//
// Holds the small list of session IDs the dock has open tabs for plus the active tab.
// Lives outside `Dock.svelte` so the Sidebar's "open in dock" button (future) can push
// a session in without a prop-drill from the tree.

class Terminals {
  // Session IDs with an open terminal tab, in the order the user opened them.
  ids = $state<string[]>([]);
  // Currently focused tab. `null` when `ids` is empty.
  active = $state<string | null>(null);

  open(id: string): void {
    if (!this.ids.includes(id)) {
      this.ids = [...this.ids, id];
    }
    this.active = id;
  }

  close(id: string): void {
    const next = this.ids.filter((x) => x !== id);
    this.ids = next;
    if (this.active === id) {
      // Focus the tab that occupied the closed slot (or the last one if we closed the tail).
      this.active = next[next.length - 1] ?? null;
    }
  }

  focus(id: string): void {
    if (this.ids.includes(id)) this.active = id;
  }

  reset(): void {
    this.ids = [];
    this.active = null;
  }
}

export const terminals = new Terminals();
