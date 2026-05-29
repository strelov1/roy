<script lang="ts">
  // Bare inline-edit input. Caller decides when editing starts (e.g. on a
  // row's double-click) and renders this component conditionally. We own
  // the input, the draft state, the keyboard handling, and focus restoration
  // back to whatever was active before edit started.
  //
  // Submission rules:
  //   - Enter → call `onSubmit(draft)` (parent decides trim / no-op / save).
  //   - blur  → same as Enter (keeps focus-out behavior intuitive).
  //   - Esc   → call `onCancel()`, no submit.
  //
  // We don't toggle anything ourselves — `onSubmit` and `onCancel` must
  // unmount this component (by setting the parent's "editing" flag false).
  // Without that, blur fires repeatedly when focus moves.

  import { onMount, tick } from 'svelte';

  let {
    value,
    busy = false,
    placeholder,
    ariaLabel,
    inputClass = '',
    onSubmit,
    onCancel,
  }: {
    /** Authoritative current value; pre-fills the input. */
    value: string;
    /** Disable input while a save is in flight so a second submit can't fire. */
    busy?: boolean;
    placeholder?: string;
    ariaLabel?: string;
    /** Extra classes appended to the <input>. */
    inputClass?: string;
    /** Called with the raw draft on Enter / blur. Trimming and no-op
     *  detection are the parent's job. */
    onSubmit: (next: string) => void;
    /** Called on Escape. Parent should set its `editing` flag false. */
    onCancel: () => void;
  } = $props();

  // Snapshot `value` once at mount; the parent owns the authoritative value
  // and won't push fresh ones into us mid-edit. Routing it through $state
  // directly would warn "only captures initial value" — which is exactly
  // what we want, but the warning is noisy. Using a plain reactive variable
  // initialized in onMount is the cleanest way to communicate intent.
  let draft = $state('');
  let inputEl: HTMLInputElement | undefined = $state();
  // Captured before mount so blur/Esc can put focus back where the user came
  // from (typically the row's <button>). Falls back to document.body when
  // there's no active element to restore.
  const returnFocusTo: HTMLElement | null =
    typeof document !== 'undefined'
      ? ((document.activeElement as HTMLElement | null) ?? null)
      : null;

  // Latch: ignore the synthetic blur that fires when Enter / Esc triggers a
  // teardown — without this, commit() runs from blur after we've already
  // run from Enter, double-firing onSubmit (and breaking the no-op guard
  // on the parent if it cleared editing=false during the first call).
  let teardown = false;

  onMount(() => {
    draft = value;
    void tick().then(() => {
      inputEl?.focus();
      inputEl?.select();
    });
  });

  function commit() {
    if (teardown) return;
    teardown = true;
    queueMicrotask(() => returnFocusTo?.focus?.());
    onSubmit(draft);
  }

  function cancel() {
    if (teardown) return;
    teardown = true;
    queueMicrotask(() => returnFocusTo?.focus?.());
    onCancel();
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      e.stopPropagation();
      commit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      cancel();
    }
  }
</script>

<input
  bind:this={inputEl}
  bind:value={draft}
  onkeydown={onKeydown}
  onblur={commit}
  onclick={(e) => e.stopPropagation()}
  ondblclick={(e) => e.stopPropagation()}
  disabled={busy}
  aria-busy={busy}
  aria-label={ariaLabel}
  {placeholder}
  class={[
    'min-w-0 flex-1 bg-transparent text-sm text-foreground placeholder:text-muted-foreground/60 focus:outline-none disabled:opacity-50',
    inputClass,
  ]}
/>
