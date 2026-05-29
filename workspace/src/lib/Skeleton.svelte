<script lang="ts">
  // Single primitive: a pulse-animated placeholder block. Callers supply
  // their own size + shape via Tailwind classes — same pattern as the
  // shadcn `<Skeleton>` recipe. We keep it inline (no @repo dep) so the
  // bundle stays one file lighter.
  import { cn } from './utils';
  type Props = { class?: string };
  let { class: cls = '' }: Props = $props();
</script>

<div class={cn('animate-pulse rounded-md bg-muted/60', cls)} aria-hidden="true"></div>
