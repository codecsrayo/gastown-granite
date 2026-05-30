<script lang="ts">
  // Per-account Login button. The actual PTY-driven `claude /login` flow is
  // hq-fe-auth.{1,2,3,4} (open). Until those land, the button surfaces the
  // affordance in the sidebar but stays disabled with a tooltip pointing at
  // the blocking bead — so operators see where the button will appear and the
  // QA path lights up the moment auth.* ships.

  interface Props {
    account: string;
    disabled?: boolean;
  }

  let { account, disabled = true }: Props = $props();
</script>

<button
  type="button"
  class="rounded border px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide transition-colors"
  style:border-color="var(--border)"
  style:color={disabled ? 'var(--ink-faint)' : 'var(--accent)'}
  style:background="transparent"
  {disabled}
  title={disabled
    ? `Login wiring pending hq-fe-auth.1 (PTY driver) for ${account}`
    : `Open Anthropic login for ${account}`}
  data-testid="quota-login-btn"
  data-account={account}
>
  Login
</button>
