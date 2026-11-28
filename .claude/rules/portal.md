---
paths:
  - "apps/portal/**"
---

# Portal rules

- **Exact numbers.** `parseBindingJson` keeps large integers, whole floats such
  as `2.0` and `-0.0`, but not the spelling of other floats (`0.10` becomes
  `0.1`). Use `parseAssistantJson` or `readAssistantJson` where every number's
  text must survive. Write numeric bodies with `stringifyBindingJson` or
  `formatExactJson`, never `JSON.stringify`, and refuse to send when
  `supportsExactNumberWrites()` is false.
- **Svelte state.** `$state` proxies plain objects, so keep exact server values
  (`BindingFloat`, `bigint`) in a non-reactive map. Reading a `SvelteMap` in
  `$effect` tracks it; wrap side-effecting loads in `untrack`.
- **Focus.** Disabling a focused button drops focus: use `keepFocusAcross`.
  Pass focus targets as getters (`focusAfterUpdate(() => element)`). Move focus
  when a focused control unmounts.
- **Stale responses.** Use an `AbortController` per operation or a monotonic
  sequence number. Never reset a sequence counter in `clear()`.
- **Availability.** When an API list gains an unavailable state, update every
  consumer of that list, not only the page being changed.
- **Tests.** Add one real-client test (`createApiClient(fakeFetch)`) per new
  submission endpoint that asserts headers and exact body bytes. The jsdom
  `Dialog` mock never fires `close`, so check dialogs and focus in a real
  browser too (headless Playwright). `getByText` also matches `<option>` text.
  After rebuilding the portal, `touch crates/genaimage-server/build.rs` so the
  server re-embeds it.
