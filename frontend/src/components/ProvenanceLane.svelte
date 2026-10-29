<script>
  let { data } = $props();
</script>

<div class="lane" style={`--lane-span:${data.span}px`}>
  <strong>{data.label}</strong>
  <span>{data.description}</span>
</div>

<style>
  .lane {
    position: relative;
    width: 172px;
    min-height: 48px;
    display: grid;
    align-content: center;
    gap: 3px;
    padding: 8px 12px;
    border: 1px solid var(--line-strong);
    border-radius: 7px;
    background: var(--surface);
    color: var(--text);
    pointer-events: none;
  }

  .lane::after {
    content: '';
    position: absolute;
    z-index: -1;
    top: 50%;
    left: calc(100% + 10px);
    width: var(--lane-span);
    border-top: 1px solid var(--flow-lane);
  }

  strong {
    font-size: 10px;
    letter-spacing: 0.07em;
    text-transform: uppercase;
  }

  span {
    color: var(--muted);
    font-size: 9px;
    line-height: 1.3;
  }
</style>
