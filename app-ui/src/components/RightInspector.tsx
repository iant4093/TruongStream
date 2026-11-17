import React, { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Check, Copy, Download, Maximize2, RefreshCw, ShieldCheck, Upload } from 'lucide-react';
import {
  appVersion,
  availableUpdate,
  checkForUpdate,
  installUpdate,
  onUpdateChange,
  type AvailableUpdate,
} from '../services/updates';
import { BUILTIN_LUTS, InspectorTab, useStreamStore } from '../state/useStreamStore';
import type { CameraPropertyId } from '../types/ipc';
import { Dropdown } from './Dropdown';
import { Badge, SectionTitle, SegmentedControl, ToggleRow } from './ui';

const PROPERTY_ORDER: ReadonlyArray<{ id: CameraPropertyId; label: string }> = [
  { id: 'shutter_speed', label: 'Shutter Speed' },
  { id: 'aperture', label: 'Aperture' },
  { id: 'iso', label: 'ISO Sensitivity' },
  { id: 'exposure_compensation', label: 'Exposure Comp (EV)' },
  { id: 'white_balance_mode', label: 'White Balance' },
  { id: 'color_temperature_kelvin', label: 'Color Temp (K)' },
  { id: 'focus_mode', label: 'Focus Mode' },
  { id: 'focus_area', label: 'AF Area' },
  { id: 'film_simulation', label: 'Film Simulation' },
];

const MANUAL_EXPOSURE_PROPS: ReadonlySet<CameraPropertyId> = new Set([
  'shutter_speed',
  'aperture',
  'iso',
  'exposure_compensation',
]);

export const RightInspector: React.FC = () => {
  const activeTab = useStreamStore((s) => s.activeTab);
  const setActiveTab = useStreamStore((s) => s.setActiveTab);
  const cameras = useStreamStore((s) => s.cameras);
  const capabilities = useStreamStore((s) => s.capabilities);
  const telemetry = useStreamStore((s) => s.telemetry);
  const lutConfig = useStreamStore((s) => s.lutConfig);
  const activeLutSummary = useStreamStore((s) => s.activeLutSummary);
  const systemConfig = useStreamStore((s) => s.systemConfig);
  const shutterRuleAuto = useStreamStore((s) => s.shutterRuleAuto);

  const setCameraProperty = useStreamStore((s) => s.setCameraProperty);
  const driveFocus = useStreamStore((s) => s.driveFocus);
  const updateLutConfig = useStreamStore((s) => s.updateLutConfig);
  const loadCustomCube = useStreamStore((s) => s.loadCustomCube);
  const updateSystemConfig = useStreamStore((s) => s.updateSystemConfig);
  const setHdmiInput = useStreamStore((s) => s.setHdmiInput);
  const setCleanOutputMode = useStreamStore((s) => s.setCleanOutputMode);

  const fileInputRef = useRef<HTMLInputElement>(null);
  const [copiedUrl, setCopiedUrl] = useState(false);
  const [extMsg, setExtMsg] = useState<string | null>(null);
  const [version, setVersion] = useState<string | null>(null);
  const [update, setUpdate] = useState<AvailableUpdate | null>(availableUpdate());
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [progress, setProgress] = useState<{ downloaded: number; total: number | null } | null>(null);

  useEffect(() => {
    let alive = true;
    appVersion()
      .then((v) => {
        if (alive) setVersion(v);
      })
      .catch(() => {
        if (alive) setVersion(null);
      });
    const off = onUpdateChange(setUpdate);
    return () => {
      alive = false;
      off();
    };
  }, []);

  const streamUrl = telemetry?.streamUrl ?? 'http://127.0.0.1:49210/stream.mjpeg';
  const hdmiCameras = cameras.filter((c) => c.brand === 'hdmi');
  const isDirectHdmi = capabilities?.brand === 'hdmi';
  const isHdmiFeed = telemetry?.sourceFormat === 'Uncompressed HDMI' || isDirectHdmi;

  const copyStreamUrl = async () => {
    try {
      await navigator.clipboard.writeText(streamUrl);
      setCopiedUrl(true);
      window.setTimeout(() => setCopiedUrl(false), 1800);
    } catch {
      setCopiedUrl(false);
    }
  };

  const triggerExtActivation = async () => {
    try {
      const msg = await invoke<string>('activate_camera_extension');
      setExtMsg(msg);
    } catch (err) {
      setExtMsg(err instanceof Error ? err.message : String(err));
    }
  };

  const runUpdateCheck = async () => {
    setChecking(true);
    setUpdateMsg(null);
    try {
      const found = await checkForUpdate();
      setUpdateMsg(
        found ? `TruongStream ${found.version} is available` : 'You are on the latest version',
      );
    } catch (err) {
      setUpdateMsg(err instanceof Error ? err.message : String(err));
    } finally {
      setChecking(false);
    }
  };

  const runUpdateInstall = async () => {
    setUpdateMsg('Downloading update…');
    try {
      await installUpdate((downloaded, total) => setProgress({ downloaded, total }));
    } catch (err) {
      setUpdateMsg(err instanceof Error ? err.message : String(err));
      setProgress(null);
    }
  };

  const onCubeSelected = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const text = await file.text();
    await loadCustomCube(text, file.name);
    e.target.value = '';
  };

  return (
    <aside className="inspector-pane" aria-label="Camera and LUT inspector">
      <div className="inspector-tabs">
        <SegmentedControl<InspectorTab>
          value={activeTab}
          options={[
            ['camera', 'Camera'],
            ['color', 'Color & LUT'],
            ['system', 'System'],
          ]}
          onChange={setActiveTab}
        />
      </div>

      <div className="inspector-scroll">
        {activeTab === 'camera' && (
          <>
            <div className="inspector-section">
              <SectionTitle>Hardware Body & Lens</SectionTitle>
              <div className="panel">
                <div className="diag-row">
                  <span>Body</span>
                  <span className="tabular-mono">{capabilities?.camera_model ?? 'Not connected'}</span>
                </div>
                <div className="diag-row">
                  <span>Lens</span>
                  <span className="tabular-mono">{capabilities?.lens_model ?? '—'}</span>
                </div>
                <div className="diag-row">
                  <span>Mode</span>
                  <span className="tabular-mono">{capabilities?.exposure_mode ?? '—'}</span>
                </div>
                <div className="diag-row">
                  <span>Battery</span>
                  <span className="tabular-mono">
                    {capabilities?.battery_percent !== null && capabilities?.battery_percent !== undefined
                      ? `${capabilities.battery_percent}%`
                      : 'USB / AC'}
                  </span>
                </div>
              </div>
            </div>

            <div className="inspector-section">
              <SectionTitle>Exposure & White Balance</SectionTitle>
              {capabilities && !capabilities.manual_mode_active && (
                <div className="mode-warning-panel" role="status">
                  Set camera dial to M (Manual) for full exposure control (currently{' '}
                  {capabilities.exposure_mode}).
                </div>
              )}
              {!capabilities ? (
                <div className="toggle-description">
                  Connect a camera to discover its live shutter, aperture, ISO, and white balance descriptors.
                </div>
              ) : (
                PROPERTY_ORDER.map(({ id, label }) => {
                  const desc = capabilities.properties[id];
                  if (!desc) return null;
                  const baseOpts =
                    desc.allowed_values.length > 0
                      ? desc.allowed_values.map((v) => [v.raw, v.label] as const)
                      : ([[desc.current.raw, desc.current.label]] as const);
                  const autoRuleLabel = `Auto (180° Rule · ${systemConfig.fpsCap <= 30 ? '1/60' : '1/125'})`;
                  const opts =
                    id === 'shutter_speed'
                      ? ([[-1, autoRuleLabel], ...baseOpts] as const)
                      : baseOpts;
                  const blockedByMode =
                    !capabilities.manual_mode_active && MANUAL_EXPOSURE_PROPS.has(id);
                  const valueDisplay =
                    id === 'shutter_speed' && shutterRuleAuto
                      ? `Auto 180° · ${desc.current.label}`
                      : id === 'exposure_compensation' &&
                          !desc.writable &&
                          capabilities.manual_mode_active
                        ? `${desc.current.label} · Auto ISO only`
                        : desc.current.label;

                  return (
                    <div key={id} className="property-field">
                      <div className="property-row">
                        <span>{label}</span>
                        <span className="property-value">{valueDisplay}</span>
                      </div>
                      <Dropdown<number>
                        label={label}
                        value={id === 'shutter_speed' && shutterRuleAuto ? -1 : desc.current.raw}
                        options={opts}
                        disabled={!desc.writable || blockedByMode}
                        onChange={(val) => void setCameraProperty(id, val)}
                      />
                    </div>
                  );
                })
              )}
            </div>

            <div className="inspector-section">
              <SectionTitle>Manual Focus Nudge</SectionTitle>
              <div className="focus-nudge-row">
                {([-3, -2, -1, 1, 2, 3] as const).map((step) => {
                  const label =
                    step < 0 ? '<'.repeat(Math.abs(step)) : '>'.repeat(step);
                  return (
                    <button
                      key={step}
                      type="button"
                      className="button"
                      disabled={!capabilities?.supports_manual_focus_drive}
                      title={step < 0 ? `Focus near (${step})` : `Focus far (+${step})`}
                      onClick={() => void driveFocus(step)}
                    >
                      {label}
                    </button>
                  );
                })}
              </div>
            </div>
          </>
        )}

        {activeTab === 'color' && (
          <>
            <div className="inspector-section">
              <SectionTitle>3D LUT Grading (.cube)</SectionTitle>
              <div className="property-field">
                <div className="property-row">
                  <span>Active LUT</span>
                  <span className="property-value">
                    {activeLutSummary ? `${activeLutSummary.size}³` : '33³'}
                  </span>
                </div>
                <Dropdown<string>
                  label="Built-in LUT preset"
                  value={lutConfig.isCustomCube ? 'custom' : lutConfig.presetOrCube}
                  options={[
                    ...BUILTIN_LUTS,
                    ...(lutConfig.isCustomCube
                      ? [['custom', lutConfig.customLabel ?? 'Custom .cube']] as const
                      : []),
                  ]}
                  onChange={(id) => {
                    if (id !== 'custom') {
                      void updateLutConfig({
                        presetOrCube: id,
                        isCustomCube: false,
                        customLabel: null,
                      });
                    }
                  }}
                />
              </div>

              <input
                ref={fileInputRef}
                type="file"
                accept=".cube"
                style={{ display: 'none' }}
                onChange={(e) => void onCubeSelected(e)}
              />
              <button
                type="button"
                className="button"
                onClick={() => fileInputRef.current?.click()}
              >
                <Upload size={13} />
                <span>Load Custom .cube…</span>
              </button>

              <div className="property-field">
                <div className="property-row">
                  <span>LUT Intensity</span>
                  <span className="property-value">{`${Math.round(lutConfig.intensity * 100)}%`}</span>
                </div>
                <input
                  type="range"
                  aria-label="LUT Intensity"
                  className="slider"
                  min={0}
                  max={100}
                  value={Math.round(lutConfig.intensity * 100)}
                  onChange={(e) =>
                    void updateLutConfig({ intensity: Number(e.target.value) / 100 })
                  }
                />
              </div>
            </div>

            <div className="inspector-section">
              <SectionTitle>Framing & Geometry</SectionTitle>
              <ToggleRow
                label="Mirror Horizontal"
                description="Flip preview and virtual webcam horizontally"
                checked={lutConfig.mirror}
                onChange={(mirror) => void updateLutConfig({ mirror })}
              />
              <ToggleRow
                label="Flip Vertical"
                description="Invert vertical orientation for rigged setups"
                checked={lutConfig.flip}
                onChange={(flip) => void updateLutConfig({ flip })}
              />

              <div className="property-field">
                <div className="property-row">
                  <span>Digital Crop</span>
                  <span className="property-value">{`${lutConfig.crop.toFixed(2)}x`}</span>
                </div>
                <input
                  type="range"
                  aria-label="Digital Crop"
                  className="slider"
                  min={100}
                  max={200}
                  step={5}
                  value={Math.round(lutConfig.crop * 100)}
                  onChange={(e) =>
                    void updateLutConfig({ crop: Number(e.target.value) / 100 })
                  }
                />
              </div>
            </div>
          </>
        )}

        {activeTab === 'system' && (
          <>
            <div className="inspector-section">
              <SectionTitle>Virtual Camera Output</SectionTitle>
              <div className="property-field">
                <div className="property-row">
                  <span>Output Resolution</span>
                </div>
                <SegmentedControl<string>
                  value={systemConfig.resolution}
                  options={[
                    ['native', 'Native'],
                    ['1080p', '1080p'],
                    ['4k', '4K UHD'],
                  ]}
                  onChange={(resolution) => void updateSystemConfig({ resolution })}
                />
                <div className="toggle-description">
                  {(() => {
                    const srcW = telemetry?.sourceWidth || 960;
                    const srcH = telemetry?.sourceHeight || 640;
                    if (systemConfig.resolution === 'native') {
                      return isHdmiFeed
                        ? `Direct 1:1 uncompressed HDMI stream (${srcW}×${srcH}) — zero scaling`
                        : `Direct 1:1 camera EVF stream (${srcW}×${srcH}) — zero scaling`;
                    }
                    if (systemConfig.resolution === '4k') {
                      return isHdmiFeed
                        ? `Uncompressed HDMI -> 4K UHD (${srcW}×${srcH} input)`
                        : `GPU-upscaled 4K 16:9 from ${srcW}×${srcH} USB EVF feed (native 4K requires HDMI)`;
                    }
                    return isHdmiFeed
                      ? `Uncompressed HDMI -> 1080p (${srcW}×${srcH} input)`
                      : `GPU-upscaled 16:9 from ${srcW}×${srcH} USB EVF feed`;
                  })()}
                </div>
              </div>

              <div className="property-field">
                <div className="property-row">
                  <span>Video Input Source (Uncompressed HDMI)</span>
                </div>
                {isDirectHdmi ? (
                  <div className="toggle-description">
                    {`Direct uncompressed HDMI capture active (${telemetry?.sourceWidth ?? 1920}×${telemetry?.sourceHeight ?? 1080})`}
                  </div>
                ) : (
                  <>
                    <Dropdown<string>
                      label="Hybrid HDMI Video Input"
                      value={telemetry?.hdmiInputId ?? ''}
                      options={[
                        ['', 'USB PTP Live View (Built-in)'],
                        ...hdmiCameras.map((c) => [c.id, c.model_name] as const),
                      ]}
                      disabled={!telemetry?.connected}
                      onChange={(val) => void setHdmiInput(val || null)}
                    />
                    <div className="toggle-description">
                      {hdmiCameras.length === 0
                        ? 'Plug an external 1080p/4K HDMI capture card to override USB EVF video while keeping USB dials & AF.'
                        : 'Pair an HDMI capture card for uncompressed 1080p/4K video while USB PTP controls dials & AF.'}
                    </div>
                  </>
                )}
              </div>

              <div className="property-field">
                <div className="property-row">
                  <span>FPS Cap</span>
                </div>
                <SegmentedControl<number>
                  value={systemConfig.fpsCap}
                  options={[
                    [30, '30 FPS'],
                    [60, '60 FPS'],
                  ]}
                  onChange={(fpsCap) => void updateSystemConfig({ fpsCap })}
                />
              </div>

              <ToggleRow
                label="Auto-Sleep Sensor Protection"
                description="Pause EVF streaming when virtual cam has 0 consumers"
                checked={systemConfig.autoSleepSensor}
                onChange={(autoSleepSensor) => void updateSystemConfig({ autoSleepSensor })}
              />

              <div className="property-field" style={{ marginTop: 10 }}>
                <button
                  type="button"
                  className="button"
                  onClick={() => setCleanOutputMode(true)}
                >
                  <Maximize2 size={13} />
                  <span>Enter Clean Output Mode (Esc to exit)</span>
                </button>
              </div>

              <div className="property-field" style={{ marginTop: 10 }}>
                <div className="property-row">
                  <span>Local MJPEG Stream (OBS / Browser Source)</span>
                </div>
                <div className="copy-url-row">
                  <input
                    type="text"
                    readOnly
                    aria-label="Local MJPEG stream URL"
                    className="input-readonly tabular-mono"
                    value={streamUrl}
                  />
                  <button
                    type="button"
                    className="button"
                    aria-label="Copy stream URL"
                    onClick={() => void copyStreamUrl()}
                  >
                    {copiedUrl ? <Check size={13} /> : <Copy size={13} />}
                    <span>{copiedUrl ? 'Copied' : 'Copy'}</span>
                  </button>
                </div>
              </div>

              <div className="property-field" style={{ marginTop: 8 }}>
                <button
                  type="button"
                  className="button"
                  onClick={() => void triggerExtActivation()}
                >
                  <ShieldCheck size={13} />
                  <span>Activate macOS Camera Extension</span>
                </button>
                {extMsg && (
                  <div className="toggle-description" role="status" style={{ marginTop: 6 }}>
                    {extMsg}
                  </div>
                )}
              </div>
            </div>

            <div className="inspector-section">
              <SectionTitle>Pipeline Diagnostics</SectionTitle>
              <div className="panel">
                <div className="diag-row">
                  <span>Camera Video Feed</span>
                  <span className="tabular-mono">
                    {`${telemetry?.sourceWidth || 960}×${telemetry?.sourceHeight || 640} ${isHdmiFeed ? 'HDMI' : 'MJPEG'}`}
                  </span>
                </div>
                <div className="diag-row">
                  <span>Virtual Sink</span>
                  <Badge tone={telemetry?.virtualCam.active ? 'ok' : 'muted'}>
                    {telemetry?.virtualCam.backend ?? 'CoreMediaIO'}
                  </Badge>
                </div>
                <div className="diag-row">
                  <span>Shared GPU Surface</span>
                  <span className="tabular-mono">
                    {telemetry?.sharedSurfaceId ? `IOSurface #${telemetry.sharedSurfaceId}` : '—'}
                  </span>
                </div>
                <div className="diag-row">
                  <span>USB Bulk Transfer</span>
                  <span className="tabular-mono">{`${(telemetry?.usbMs ?? 0).toFixed(1)} ms`}</span>
                </div>
                <div className="diag-row">
                  <span>GPU LUT + NV12</span>
                  <span className="tabular-mono">{`${(telemetry?.gpuMs ?? 0).toFixed(1)} ms`}</span>
                </div>
                <div className="diag-row">
                  <span>Mailbox Frames</span>
                  <span className="tabular-mono">
                    {`${telemetry?.ringPushed ?? 0} in / ${telemetry?.ringDropped ?? 0} dropped`}
                  </span>
                </div>
              </div>
            </div>

            <div className="inspector-section">
              <SectionTitle>Updates</SectionTitle>
              <div className="panel">
                <div className="diag-row" style={{ marginBottom: 8 }}>
                  <span>Installed Version</span>
                  <span className="tabular-mono">
                    {version ? `TruongStream v${version}` : 'TruongStream v1.0.0'}
                  </span>
                </div>
                <div className="update-actions">
                  <button
                    type="button"
                    className="button"
                    disabled={checking}
                    onClick={() => void runUpdateCheck()}
                  >
                    <RefreshCw size={13} />
                    <span>{checking ? 'Checking…' : 'Check for Updates'}</span>
                  </button>
                  {update && (
                    <button
                      type="button"
                      className="button button-primary"
                      disabled={Boolean(progress)}
                      onClick={() => void runUpdateInstall()}
                    >
                      <Download size={13} />
                      <span>{`Install ${update.version} & Restart`}</span>
                    </button>
                  )}
                </div>
                {updateMsg && (
                  <div className="toggle-description" role="status" style={{ marginTop: 8 }}>
                    {updateMsg}
                  </div>
                )}
                {update?.notes && (
                  <pre className="update-notes">{update.notes}</pre>
                )}
                {progress && (
                  <div className="toggle-description" style={{ marginTop: 6 }}>
                    {progress.total
                      ? `${Math.round((progress.downloaded / progress.total) * 100)}% downloaded`
                      : `${Math.round(progress.downloaded / 1024)} KB downloaded`}
                  </div>
                )}
              </div>
            </div>
          </>
        )}
      </div>
    </aside>
  );
};
