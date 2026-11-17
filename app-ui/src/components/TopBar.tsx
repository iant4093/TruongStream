import React from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Camera, Plug, RefreshCw, Unplug, Video } from 'lucide-react';
import { useStreamStore } from '../state/useStreamStore';
import { Dropdown } from './Dropdown';
import { WindowControls, isWindowsPlatform } from './WindowControls';

export const TopBar: React.FC = () => {
  const cameras = useStreamStore((s) => s.cameras);
  const selectedCameraId = useStreamStore((s) => s.selectedCameraId);
  const telemetry = useStreamStore((s) => s.telemetry);
  const busy = useStreamStore((s) => s.busy);
  const refreshCameras = useStreamStore((s) => s.refreshCameras);
  const connectCamera = useStreamStore((s) => s.connectCamera);
  const disconnectCamera = useStreamStore((s) => s.disconnectCamera);
  const setActiveTab = useStreamStore((s) => s.setActiveTab);

  const connected = telemetry?.connected ?? false;
  const streaming = telemetry?.streaming ?? false;
  const vcam = telemetry?.virtualCam;

  const streamStatusLabel = !connected
    ? 'No active stream'
    : streaming
      ? `${telemetry?.width ?? 960}×${telemetry?.height ?? 640} · ${telemetry?.fps.toFixed(1) ?? '0.0'} fps · ${telemetry?.latencyMs.toFixed(1) ?? '0.0'} ms`
      : 'Connected · Ready';

  const vcamButtonLabel = !vcam?.active
    ? 'Virtual Cam: Standby'
    : vcam.consumer_count > 0
      ? `Virtual Cam: ${vcam.consumer_count} Active`
      : 'Virtual Cam: Ready';

  const cameraOptions = cameras.map(
    (c) => [c.id, `${c.model_name} (${c.usb_speed_mbps} Mbps)`] as const,
  );
  const isWindows = isWindowsPlatform();

  const handleTitlebarDoubleClick = (e: React.MouseEvent<HTMLElement>) => {
    const target = e.target as HTMLElement | null;
    if (
      target?.closest(
        'button, select, input, [role="listbox"], [role="combobox"], .window-controls, .window-controls-win',
      )
    ) {
      return;
    }
    void getCurrentWindow()
      .toggleMaximize()
      .catch(() => {});
  };

  return (
    <header
      className={`workspace-titlebar${isWindows ? ' is-windows' : ''}`}
      data-tauri-drag-region
      onDoubleClick={handleTitlebarDoubleClick}
    >
      <div className="topbar-left">
        {!isWindows && <WindowControls variant="macos" />}
        <div className="app-brand" data-tauri-drag-region>
          <Camera size={16} strokeWidth={1.7} />
          <span>TruongStream</span>
        </div>

        <Dropdown
          className="camera-select"
          label="Connected camera"
          value={selectedCameraId}
          options={cameraOptions}
          onChange={(id) => void connectCamera(id)}
          placeholder="No USB camera detected"
          disabled={busy}
        />

        <button
          type="button"
          className="icon-button"
          title="Refresh USB cameras"
          aria-label="Refresh USB cameras"
          onClick={() => void refreshCameras()}
          disabled={busy}
        >
          <RefreshCw size={14} />
        </button>

        {connected ? (
          <button
            type="button"
            className="button"
            onClick={() => void disconnectCamera()}
            disabled={busy}
          >
            <Unplug size={13} />
            <span>Disconnect</span>
          </button>
        ) : (
          <button
            type="button"
            className="button button-primary"
            onClick={() => void connectCamera(selectedCameraId)}
            disabled={busy || !selectedCameraId}
          >
            <Plug size={13} />
            <span>Connect</span>
          </button>
        )}
      </div>

      <div className="topbar-right">
        <span className="workspace-label" aria-label="Stream telemetry">
          {streamStatusLabel}
        </span>

        <button
          type="button"
          className={`button${vcam?.active ? ' is-active' : ''}`}
          onClick={() => setActiveTab('system')}
        >
          <span className={`status-dot ${streaming ? 'is-live' : connected ? 'is-ok' : ''}`} />
          <Video size={13} />
          <span>{vcamButtonLabel}</span>
        </button>

        {isWindows && <WindowControls variant="windows" />}
      </div>
    </header>
  );
};
