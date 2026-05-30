<script lang="ts">
  // hq-fe-view.8 — left rail listing the six canonical Gas Town roles
  // (frontend-features.md §8). The full `GET /api/roles` payload (with skills +
  // scope) lands with hq-fe-skills.2; until then we render the static catalog
  // and annotate each row with the live session count derived from
  // `/api/sessions`. Selecting a row drives `RolePanel` on the right.

  type Props = {
    roles: string[];
    counts: Record<string, number>;
    selected: string;
    onselect: (role: string) => void;
  };

  let { roles, counts, selected, onselect }: Props = $props();

  function roleColor(role: string): string {
    if (role === 'mayor') return 'var(--warn)';
    if (role === 'polecat') return 'var(--ink-soft)';
    return 'var(--accent)';
  }
</script>

<ul class="flex flex-col gap-px" aria-label="Roles">
  {#each roles as role (role)}
    {@const active = role === selected}
    <li>
      <button
        type="button"
        class="flex w-full items-baseline justify-between rounded px-3 py-2 font-mono text-sm transition-colors"
        style:color={active ? roleColor(role) : 'var(--ink-soft)'}
        style:background={active ? 'var(--accent-soft)' : 'transparent'}
        aria-current={active ? 'true' : undefined}
        onclick={() => onselect(role)}
      >
        <span>{role}</span>
        <span class="text-[10px]" style="color: var(--ink-faint)">
          {counts[role] ?? 0}
        </span>
      </button>
    </li>
  {/each}
</ul>
