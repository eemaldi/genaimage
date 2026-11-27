<script lang="ts">
  import { onMount, tick, type Component } from 'svelte';
  import Button from './lib/ui/Button.svelte';
  import Dialog from './lib/ui/Dialog.svelte';
  import ToastRegion from './lib/ui/ToastRegion.svelte';
  import { matchHash } from './router';
  import {
    acceptNavigation,
    initialiseNavigation,
    navigateHash,
  } from './navigation';
  import { api } from './lib/api/client';
  import { isDismissed } from './routes/setup/model';
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

  initialiseNavigation();
  let route = $state(matchHash(window.location.hash));
  let navOpen = $state(false);
  let helpOpen = $state(false);
  let shortcutsEnabled = $state(shortcutsAllowed());
  let main: HTMLElement;
  let header: HTMLElement;
  let navToggle: HTMLButtonElement;
  let AssistantPanel = $state<Component>();
  let assistantOpen = $state(loadAssistantOpen());
  let assistantSide = $state<'left' | 'right'>(loadAssistantSide());
  let assistantLoading = $state(false);
  let section = $derived(
    route.pattern === '/runs/:id' ? '/jobs' : route.pattern?.split('/:')[0],
  );

  function loadAssistantOpen() {
    try {
      return localStorage.getItem('genaimage.assistant.open') === 'true';
    } catch {
      return false;
    }
  }

  function loadAssistantSide(): 'left' | 'right' {
    try {
      return localStorage.getItem('genaimage.assistant.side') === 'left'
        ? 'left'
        : 'right';
    } catch {
      return 'right';
    }
  }

  function persistAssistantOpen(value: boolean) {
    try {
      localStorage.setItem('genaimage.assistant.open', String(value));
    } catch {
      /* Keep the in-memory preference if storage is unavailable. */
    }
  }

  async function loadAssistantPanel() {
    if (AssistantPanel || assistantLoading) return;
    assistantLoading = true;
    try {
      AssistantPanel = (await import('./lib/assistant/AssistantPanel.svelte'))
        .default;
    } finally {
      assistantLoading = false;
    }
  }

  async function toggleAssistant() {
    assistantOpen = !assistantOpen;
    persistAssistantOpen(assistantOpen);
    if (assistantOpen) await loadAssistantPanel();
    else {
      await tick();
      document.getElementById('assistant-toggle')?.focus();
    }
  }

  function closeAssistant() {
    assistantOpen = false;
    persistAssistantOpen(false);
    void tick().then(() =>
      document.getElementById('assistant-toggle')?.focus(),
    );
  }

  function setAssistantSide(side: 'left' | 'right') {
    assistantSide = side;
    try {
      localStorage.setItem('genaimage.assistant.side', side);
    } catch {
      /* Keep the in-memory preference if storage is unavailable. */
    }
  }

  onMount(() => {
    if (assistantOpen) void loadAssistantPanel();
    let mounted = true;
    if (route.path === '/')
      api
        .setupStatus()
        .then((status) => {
          if (!mounted || route.path !== '/') return;
          if (status.needs_setup && !isDismissed(status)) {
            history.replaceState(history.state, '', '#/setup');
            route = matchHash('#/setup');
          }
        })
        .catch(() => {
          /* Setup is only offered when the server says it is needed. */
        });
    return () => {
      mounted = false;
    };
  });

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
    if (!acceptNavigation()) return;
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
      navigateHash(hash);
      if (route.path === path) {
        navOpen = false;
        focusMain();
      }
    },
    help() {
      helpOpen = true;
    },
  });

  function onDocumentClick(event: MouseEvent) {
    if (
      event.defaultPrevented ||
      event.button !== 0 ||
      event.ctrlKey ||
      event.metaKey ||
      event.shiftKey ||
      event.altKey ||
      !(event.target instanceof Element)
    )
      return;
    const link = event.target.closest('a');
    const href = link?.getAttribute('href');
    if (
      !link ||
      !href?.startsWith('#/') ||
      link.hasAttribute('download') ||
      (link.target && link.target !== '_self')
    )
      return;
    event.preventDefault();
    navigateHash(href);
  }

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

<svelte:document onclick={onDocumentClick} />
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
        id="assistant-toggle"
        aria-expanded={assistantOpen && route.path !== '/assistant'}
        aria-controls="assistant-panel"
        onclick={toggleAssistant}>Assistant chat</Button
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
{#if assistantOpen && route.path !== '/assistant'}
  <div id="assistant-panel">
    {#if AssistantPanel}
      <AssistantPanel
        presentation="dock"
        side={assistantSide}
        close={closeAssistant}
        setSide={setAssistantSide}
      />
    {:else if assistantLoading}<p class="visually-hidden" role="status">
        Loading assistant…
      </p>{/if}
  </div>
{/if}
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
