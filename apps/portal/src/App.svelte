<script lang="ts">
  import { tick } from 'svelte';
  import Button from './lib/ui/Button.svelte';
  import Dialog from './lib/ui/Dialog.svelte';
  import ToastRegion from './lib/ui/ToastRegion.svelte';
  import { matchHash } from './router';
  import { createShortcutHandler, sections, shortcuts } from './shortcuts';
  import './styles/tokens.css';
  import './styles/ui.css';

  const shortcutPreference = 'genaimage.disable-shortcuts';
  function shortcutsAllowed() {
    try {
      return localStorage.getItem(shortcutPreference) !== 'true';
    } catch {
      return true;
    }
  }

  let route = $state(matchHash(window.location.hash));
  let navOpen = $state(false);
  let helpOpen = $state(false);
  let shortcutsEnabled = $state(shortcutsAllowed());
  let main: HTMLElement;
  let header: HTMLElement;
  let navToggle: HTMLButtonElement;
  let section = $derived(
    route.pattern === '/runs/:id' ? '/jobs' : route.pattern?.split('/:')[0],
  );

  function setShortcutPreference(
    event: Event & { currentTarget: HTMLInputElement },
  ) {
    shortcutsEnabled = event.currentTarget.checked;
    keyboard.reset();
    try {
      localStorage.setItem(shortcutPreference, String(!shortcutsEnabled));
    } catch {
      /* Keep the in-memory preference if storage is unavailable. */
    }
  }

  function focusMain() {
    main.focus();
  }

  async function updateRoute() {
    const next = matchHash(window.location.hash);
    const changed = next.path !== route.path;
    route = next;
    if (changed) {
      navOpen = false;
      await tick();
      if (!document.querySelector('dialog[open]')) focusMain();
    }
  }

  const keyboard = createShortcutHandler({
    navigate(path) {
      const hash = `#${path}`;
      if (window.location.hash !== hash) window.location.hash = hash;
      if (route.path === path) {
        navOpen = false;
        focusMain();
      }
    },
    help() {
      helpOpen = true;
    },
  });

  function onKeydown(event: KeyboardEvent) {
    if (event.target instanceof Node && header.contains(event.target))
      closeNav(event);
    if (!shortcutsEnabled || helpOpen || document.querySelector('dialog[open]'))
      keyboard.reset();
    else keyboard.handle(event);
  }

  function closeNav(event: KeyboardEvent) {
    if (
      event.key === 'Escape' &&
      navOpen &&
      !event.defaultPrevented &&
      !helpOpen &&
      window.matchMedia('(max-width: 48rem)').matches
    ) {
      navOpen = false;
      navToggle.focus();
      event.preventDefault();
    }
  }
</script>

<svelte:window
  onhashchange={updateRoute}
  onkeydown={onKeydown}
  onblur={keyboard.reset}
/>

<a
  class="skip-link"
  href="#main-content"
  onclick={(event) => {
    event.preventDefault();
    focusMain();
  }}>Skip to main content</a
>
<header class="app-header" bind:this={header}>
  <div class="app-masthead">
    <strong class="wordmark">GenAImage</strong>
    <div class="app-actions">
      <button
        class="ui-button nav-toggle"
        type="button"
        bind:this={navToggle}
        aria-expanded={navOpen}
        aria-controls="main-navigation"
        onclick={() => (navOpen = !navOpen)}>Menu</button
      >
      <Button
        onclick={() => {
          keyboard.reset();
          helpOpen = true;
        }}>Keyboard shortcuts</Button
      >
    </div>
  </div>
  <nav
    id="main-navigation"
    class:expanded={navOpen}
    aria-label="Main navigation"
  >
    <ul>
      {#each sections as item (item.path)}
        <li>
          <a
            href={`#${item.path}`}
            aria-label={'planned' in item ? `${item.label} Planned` : undefined}
            aria-current={section === item.path
              ? route.path === item.path
                ? 'page'
                : 'location'
              : undefined}
            onclick={(event) => {
              if (
                !event.ctrlKey &&
                !event.metaKey &&
                !event.shiftKey &&
                !event.altKey &&
                route.path === item.path
              ) {
                navOpen = false;
                focusMain();
              }
            }}
          >
            {item.label}{#if 'planned' in item}
              <span class="planned">Planned</span>{/if}
          </a>
        </li>
      {/each}
    </ul>
  </nav>
</header>
<main id="main-content" tabindex="-1" bind:this={main}>
  <div class="app-page">
    <route.component params={route.params} query={route.query} />
  </div>
</main>
<ToastRegion returnFocus={focusMain} />
<Dialog bind:open={helpOpen} title="Keyboard shortcuts" returnFocus={focusMain}>
  <p>For two-key shortcuts, press the keys in order, not together.</p>
  <dl class="shortcut-list">
    {#each shortcuts as shortcut (shortcut.label)}
      <dt>
        {#each shortcut.keys as key, index (index)}<kbd>{key}</kbd>{/each}
      </dt>
      <dd>{shortcut.label}</dd>
    {/each}
  </dl>
  <label class="shortcut-preference"
    ><input
      type="checkbox"
      checked={shortcutsEnabled}
      onchange={setShortcutPreference}
    /> Enable character-key shortcuts</label
  >
</Dialog>
