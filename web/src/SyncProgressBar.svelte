<script lang="ts">
  import type { SyncStatus } from "./types";

  let { syncStatus, variant = "inline" }: { syncStatus: SyncStatus; variant?: "hero" | "inline" } = $props();

  let percent = $derived(
    syncStatus.progress_total > 0
      ? Math.min(100, Math.round((syncStatus.progress_crawled / syncStatus.progress_total) * 100))
      : 0
  );
</script>

<div class="sync-progress" class:hero={variant === "hero"}>
  <span class="sync-label">{syncStatus.progress_crawled}/{syncStatus.progress_total} days synced</span>
  <div class="sync-bar">
    <div class="sync-fill" style="width: {percent}%"></div>
  </div>
</div>

<style>
  .sync-progress {
    font-size: 0.75rem;
    color: var(--accent);
  }

  .sync-progress.hero {
    margin-top: 0.4rem;
  }

  .sync-progress.hero .sync-label {
    text-shadow:
      -1px -1px 0 rgba(0, 0, 0, 0.7),
       1px -1px 0 rgba(0, 0, 0, 0.7),
      -1px  1px 0 rgba(0, 0, 0, 0.7),
       1px  1px 0 rgba(0, 0, 0, 0.7);
  }

  .sync-bar {
    width: 200px;
    height: 4px;
    background: rgba(255, 255, 255, 0.2);
    border-radius: 2px;
    margin-top: 0.25rem;
    overflow: hidden;
  }

  .sync-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 2px;
    transition: width 0.5s ease;
  }
</style>
