<script lang="ts">
  import type { QuotaRotation } from '$lib/types/quota';
  import { resetCountdown } from '$lib/quota-meter';

  interface Props {
    rotation: QuotaRotation;
    /** Pinned clock (for tests). */
    nowSecs?: number;
    /** Tail cap on the rotations row. The sidebar is narrow so default is small. */
    rotationsLimit?: number;
  }

  let { rotation, nowSecs, rotationsLimit = 4 }: Props = $props();

  let visibleRotations = $derived(rotation.recent_rotations.slice(-rotationsLimit).reverse());
</script>

{#if rotation.waiting_unlock.length === 0 && rotation.recent_rotations.length === 0}
  <p
    class="font-mono text-[10px]"
    style="color: var(--ink-faint)"
    data-testid="rotation-empty"
  >
    no rotations
  </p>
{:else}
  <section class="flex flex-col gap-1" data-testid="rotation-chips">
    {#if rotation.waiting_unlock.length > 0}
      <ul class="flex flex-wrap gap-1">
        {#each rotation.waiting_unlock as row (row.account)}
          {@const cd = resetCountdown(row.unlock_at_secs, nowSecs)}
          <li
            class="rounded border px-1.5 py-0.5 font-mono text-[10px]"
            style:border-color="var(--warn)"
            style:color="var(--warn)"
            style:background="var(--warn-soft)"
            title={`${row.account} cooldown · unlocks ${cd || 'eventually'}`}
          >
            {row.account}{cd ? ` · ${cd}` : ''}
          </li>
        {/each}
      </ul>
    {/if}
    {#if visibleRotations.length > 0}
      <ul
        class="flex flex-col gap-px font-mono text-[10px]"
        style="color: var(--ink-faint)"
      >
        {#each visibleRotations as rot (rot.ts)}
          <li title={`rotated at ${rot.ts}`}>
            {rot.from}
            <span style="color: var(--ink-faint)">→</span>
            {rot.to}
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{/if}
