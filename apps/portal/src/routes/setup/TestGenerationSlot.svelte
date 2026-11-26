<script lang="ts">
  import type { TestGenerationState } from './model';

  let { state: _state }: { state: TestGenerationState } = $props();
</script>

<!-- Component C (test generation) replaces the body of this slot. -->
<div data-slot="test-generation">
  <p>
    A test generation is not available yet. It is planned as the last step of
    this wizard and will not run without your confirmation.
  </p>
</div>
