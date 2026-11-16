import React, { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { X } from 'lucide-react';
import { ContextMenu, type ContextMenuItem } from './components/ContextMenu';
import { LiveViewport } from './components/LiveViewport';
import { RightInspector } from './components/RightInspector';
import { TopBar } from './components/TopBar';
import { useStreamStore } from './state/useStreamStore';

export const App: React.FC = () => {
  const refreshCameras = useStreamStore((s) => s.refreshCameras);
  const pollFrame = useStreamStore((s) => s.pollFrame);
  const pollTelemetry = useStreamStore((s) => s.pollTelemetry);
  const telemetry = useStreamStore((s) => s.telemetry);
  const capabilities = useStreamStore((s) => s.capabilities);
  const streaming = telemetry?.streaming ?? false;
  const connected = telemetry?.connected ?? false;
  const fpsCap = useStreamStore((s) => s.systemConfig.fpsCap);
  const showThirdsGrid = useStreamStore((s) => s.showThirdsGrid);
  const splitEnabled = useStreamStore((s) => s.splitEnabled);
  const lutConfig = useStreamStore((s) => s.lutConfig);
  const focusReticle = useStreamStore((s) => s.focusReticle);
  const cleanOutputMode = useStreamStore((s) => s.cleanOutputMode);
  const error = useStreamStore((s) => s.error);

  const clearError = useStreamStore((s) => s.clearError);
  const toggleStream = useStreamStore((s) => s.toggleStream);
  const triggerAutofocus = useStreamStore((s) => s.triggerAutofocus);
  const resetToWholeAreaAf = useStreamStore((s) => s.resetToWholeAreaAf);
  const setShowThirdsGrid = useStreamStore((s) => s.setShowThirdsGrid);
  const setSplitEnabled = useStreamStore((s) => s.setSplitEnabled);
  const updateLutConfig = useStreamStore((s) => s.updateLutConfig);
  const setCleanOutputMode = useStreamStore((s) => s.setCleanOutputMode);

  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  useEffect(() => {
    void refreshCameras();
    void pollTelemetry();
  }, [refreshCameras, pollTelemetry]);

  useEffect(() => {
    if (!cleanOutputMode) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setCleanOutputMode(false);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [cleanOutputMode, setCleanOutputMode]);

  useEffect(() => {
    if (!streaming) return;
    let active = true;
    let inflight = false;

    const frameIntervalMs = Math.max(16, Math.round(1000 / Math.max(fpsCap, 15)));
    const frameTimer = window.setInterval(() => {
      if (!active || inflight) return;
      inflight = true;
      void pollFrame().finally(() => {
        inflight = false;
      });
    }, frameIntervalMs);

    const telemetryTimer = window.setInterval(() => {
      if (active) void pollTelemetry();
    }, 500);

    return () => {
      active = false;
      window.clearInterval(frameTimer);
      window.clearInterval(telemetryTimer);
    };
  }, [streaming, fpsCap, pollFrame, pollTelemetry]);

  const contextItems: ContextMenuItem[] = [
    ...(connected
      ? [
          {
            id: 'stream',
            label: streaming ? 'Stop Live View' : 'Start Live View',
            onSelect: () => void toggleStream(),
          },
          {
            id: 'af',
            label: 'Trigger Autofocus',
            onSelect: () => void triggerAutofocus(),
          },
          ...(focusReticle !== null || capabilities?.properties.focus_area?.current.raw === 1
            ? [
                {
                  id: 'whole-area-af',
                  label: 'Reset to Whole Area AF',
                  onSelect: () => void resetToWholeAreaAf(),
                },
              ]
            : []),
        ]
      : []),
    {
      id: 'thirds',
      label: 'Rule-of-Thirds Grid',
      checked: showThirdsGrid,
      dividerBefore: connected,
      onSelect: () => setShowThirdsGrid(!showThirdsGrid),
    },
    {
      id: 'split',
      label: 'LUT Split Wipe',
      checked: splitEnabled,
      onSelect: () => void setSplitEnabled(!splitEnabled),
    },
    {
      id: 'mirror',
      label: 'Mirror Horizontal',
      checked: lutConfig.mirror,
      onSelect: () => void updateLutConfig({ mirror: !lutConfig.mirror }),
    },
    {
      id: 'reset-lut',
      label: 'Reset LUT to Neutral',
      onSelect: () =>
        void updateLutConfig({
          presetOrCube: 'neutral',
          isCustomCube: false,
          customLabel: null,
          intensity: 1.0,
        }),
    },
    {
      id: 'clean-output',
      label: cleanOutputMode ? 'Exit Clean Output Mode' : 'Clean Output Mode',
      checked: cleanOutputMode,
      dividerBefore: true,
      onSelect: () => setCleanOutputMode(!cleanOutputMode),
    },
    {
      id: 'fullscreen',
      label: 'Toggle Fullscreen',
      onSelect: () => {
        void (async () => {
          try {
            const w = getCurrentWindow();
            const full = await w.isFullscreen();
            await w.setFullscreen(!full);
          } catch {
            // Browser preview fallback
          }
        })();
      },
    },
    {
      id: 'refresh-cameras',
      label: 'Refresh USB Cameras',
      dividerBefore: true,
      onSelect: () => void refreshCameras(),
    },
  ];

  return (
    <div
      className={`app-window-frame ${cleanOutputMode ? 'is-clean-output' : ''}`}
      onContextMenu={(e) => {
        e.preventDefault();
        setContextMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      {!cleanOutputMode && <TopBar />}
      {!cleanOutputMode && error && (
        <div role="alert" className="error-banner">
          <span>{error}</span>
          <button
            type="button"
            className="icon-button"
            aria-label="Dismiss error"
            onClick={clearError}
          >
            <X size={14} />
          </button>
        </div>
      )}
      <main className="studio-body">
        <LiveViewport />
        {!cleanOutputMode && <RightInspector />}
      </main>
      {contextMenu && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          items={contextItems}
          onClose={() => setContextMenu(null)}
        />
      )}
    </div>
  );
};

export default App;
