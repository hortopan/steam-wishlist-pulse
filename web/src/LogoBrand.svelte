<script lang="ts">
  import logoSrc from './assets/logo.png?w=160&format=webp';

  let { size = 'default' }: { size?: 'default' | 'large' } = $props();
</script>

<div class="logo-brand" class:large={size === 'large'}>
  <div class="logo-icon">
    <img src={logoSrc} alt="Wishlist Pulse" />
  </div>
  <div class="logo-text">
    <span class="word-top">Wishlist</span>
    <span class="pulse-line-wrap">
      <svg class="pulse-line" viewBox="0 0 200 16" preserveAspectRatio="none">
        <defs>
          <linearGradient id="brandPulseGrad" x1="0%" y1="0%" x2="100%" y2="0%">
            <stop offset="0%" stop-color="#22c55e" stop-opacity="0.15" />
            <stop offset="30%" stop-color="#4ade80" stop-opacity="1" />
            <stop offset="70%" stop-color="#22c55e" stop-opacity="1" />
            <stop offset="100%" stop-color="#22c55e" stop-opacity="0.15" />
          </linearGradient>
          <clipPath id="brandPulseClip">
            <rect x="0" y="0" width="200" height="16" />
          </clipPath>
        </defs>
        <g clip-path="url(#brandPulseClip)">
          <path
            class="pulse-path"
            d="M-100,8 L-85,8 L-80,2 L-72,14 L-64,2 L-56,14 L-48,2 L-40,8 L0,8 L15,8 L20,2 L28,14 L36,2 L44,14 L52,2 L60,8 L100,8 L115,8 L120,2 L128,14 L136,2 L144,14 L152,2 L160,8 L200,8 L215,8 L220,2 L228,14 L236,2 L244,14 L252,2 L260,8 L300,8"
            fill="none"
            stroke="url(#brandPulseGrad)"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </g>
      </svg>
      <svg class="pulse-line pulse-glow" viewBox="0 0 200 16" preserveAspectRatio="none">
        <defs>
          <clipPath id="brandPulseClipGlow">
            <rect x="0" y="0" width="200" height="16" />
          </clipPath>
        </defs>
        <g clip-path="url(#brandPulseClipGlow)">
          <path
            class="pulse-path"
            d="M-100,8 L-85,8 L-80,2 L-72,14 L-64,2 L-56,14 L-48,2 L-40,8 L0,8 L15,8 L20,2 L28,14 L36,2 L44,14 L52,2 L60,8 L100,8 L115,8 L120,2 L128,14 L136,2 L144,14 L152,2 L160,8 L200,8 L215,8 L220,2 L228,14 L236,2 L244,14 L252,2 L260,8 L300,8"
            fill="none"
            stroke="#4ade80"
            stroke-width="3"
            stroke-linecap="round"
            stroke-linejoin="round"
            opacity="0.2"
          />
        </g>
      </svg>
    </span>
    <span class="word-bottom">Pulse</span>
  </div>
</div>

<style>
  .logo-brand {
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
    --icon-size: 3rem;
    --font-size: 1.1rem;

    --line-height: 0.5rem;
  }

  .logo-brand.large {
    --icon-size: 5rem;
    --font-size: 1.6rem;

    --line-height: 0.65rem;
  }

  .logo-icon {
    position: relative;
    flex-shrink: 0;
  }

  .logo-icon img {
    width: var(--icon-size);
    height: var(--icon-size);
    border-radius: 50%;
    border: 2px solid var(--border);
    display: block;
  }

  .logo-text {
    display: flex;
    flex-direction: column;
    gap: 0;
    line-height: 1.15;
  }

  .word-top,
  .word-bottom {
    font-weight: 700;
    font-size: var(--font-size);
    background: linear-gradient(135deg, var(--accent, #d97706), var(--accent-glow, #f59e0b));
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
    background-clip: text;
  }

  .word-top {
    letter-spacing: 0.04em;
  }

  .word-bottom {
    letter-spacing: 0.08em;
    text-transform: uppercase;
    font-size: calc(var(--font-size) * 0.85);
  }

  .pulse-line-wrap {
    position: relative;
    width: 100%;
    height: var(--line-height);
    display: block;
    overflow: hidden;
    margin: 0.1rem 0;
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
