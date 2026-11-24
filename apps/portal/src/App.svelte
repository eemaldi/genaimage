<script lang="ts">
  import { onMount } from 'svelte';
  import { matchHash } from './router';

  let route = $state(matchHash(window.location.hash));
  onMount(() => {
    const update = () => {
      route = matchHash(window.location.hash);
    };
    window.addEventListener('hashchange', update);
    return () => window.removeEventListener('hashchange', update);
  });
</script>

<header>
  <strong>GenAImage</strong>
  <nav aria-label="Main navigation">
    <a href="#/">Dashboard</a>
    <a href="#/generate">Generate</a>
    <a href="#/jobs">Jobs</a>
    <a href="#/gallery">Gallery</a>
    <a href="#/workflows">Workflows</a>
    <a href="#/assistant">Assistant</a>
    <a href="#/settings">Settings</a>
  </nav>
</header>
<main>
  <route.component params={route.params} query={route.query} />
</main>
