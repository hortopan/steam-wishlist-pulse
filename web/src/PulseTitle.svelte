<script lang="ts">
  let { tag = 'h1', class: className = '' }: { tag?: string; class?: string } = $props();
</script>

<svelte:element this={tag} class="pulse-title {className}">
  <span class="word">Wishlist</span>
  <span class="pulse-line-wrap">
    <svg class="pulse-line" viewBox="0 0 200 24" preserveAspectRatio="none">
      <defs>
        <linearGradient id="pulseGrad" x1="0%" y1="0%" x2="100%" y2="0%">
          <stop offset="0%" stop-color="#22c55e" stop-opacity="0.2" />
          <stop offset="30%" stop-color="#4ade80" stop-opacity="1" />
          <stop offset="70%" stop-color="#22c55e" stop-opacity="1" />
          <stop offset="100%" stop-color="#22c55e" stop-opacity="0.2" />
        </linearGradient>
        <clipPath id="pulseClip">
          <rect x="0" y="0" width="200" height="24" />
        </clipPath>
      </defs>
      <g clip-path="url(#pulseClip)">
        <path
          class="pulse-path"
          d="M-100,12 L-85,12 L-80,4 L-72,20 L-64,4 L-56,20 L-48,4 L-40,12 L0,12 L15,12 L20,4 L28,20 L36,4 L44,20 L52,4 L60,12 L100,12 L115,12 L120,4 L128,20 L136,4 L144,20 L152,4 L160,12 L200,12 L215,12 L220,4 L228,20 L236,4 L244,20 L252,4 L260,12 L300,12"
          fill="none"
          stroke="url(#pulseGrad)"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </g>
    </svg>
    <svg class="pulse-line pulse-glow" viewBox="0 0 200 24" preserveAspectRatio="none">
      <defs>
        <clipPath id="pulseClipGlow">
          <rect x="0" y="0" width="200" height="24" />
        </clipPath>
      </defs>
      <g clip-path="url(#pulseClipGlow)">
        <path
          class="pulse-path"
          d="M-100,12 L-85,12 L-80,4 L-72,20 L-64,4 L-56,20 L-48,4 L-40,12 L0,12 L15,12 L20,4 L28,20 L36,4 L44,20 L52,4 L60,12 L100,12 L115,12 L120,4 L128,20 L136,4 L144,20 L152,4 L160,12 L200,12 L215,12 L220,4 L228,20 L236,4 L244,20 L252,4 L260,12 L300,12"
          fill="none"
          stroke="#4ade80"
          stroke-width="4"
          stroke-linecap="round"
          stroke-linejoin="round"
          opacity="0.25"
        />
      </g>
    </svg>
  </span>
  <span class="word">Pulse</span>
</svelte:element>

<style>
  .pulse-title {
    display: inline-flex;
    align-items: center;
    gap: 0;
    font-weight: 700;
    margin: 0;
  }

  .word {
    background: linear-gradient(135deg, var(--accent, #d97706), var(--accent-glow, #f59e0b));
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
    background-clip: text;
  }

  .pulse-line-wrap {
    position: relative;
    width: 3em;
    height: 1.2em;
    display: inline-flex;
    align-items: center;
    flex-shrink: 0;
    overflow: hidden;
  }

  .pulse-line {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
  }

  .pulse-glow {
    filter: blur(3px);
  }

  .pulse-path {
    animation: pulse-scroll 2s linear infinite;
  }

  @keyframes pulse-scroll {
    0% {
      transform: translateX(0);
    }
    100% {
      transform: translateX(-100px);
    }
  }
</style>
