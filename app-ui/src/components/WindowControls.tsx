import React from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';

export const WindowControls: React.FC = () => {
  const run = (fn: (w: ReturnType<typeof getCurrentWindow>) => Promise<void>) => () => {
    void fn(getCurrentWindow()).catch(() => {});
  };

  return (
    <div className="window-controls" aria-label="Window controls">
      <button
        type="button"
        aria-label="Close window"
        className="traffic-light is-close"
        onClick={run((w) => w.close())}
      >
        <svg viewBox="0 0 8 8" width="8" height="8" aria-hidden="true">
          <path
            d="M1.75 1.75L6.25 6.25M6.25 1.75L1.75 6.25"
            stroke="currentColor"
            strokeWidth="1.25"
            strokeLinecap="round"
          />
        </svg>
      </button>

      <button
        type="button"
        aria-label="Minimize window"
        className="traffic-light is-minimize"
        onClick={run((w) => w.minimize())}
      >
        <svg viewBox="0 0 8 8" width="8" height="8" aria-hidden="true">
          <path
            d="M1.5 4H6.5"
            stroke="currentColor"
            strokeWidth="1.25"
            strokeLinecap="round"
          />
        </svg>
      </button>

      <button
        type="button"
        aria-label="Toggle fullscreen"
        className="traffic-light is-fullscreen"
        onClick={run(async (w) => {
          const full = await w.isFullscreen();
          await w.setFullscreen(!full);
        })}
      >
        <svg viewBox="0 0 8 8" width="8" height="8" aria-hidden="true">
          <path
            d="M1.6 1.6L3.9 1.6L1.6 3.9Z M6.4 6.4L4.1 6.4L6.4 4.1Z"
            fill="currentColor"
          />
        </svg>
      </button>
    </div>
  );
};
