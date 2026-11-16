import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import type {
  CameraCapabilities,
  CameraPropertyId,
  DiscoveredCamera,
  FramePayload,
  LutSummary,
  PipelineTelemetry,
  SystemConfig,
} from '../types/ipc';

export type InspectorTab = 'camera' | 'color' | 'system';

export const BUILTIN_LUTS = [
  ['neutral', 'Neutral (Bypass)'],
  ['rec709_crisp', 'Rec.709 Crisp'],
  ['teal_orange', 'Cinematic Teal & Orange'],
  ['kodak_2383_warm', 'Warm Kodak 2383'],
  ['fuji_eterna', 'Fujifilm Eterna Cinema'],
  ['classic_chrome', 'Classic Chrome Documentary'],
  ['sony_s_cinetone', 'S-Cinetone Natural Skin'],
  ['golden_hour', 'Golden Hour Amber'],
  ['bleach_bypass', 'Silver Bleach Bypass'],
  ['monochrome_acros', 'Monochrome Acros'],
] as const;

export interface LutUiConfig {
  presetOrCube: string;
  isCustomCube: boolean;
  customLabel: string | null;
  intensity: number;
  mirror: boolean;
  flip: boolean;
  crop: number;
}

export interface StreamStore {
  cameras: DiscoveredCamera[];
  selectedCameraId: string;
  capabilities: CameraCapabilities | null;
  telemetry: PipelineTelemetry | null;
  activeTab: InspectorTab;
  gradedFrameUrl: string | null;
  ungradedFrameUrl: string | null;
  showThirdsGrid: boolean;
  splitEnabled: boolean;
  splitRatio: number;
  focusReticle: { x: number; y: number; key: number } | null;
  lutConfig: LutUiConfig;
  activeLutSummary: LutSummary | null;
  systemConfig: SystemConfig;
  shutterRuleAuto: boolean;
  cleanOutputMode: boolean;
  busy: boolean;
  error: string | null;

  setActiveTab: (tab: InspectorTab) => void;
  setShowThirdsGrid: (show: boolean) => void;
  setSplitEnabled: (enabled: boolean) => Promise<void>;
  setSplitRatio: (ratio: number) => void;
  setCleanOutputMode: (enabled: boolean) => void;
  clearError: () => void;

  refreshCameras: () => Promise<void>;
  connectCamera: (cameraId: string) => Promise<void>;
  disconnectCamera: () => Promise<void>;
  toggleStream: () => Promise<void>;
  pollFrame: () => Promise<void>;
  pollTelemetry: () => Promise<void>;
  setCameraProperty: (property: CameraPropertyId, rawValue: number) => Promise<void>;
  setShutterRuleAuto: (enabled: boolean) => Promise<void>;
  triggerAutofocus: (
    x?: number,
    y?: number,
    reticleX?: number,
    reticleY?: number,
  ) => Promise<void>;
  resetToWholeAreaAf: () => Promise<void>;
  driveFocus: (step: number) => Promise<void>;
  updateLutConfig: (patch: Partial<LutUiConfig>) => Promise<void>;
  loadCustomCube: (cubeText: string, fileName: string) => Promise<void>;
  updateSystemConfig: (patch: Partial<SystemConfig>) => Promise<void>;
}

const DEFAULT_LUT_CONFIG: LutUiConfig = {
  presetOrCube: 'neutral',
  isCustomCube: false,
  customLabel: null,
  intensity: 1.0,
  mirror: false,
  flip: false,
  crop: 1.0,
};

function findShutterForFps(caps: CameraCapabilities | null, fpsCap: number): number | null {
  const opts = caps?.properties.shutter_speed?.allowed_values ?? [];
  const preferred = fpsCap <= 30 ? ['1/60', '1/50'] : ['1/125', '1/120', '1/100'];
  for (const label of preferred) {
    const found = opts.find((v) => v.label === label);
    if (found) return found.raw;
  }
  return null;
}

export const useStreamStore = create<StreamStore>((set, get) => ({
  cameras: [],
  selectedCameraId: '',
  capabilities: null,
  telemetry: null,
  activeTab: 'camera',
  gradedFrameUrl: null,
  ungradedFrameUrl: null,
  showThirdsGrid: false,
  splitEnabled: false,
  splitRatio: 0.5,
  focusReticle: null,
  lutConfig: DEFAULT_LUT_CONFIG,
  activeLutSummary: null,
  systemConfig: {
    resolution: '1080p',
    fpsCap: 60,
    autoSleepSensor: true,
  },
  shutterRuleAuto: false,
  cleanOutputMode: false,
  busy: false,
  error: null,

  setActiveTab: (activeTab) => set({ activeTab }),
  setShowThirdsGrid: (showThirdsGrid) => set({ showThirdsGrid }),
  setSplitRatio: (ratio) => {
    if (!Number.isFinite(ratio)) return;
    set({ splitRatio: Math.max(0.05, Math.min(0.95, ratio)) });
  },
  setCleanOutputMode: (cleanOutputMode) => set({ cleanOutputMode }),
  clearError: () => set({ error: null }),

  setSplitEnabled: async (enabled) => {
    set({ splitEnabled: enabled });
    await invoke('set_split_preview', { enabled }).catch(() => {});
  },

  refreshCameras: async () => {
    try {
      const cameras = await invoke<DiscoveredCamera[]>('list_cameras');
      const prev = get().selectedCameraId;
      const nextId = cameras.some((c) => c.id === prev) ? prev : (cameras[0]?.id ?? '');
      set({ cameras, selectedCameraId: nextId, error: null });
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },

  connectCamera: async (cameraId) => {
    if (!cameraId) return;
    set({ busy: true, error: null, selectedCameraId: cameraId });
    try {
      const capabilities = await invoke<CameraCapabilities>('connect_camera', { cameraId });
      await invoke('start_stream');
      set({ capabilities, busy: false });
      await get().pollTelemetry();
    } catch (err) {
      set({
        busy: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  disconnectCamera: async () => {
    set({ busy: true, error: null });
    try {
      await invoke('disconnect_camera');
      set({
        capabilities: null,
        gradedFrameUrl: null,
        ungradedFrameUrl: null,
        focusReticle: null,
        shutterRuleAuto: false,
        busy: false,
      });
      await get().pollTelemetry();
    } catch (err) {
      set({ busy: false, error: err instanceof Error ? err.message : String(err) });
    }
  },

  toggleStream: async () => {
    const streaming = get().telemetry?.streaming ?? false;
    try {
      await invoke(streaming ? 'stop_stream' : 'start_stream');
      await get().pollTelemetry();
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },

  pollFrame: async () => {
    try {
      const frame = await invoke<FramePayload | null>('get_latest_frame');
      if (frame) {
        const prevTelemetry = get().telemetry;
        set({
          gradedFrameUrl: frame.gradedDataUrl,
          ungradedFrameUrl: frame.ungradedDataUrl,
          capabilities: frame.capabilities,
          ...(prevTelemetry
            ? {
                telemetry: {
                  ...prevTelemetry,
                  width: frame.width,
                  height: frame.height,
                  sourceWidth: frame.sourceWidth,
                  sourceHeight: frame.sourceHeight,
                  fps: frame.fps,
                  latencyMs: frame.latencyMs,
                },
              }
            : {}),
        });
      }
    } catch {
      // Frame polling errors are surfaced via telemetry/connection status.
    }
  },

  pollTelemetry: async () => {
    try {
      const telemetry = await invoke<PipelineTelemetry>('get_telemetry');
      set({
        telemetry,
        systemConfig: telemetry.systemConfig,
        activeLutSummary: telemetry.activeLut,
      });
    } catch {
      // Ignore transient IPC shutdown errors.
    }
  },

  setCameraProperty: async (property, rawValue) => {
    if (property === 'shutter_speed' && rawValue === -1) {
      await get().setShutterRuleAuto(true);
      return;
    }
    try {
      const capabilities = await invoke<CameraCapabilities>('set_camera_property', {
        property,
        rawValue,
      });
      set({
        capabilities,
        error: null,
        ...(property === 'shutter_speed' ? { shutterRuleAuto: false } : {}),
        ...(property === 'focus_area' && rawValue === 14 ? { focusReticle: null } : {}),
      });
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },

  setShutterRuleAuto: async (enabled) => {
    set({ shutterRuleAuto: enabled });
    if (!enabled) return;
    const targetRaw = findShutterForFps(get().capabilities, get().systemConfig.fpsCap);
    if (targetRaw === null) return;
    try {
      const capabilities = await invoke<CameraCapabilities>('set_camera_property', {
        property: 'shutter_speed',
        rawValue: targetRaw,
      });
      set({ capabilities, error: null });
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },

  triggerAutofocus: async (x, y, reticleX, reticleY) => {
    const rx = reticleX ?? x;
    const ry = reticleY ?? y;
    if (rx !== undefined && ry !== undefined) {
      set({ focusReticle: { x: rx, y: ry, key: Date.now() } });
    } else {
      set({ focusReticle: null });
    }
    try {
      const capabilities = await invoke<CameraCapabilities | null>('trigger_autofocus', {
        x: x ?? null,
        y: y ?? null,
      });
      if (capabilities) {
        set({ capabilities });
      }
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },

  resetToWholeAreaAf: async () => {
    await get().triggerAutofocus();
  },

  driveFocus: async (step) => {
    try {
      await invoke('drive_focus', { step });
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },

  updateLutConfig: async (patch) => {
    const next: LutUiConfig = { ...get().lutConfig, ...patch };
    set({ lutConfig: next, error: null });
    try {
      const activeLutSummary = await invoke<LutSummary>('set_lut_config', {
        presetOrCube: next.presetOrCube,
        isCustomCube: next.isCustomCube,
        intensity: next.intensity,
        mirror: next.mirror,
        flip: next.flip,
        crop: next.crop,
      });
      set({ activeLutSummary });
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },

  loadCustomCube: async (cubeText, fileName) => {
    await get().updateLutConfig({
      presetOrCube: cubeText,
      isCustomCube: true,
      customLabel: fileName,
    });
  },

  updateSystemConfig: async (patch) => {
    const next = { ...get().systemConfig, ...patch };
    set({ systemConfig: next });
    try {
      const saved = await invoke<SystemConfig>('set_system_config', {
        resolution: next.resolution,
        fpsCap: next.fpsCap,
        autoSleepSensor: next.autoSleepSensor,
      });
      set({ systemConfig: saved });
      if (patch.fpsCap !== undefined && get().shutterRuleAuto) {
        const targetRaw = findShutterForFps(get().capabilities, saved.fpsCap);
        if (targetRaw !== null) {
          const capabilities = await invoke<CameraCapabilities>('set_camera_property', {
            property: 'shutter_speed',
            rawValue: targetRaw,
          });
          set({ capabilities });
        }
      }
      await get().pollTelemetry();
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },
}));
