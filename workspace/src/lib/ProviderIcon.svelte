<script lang="ts">
  import { Plug } from '@lucide/svelte';
  import { PROVIDER_ICON, catalogIcon, type ProviderName } from './provider-icons';
  import { cn } from '$lib/utils';

  // `name` selects which brand to render. Accepts:
  //   * A `ProviderName` (agent preset like `anthropic` or catalog mark
  //     like `github`) → renders the matching inline brand SVG.
  //   * Any other catalog `icon:` key from `~/.roy/connections.yaml` → if
  //     the key resolves via `catalogIcon`, we render that brand; else we
  //     fall back to a neutral Lucide Plug glyph so the UI still ships an
  //     icon for unmodelled yaml entries.
  // `class` is forwarded so callers control size + color via Tailwind.
  type Props = {
    name: string;
    class?: string;
  };
  let { name, class: cls = 'size-3.5' }: Props = $props();

  // Direct hit on the brand table (covers agent presets AND catalog keys
  // that share the brand id, like `github`). On miss, route through the
  // catalog resolver — currently a single case, but the structure leaves
  // room to alias future yaml keys (e.g. `gh` → `github`).
  const spec = $derived.by(() => {
    const direct = PROVIDER_ICON[name as ProviderName];
    if (direct) return direct;
    const resolved = catalogIcon(name);
    return resolved ? PROVIDER_ICON[resolved] : null;
  });
</script>

{#if spec}
  <svg
    viewBox={spec.viewBox}
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
    class={cn('inline-block', cls)}
  >
    {@html spec.content}
  </svg>
{:else}
  <Plug class={cn('inline-block', cls)} aria-hidden="true" />
{/if}
