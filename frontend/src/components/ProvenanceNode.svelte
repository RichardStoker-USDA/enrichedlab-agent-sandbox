<script>
  import { Handle, Position } from '@xyflow/svelte';

  let { data, selected = false } = $props();
</script>

<Handle type="target" position={Position.Left} aria-hidden="true" tabindex="-1" />
<article class:selected class:artifact={data.raw.source === 'artifact'} title={`${data.label} · ${data.raw.actor}`}>
  <span class="node-role">{data.raw.source === 'artifact' ? 'Artifact' : data.raw.evidence_role}</span>
  <strong>{data.label}</strong>
  <span class="node-meta">{data.raw.actor}</span>
  <span class="node-time">{data.clock}</span>
</article>
<Handle type="source" position={Position.Right} aria-hidden="true" tabindex="-1" />

<style>
  article {
    width: 196px;
    display: grid;
    gap: 5px;
    padding: 13px 14px;
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    background: var(--surface);
    box-shadow: 0 8px 26px color-mix(in srgb, var(--shadow) 10%, transparent);
    color: var(--text);
  }

  article.selected {
    border-color: var(--ucd-gold);
    outline: 2px solid color-mix(in srgb, var(--ucd-gold) 38%, transparent);
    outline-offset: 2px;
  }

  article.artifact {
    border-style: dashed;
  }

  strong {
    font-size: 12px;
    line-height: 1.35;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  .node-role {
    color: var(--muted);
    font-size: 9px;
    font-weight: 760;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .node-meta,
  .node-time {
    color: var(--muted);
    font-size: 10px;
    overflow-wrap: anywhere;
  }
</style>
