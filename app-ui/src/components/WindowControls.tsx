import React, { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';

export function isWindowsPlatform(): boolean {
  if (typeof navigator === 'undefined') return false;
  const platform = navigator.platform ?? '';
  const ua = navigator.userAgent ?? '';
  return /^win/i.test(platform) || /\bwindows\b/i.test(ua);
}

interface WindowControlsProps {
  variant?: 'macos' | 'windows';
}

export const WindowControls: React.FC<WindowControlsProps> = ({ variant }) => {
  const resolvedVariant = variant ?? (isWindowsPlatform() ? 'windows' : 'macos');
  const [isMaximized, setIsMaximized] = useState(false);
  const [isFullscreen, setIsFullscreen] = useState(false);

  useEffect(() => {
    if (resolvedVariant !== 'windows') return;
    let mounted = true;
    const syncState = async () => {
      try {
        const w = getCurrentWindow();
        const [max, full] = await Promise.all([
          w.isMaximized ? w.isMaximized() : Promise.resolve(false),
          w.isFullscreen ? w.isFullscreen() : Promise.resolve(false),
        ]);
        if (mounted) {
          setIsMaximized(Boolean(max));
          setIsFullscreen(Boolean(full));
        }
      } catch {
        // Browser preview fallback
      }
    };
    void syncState();
    window.addEventListener('resize', syncState);
    return () => {
      mounted = false;
      window.removeEventListener('resize', syncState);
    };
  }, [resolvedVariant]);

  const run = (fn: (w: ReturnType<typeof getCurrentWindow>) => Promise<void>) => () => {
    void fn(getCurrentWindow()).catch(() => {});
  };

  if (resolvedVariant === 'windows') {
    return (
      <div
        className="window-controls-win"
        aria-label="Window controls"
        onMouseDown={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
      >
        <button
          type="button"
          aria-label="Minimize window"
          title="Minimize"
          className="win-control-btn"
          onClick={run((w) => w.minimize())}
        >
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <line x1="1" y1="5" x2="9" y2="5" stroke="currentColor" strokeWidth="1.1" />
          </svg>
        </button>

        <button
          type="button"
          aria-label={isMaximized ? 'Restore window' : 'Maximize window'}
          title={isMaximized ? 'Restore Down' : 'Maximize'}
          className="win-control-btn"
          onClick={run(async (w) => {
            await w.toggleMaximize();
            const next = w.isMaximized ? await w.isMaximized() : !isMaximized;
            setIsMaximized(Boolean(next));
          })}
        >
          {isMaximized ? (
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
              <path
                d="M1.5 3.5h5v5h-5z"
                fill="none"
                stroke="currentColor"
                strokeWidth="1"
              />
              <path
                d="M3.5 3.5V1.5h5v5H6.5"
                fill="none"
                stroke="currentColor"
                strokeWidth="1"
              />
            </svg>
          ) : (
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
              <rect
                x="1.5"
                y="1.5"
                width="7"
                height="7"
                fill="none"
                stroke="currentColor"
                strokeWidth="1"
              />
            </svg>
          )}
        </button>

        <button
          type="button"
          aria-label="Toggle fullscreen"
          title={isFullscreen ? 'Exit Fullscreen (F11)' : 'Fullscreen (F11)'}
          className={`win-control-btn${isFullscreen ? ' is-active-fullscreen' : ''}`}
          onClick={run(async (w) => {
            const full = await w.isFullscreen();
            await w.setFullscreen(!full);
            setIsFullscreen(!full);
          })}
        >
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path
              d="M1.5 4V1.5H4M6 1.5H8.5V4M8.5 6V8.5H6M4 8.5H1.5V6"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.05"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        </button>

        <button
          type="button"
          aria-label="Close window"
          title="Close"
          className="win-control-btn is-win-close"
          onClick={run((w) => w.close())}
        >
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path
              d="M1.5 1.5L8.5 8.5M8.5 1.5L1.5 8.5"
              stroke="currentColor"
              strokeWidth="1.15"
              strokeLinecap="round"
            />
          </svg>
        </button>
      </div>
    );
  }

  return (
    <div
      className="window-controls"
      aria-label="Window controls"
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
    >
      <button
        type="button"
        aria-label="Close window"
        title="Close window"
        className="traffic-light is-close"
        onClick={run((w) => w.close())}
      >
        <svg width="8" height="8" viewBox="0 0 8 8" aria-hidden="true">
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
        title="Minimize window"
        className="traffic-light is-minimize"
        onClick={run((w) => w.minimize())}
      >
        <svg width="8" height="8" viewBox="0 0 8 8" aria-hidden="true">
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
        title="Toggle fullscreen"
        className="traffic-light is-fullscreen"
        onClick={run(async (w) => {
          const full = await w.isFullscreen();
          await w.setFullscreen(!full);
        })}
      >
        <svg width="8" height="8" viewBox="0 0 8 8" aria-hidden="true">
          <path
            d="M2 6L6 2M3.7 2H6V4.3M4.3 6H2V3.7"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.15"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </button>
    </div>
  );
};
