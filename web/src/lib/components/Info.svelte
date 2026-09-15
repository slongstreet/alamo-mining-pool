<script lang="ts">
  /**
   * An (i) glyph with an explainer. Shows on hover and keyboard focus like a title, and
   * toggles on tap so it also works on touch screens, where hover does not exist.
   */
  let { text, align = 'center' }: { text: string; align?: 'center' | 'right' } = $props();

  let open = $state(false);
</script>

<span class="info" class:open class:right={align === 'right'}>
  <button
    type="button"
    class="glyph"
    aria-label="More information"
    aria-expanded={open}
    onclick={(e) => {
      e.stopPropagation();
      open = !open;
    }}
    onblur={() => (open = false)}
    onkeydown={(e) => e.key === 'Escape' && (open = false)}
  >
    i
  </button>
  <span class="tip" role="tooltip">{text}</span>
</span>

<style>
  .info {
    position: relative;
    display: inline-block;
    margin-left: 0.3rem;
    vertical-align: 0.1em;
    line-height: 1;
  }
  .glyph {
    all: unset;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1em;
    height: 1em;
    border-radius: 50%;
    border: 1px solid var(--muted);
    color: var(--muted);
    font-size: 0.7em;
    font-style: italic;
    font-family: Georgia, serif;
    cursor: help;
  }
  .glyph:hover,
  .glyph:focus-visible,
  .open .glyph {
    border-color: var(--text);
    color: var(--text);
  }
  .glyph:focus-visible {
    outline: 2px solid var(--accent-2);
    outline-offset: 1px;
  }
  .tip {
    display: none;
    position: absolute;
    z-index: 10;
    left: 50%;
    bottom: calc(100% + 0.45rem);
    transform: translateX(-50%);
    width: max-content;
    max-width: min(20rem, 80vw);
    padding: 0.5rem 0.65rem;
    background: var(--panel-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow);
    font-size: 0.78rem;
    font-weight: 400;
    font-style: normal;
    line-height: 1.4;
    text-align: left;
    text-transform: none;
    letter-spacing: normal;
    white-space: normal;
  }
  .right .tip {
    left: auto;
    right: 0;
    transform: none;
  }
  .info:hover .tip,
  .glyph:focus-visible + .tip,
  .open .tip {
    display: block;
  }
</style>
