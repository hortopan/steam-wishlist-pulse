export const POLL_INTERVAL = 30_000;
export const TICK_INTERVAL = 60_000;
export const FLASH_DURATION = 1_200;
export const FLASH_ROW_DURATION = 1_500;
export const TOAST_DURATION = 3_000;
export const TOAST_DISMISS_DURATION = 400;

export const METRIC_CONFIG: Record<
  string,
  { label: string; color: string; prefix: string }
> = {
  adds: { label: 'Wishlist Adds', color: 'var(--green)', prefix: '' },
  deletes: { label: 'Wishlist Deletes', color: 'var(--red)', prefix: '' },
  purchases: { label: 'Purchases', color: 'var(--blue)', prefix: '' },
  gifts: { label: 'Gifts', color: 'var(--amber)', prefix: '' },
};

export const METRIC_KEYS = Object.keys(METRIC_CONFIG);
