let audioCtx: AudioContext | null = null;
let unlocked = false;

const STORAGE_KEY = 'wishlist-pulse-sound-enabled';

let _soundEnabled: boolean = localStorage.getItem(STORAGE_KEY) !== 'false'; // default on

export function isSoundEnabled(): boolean {
  return _soundEnabled;
}

export function setSoundEnabled(enabled: boolean) {
  _soundEnabled = enabled;
  localStorage.setItem(STORAGE_KEY, String(enabled));
}

/**
 * Call this once to initialize the AudioContext on a user gesture (click/keydown).
 * Automatically removes itself after the first interaction.
 */
export function initAudio() {
  if (unlocked) return;

  const unlock = () => {
    if (unlocked) return;
    unlocked = true;
    audioCtx = new AudioContext();
    // Play a silent buffer to fully unlock on iOS/Safari
    const buf = audioCtx.createBuffer(1, 1, 22050);
    const src = audioCtx.createBufferSource();
    src.buffer = buf;
    src.connect(audioCtx.destination);
    src.start(0);
    document.removeEventListener('click', unlock);
    document.removeEventListener('keydown', unlock);
  };

  document.addEventListener('click', unlock, { once: false });
  document.addEventListener('keydown', unlock, { once: false });
}

function playTone(
  ctx: AudioContext,
  freq: number,
  start: number,
  duration: number,
  volume: number,
  type: OscillatorType = 'sine',
) {
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  osc.type = type;
  osc.frequency.value = freq;
  gain.gain.setValueAtTime(0, start);
  gain.gain.linearRampToValueAtTime(volume, start + 0.02); // soft attack
  gain.gain.setValueAtTime(volume, start + duration * 0.5);
  gain.gain.exponentialRampToValueAtTime(0.001, start + duration);
  osc.connect(gain).connect(ctx.destination);
  osc.start(start);
  osc.stop(start + duration);
}

/**
 * Play a pleasant ascending chime — like a Steam achievement or wishlist notification.
 * Three-note arpeggio with a warm shimmer tail (~0.8s total).
 */
export function playNotificationSound() {
  if (!audioCtx || !unlocked || !_soundEnabled) return;

  try {
    if (audioCtx.state === 'suspended') {
      audioCtx.resume();
    }

    const t = audioCtx.currentTime;
    const vol = 0.12;

    // Warm three-note arpeggio: C5 → E5 → G5
    playTone(audioCtx, 523.25, t,        0.25, vol, 'sine');      // C5
    playTone(audioCtx, 659.25, t + 0.12, 0.30, vol, 'sine');      // E5
    playTone(audioCtx, 783.99, t + 0.25, 0.55, vol * 1.1, 'sine'); // G5 — sustained

    // Subtle shimmer overtone on the final note
    playTone(audioCtx, 783.99 * 2, t + 0.25, 0.55, vol * 0.25, 'sine'); // G6 octave
    playTone(audioCtx, 783.99 * 3, t + 0.30, 0.40, vol * 0.08, 'sine'); // harmonic
  } catch {
    // Silently ignore audio errors
  }
}
