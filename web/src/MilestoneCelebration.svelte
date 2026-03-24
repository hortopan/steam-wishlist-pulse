<script lang="ts">
  import type { GameDetailResponse } from './types';
  import { formatDate } from './utils';
  import LogoBrand from './LogoBrand.svelte';

  let { detail, onClose }: { detail: GameDetailResponse; onClose: () => void } = $props();

  let netWishlists = $derived(
    detail.latest
      ? detail.latest.total_adds - detail.latest.total_deletes - detail.latest.total_purchases - detail.latest.total_gifts
      : 0
  );

  let todayNet = $derived(
    detail.latest
      ? detail.latest.adds - detail.latest.deletes - detail.latest.purchases - detail.latest.gifts
      : 0
  );

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') onClose();
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_interactive_supports_focus -->
<div class="celebration" role="dialog" aria-label="Milestone celebration" onclick={(e) => { if (e.target === e.currentTarget) onClose(); }}>
  <!-- Blurred background -->
  {#if detail.image_url}
    <div class="celebration-bg" style:background-image="url({detail.image_url})"></div>
  {/if}

  <!-- Close button -->
  <button class="close-btn" onclick={onClose} aria-label="Close milestone view">
    <svg width="20" height="20" viewBox="0 0 20 20" fill="none">
      <path d="M15 5L5 15M5 5L15 15" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
    </svg>
  </button>

  <!-- Branding -->
  <div class="branding">
    <LogoBrand />
  </div>

  <!-- Content -->
  <div class="celebration-content">
    <!-- Game image -->
    {#if detail.image_url}
      <div class="game-image-wrap">
        <img class="game-image" src={detail.image_url} alt={detail.name} />
      </div>
    {/if}

    <!-- Game title -->
    <h2 class="game-title">{detail.name}</h2>

    <!-- Hero number -->
    <div class="hero-number">{netWishlists.toLocaleString()}</div>
    <div class="hero-label">Wishlists</div>

    <!-- Pulse line separator -->
    <div class="pulse-separator">
      <svg class="pulse-line" viewBox="0 0 200 16" preserveAspectRatio="none">
        <defs>
          <linearGradient id="celebPulseGrad" x1="0%" y1="0%" x2="100%" y2="0%">
            <stop offset="0%" stop-color="#d97706" stop-opacity="0.15" />
            <stop offset="30%" stop-color="#f59e0b" stop-opacity="1" />
            <stop offset="70%" stop-color="#d97706" stop-opacity="1" />
            <stop offset="100%" stop-color="#d97706" stop-opacity="0.15" />
          </linearGradient>
          <clipPath id="celebPulseClip">
            <rect x="0" y="0" width="200" height="16" />
          </clipPath>
        </defs>
        <g clip-path="url(#celebPulseClip)">
          <path
            class="pulse-path"
            d="M-100,8 L-85,8 L-80,2 L-72,14 L-64,2 L-56,14 L-48,2 L-40,8 L0,8 L15,8 L20,2 L28,14 L36,2 L44,14 L52,2 L60,8 L100,8 L115,8 L120,2 L128,14 L136,2 L144,14 L152,2 L160,8 L200,8 L215,8 L220,2 L228,14 L236,2 L244,14 L252,2 L260,8 L300,8"
            fill="none"
            stroke="url(#celebPulseGrad)"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </g>
      </svg>
      <svg class="pulse-line pulse-glow" viewBox="0 0 200 16" preserveAspectRatio="none">
        <defs>
          <clipPath id="celebPulseClipGlow">
            <rect x="0" y="0" width="200" height="16" />
          </clipPath>
        </defs>
        <g clip-path="url(#celebPulseClipGlow)">
          <path
            class="pulse-path"
            d="M-100,8 L-85,8 L-80,2 L-72,14 L-64,2 L-56,14 L-48,2 L-40,8 L0,8 L15,8 L20,2 L28,14 L36,2 L44,14 L52,2 L60,8 L100,8 L115,8 L120,2 L128,14 L136,2 L144,14 L152,2 L160,8 L200,8 L215,8 L220,2 L228,14 L236,2 L244,14 L252,2 L260,8 L300,8"
            fill="none"
            stroke="#f59e0b"
            stroke-width="3"
            stroke-linecap="round"
            stroke-linejoin="round"
            opacity="0.2"
          />
        </g>
      </svg>
    </div>

    <!-- Secondary stats -->
    {#if detail.latest}
      <div class="stats-row">
        <div class="stat-pill stat-today">
          <span class="stat-value">{todayNet >= 0 ? '+' : ''}{todayNet.toLocaleString()}</span>
          <span class="stat-label">Today</span>
        </div>
        <span class="stat-dot">&middot;</span>
        <div class="stat-pill stat-purchases">
          <span class="stat-value">{detail.latest.total_purchases.toLocaleString()}</span>
          <span class="stat-label">Purchases</span>
        </div>
        <span class="stat-dot">&middot;</span>
        <div class="stat-pill stat-gifts">
          <span class="stat-value">{detail.latest.total_gifts.toLocaleString()}</span>
          <span class="stat-label">Gifts</span>
        </div>
      </div>

      <div class="date-line">{formatDate(detail.latest.date)}</div>
    {/if}
  </div>
</div>

<style>
  .celebration {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    background: var(--bg, #0d0e12);
    overflow: hidden;
  }

  .celebration-bg {
    position: absolute;
    inset: -20px;
    background-size: cover;
    background-position: center;
    filter: blur(12px) brightness(0.2);
    transform: scale(1.1);
    z-index: 0;
  }

  .close-btn {
    position: absolute;
    top: 1.5rem;
    left: 1.5rem;
    z-index: 2;
    background: rgba(255, 255, 255, 0.08);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 50%;
    width: 2.5rem;
    height: 2.5rem;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-muted, #8a8578);
    cursor: pointer;
    transition: background 0.2s, color 0.2s;
  }

  .close-btn:hover {
    background: rgba(255, 255, 255, 0.15);
    color: var(--text, #e1ddd6);
  }

  .branding {
    position: absolute;
    top: 1.25rem;
    right: 1.5rem;
    z-index: 2;
  }

  .celebration-content {
    position: relative;
    z-index: 1;
    text-align: center;
    padding: 2rem;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.25rem;
    max-width: 640px;
    width: 100%;
  }

  .game-image-wrap {
    margin-bottom: 1rem;
    border-radius: 0.75rem;
    overflow: hidden;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5), 0 0 0 1px rgba(255, 255, 255, 0.06);
    max-width: 460px;
    width: 80%;
  }

  .game-image {
    display: block;
    width: 100%;
    height: auto;
  }

  .game-title {
    font-size: clamp(1.1rem, 3vw, 1.5rem);
    font-weight: 700;
    color: var(--text, #e1ddd6);
    margin: 0.5rem 0 0.25rem;
    letter-spacing: 0.01em;
  }

  .hero-number {
    font-size: clamp(3.5rem, 10vw, 7rem);
    font-weight: 800;
    color: #fff;
    line-height: 1;
    text-shadow: 0 0 60px rgba(217, 119, 6, 0.25), 0 0 120px rgba(217, 119, 6, 0.1);
    margin-top: 0.25rem;
  }

  .hero-label {
    font-size: clamp(0.9rem, 2vw, 1.3rem);
    letter-spacing: 0.25em;
    text-transform: uppercase;
    color: var(--accent, #d97706);
    font-weight: 600;
    margin-bottom: 0.5rem;
  }

  .pulse-separator {
    position: relative;
    width: 60%;
    max-width: 300px;
    height: 0.75rem;
    overflow: hidden;
    margin: 0.5rem 0 1rem;
  }

  .pulse-line {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
  }

  .pulse-glow {
    filter: blur(2px);
  }

  .pulse-path {
    animation: celebPulseScroll 2s linear infinite;
  }

  @keyframes celebPulseScroll {
    0% { transform: translateX(0); }
    100% { transform: translateX(-100px); }
  }

  .stats-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
    justify-content: center;
  }

  .stat-pill {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.15rem;
  }

  .stat-value {
    font-size: 1.1rem;
    font-weight: 700;
  }

  .stat-label {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted, #8a8578);
    font-weight: 500;
  }

  .stat-today .stat-value { color: var(--green, #22c55e); }
  .stat-purchases .stat-value { color: var(--blue, #3b82f6); }
  .stat-gifts .stat-value { color: var(--amber, #f59e0b); }

  .stat-dot {
    color: var(--text-muted, #8a8578);
    font-size: 1.2rem;
    opacity: 0.4;
  }

  .date-line {
    margin-top: 1rem;
    font-size: 0.8rem;
    color: var(--text-muted, #8a8578);
    letter-spacing: 0.03em;
  }

  @media (max-width: 640px) {
    .celebration-content {
      padding: 1.5rem 1rem;
    }

    .game-image-wrap {
      width: 90%;
    }

    .branding {
      top: 1rem;
      right: 1rem;
    }

    .close-btn {
      top: 1rem;
      left: 1rem;
    }
  }
</style>
