import { mockIPC } from '@tauri-apps/api/mocks';
import type {
  CameraCapabilities,
  CameraPropertyId,
  DiscoveredCamera,
  FramePayload,
  LutSummary,
  PipelineTelemetry,
  SystemConfig,
} from '../types/ipc';

const MOCK_CAMERAS: DiscoveredCamera[] = [
  {
    id: 'canon:04a9:32f7:5abc05457047d3dab092e89055fe03c6',
    brand: 'canon',
    vendor_id: 0x04a9,
    product_id: 0x32f7,
    model_name: 'Canon EOS R7',
    serial_number: '5abc05457047d3dab092e89055fe03c6',
    usb_speed_mbps: 480,
  },
];

const MOCK_HDMI_PREVIEW_CAMERA: DiscoveredCamera = {
  id: 'hdmi:camlink-4k',
  brand: 'hdmi',
  vendor_id: 0x0fd9,
  product_id: 0x0066,
  model_name: 'Cam Link 4K (HDMI 2160p)',
  serial_number: 'camlink-4k',
  usb_speed_mbps: 5000,
};

function createInitialCapabilities(): CameraCapabilities {
  return {
    brand: 'canon',
    camera_model: 'Canon EOS R7',
    lens_model: 'RF-S 18-150mm F3.5-6.3 IS STM',
    firmware_version: '1.5.0',
    serial_number: '5abc05457047d3dab092e89055fe03c6',
    battery_percent: 88,
    exposure_mode: 'M (Manual)',
    manual_mode_active: true,
    supports_autofocus: true,
    supports_touch_autofocus: true,
    supports_manual_focus_drive: true,
    properties: {
      shutter_speed: {
        id: 'shutter_speed',
        raw_code: 0xd102,
        current: { raw: 0x70, label: '1/125' },
        writable: true,
        allowed_values: [
          { raw: 0x60, label: '1/30' },
          { raw: 0x68, label: '1/60' },
          { raw: 0x70, label: '1/125' },
          { raw: 0x78, label: '1/250' },
          { raw: 0x80, label: '1/500' },
        ],
      },
      aperture: {
        id: 'aperture',
        raw_code: 0xd101,
        current: { raw: 0x25, label: 'f/3.5' },
        writable: true,
        allowed_values: [
          { raw: 0x25, label: 'f/3.5' },
          { raw: 0x28, label: 'f/4.0' },
          { raw: 0x30, label: 'f/5.6' },
          { raw: 0x38, label: 'f/8.0' },
        ],
      },
      iso: {
        id: 'iso',
        raw_code: 0xd103,
        current: { raw: 0x60, label: 'ISO 800' },
        writable: true,
        allowed_values: [
          { raw: 0x00, label: 'Auto' },
          { raw: 0x48, label: 'ISO 100' },
          { raw: 0x58, label: 'ISO 400' },
          { raw: 0x60, label: 'ISO 800' },
          { raw: 0x68, label: 'ISO 1600' },
          { raw: 0x70, label: 'ISO 3200' },
        ],
      },
      exposure_compensation: {
        id: 'exposure_compensation',
        raw_code: 0xd104,
        current: { raw: 0x00, label: '0.0 EV' },
        writable: true,
        allowed_values: [
          { raw: 0xf8, label: '-1.0 EV' },
          { raw: 0xfd, label: '-0.3 EV' },
          { raw: 0x00, label: '0.0 EV' },
          { raw: 0x03, label: '+0.3 EV' },
          { raw: 0x08, label: '+1.0 EV' },
        ],
      },
      white_balance_mode: {
        id: 'white_balance_mode',
        raw_code: 0xd109,
        current: { raw: 9, label: 'Kelvin' },
        writable: true,
        allowed_values: [
          { raw: 0, label: 'Auto (Ambience)' },
          { raw: 1, label: 'Daylight' },
          { raw: 3, label: 'Cloudy' },
          { raw: 9, label: 'Kelvin' },
        ],
      },
      color_temperature_kelvin: {
        id: 'color_temperature_kelvin',
        raw_code: 0xd10a,
        current: { raw: 5600, label: '5600K' },
        writable: true,
        allowed_values: [
          { raw: 3200, label: '3200K' },
          { raw: 4500, label: '4500K' },
          { raw: 5600, label: '5600K' },
          { raw: 6500, label: '6500K' },
        ],
      },
      focus_mode: {
        id: 'focus_mode',
        raw_code: 0xd108,
        current: { raw: 0, label: 'One-Shot AF' },
        writable: true,
        allowed_values: [
          { raw: 0, label: 'One-Shot AF' },
          { raw: 1, label: 'AF-C (Servo AF)' },
        ],
      },
      focus_area: {
        id: 'focus_area',
        raw_code: 0xd1ba,
        current: { raw: 14, label: 'Whole Area AF' },
        writable: true,
        allowed_values: [
          { raw: 14, label: 'Whole Area AF' },
          { raw: 2, label: 'Face + Tracking' },
          { raw: 1, label: '1-Point AF' },
          { raw: 10, label: 'Spot AF' },
        ],
      },
    },
  };
}

function previewPhotoForLut(title: string): string {
  if (title.includes('Teal')) return '/preview/teal-orange.jpg';
  if (title.includes('Kodak')) return '/preview/kodak-2383.jpg';
  if (title.includes('Eterna')) return '/preview/fuji-eterna.jpg';
  if (title.includes('Monochrome') || title.includes('Acros')) {
    return '/preview/monochrome-acros.jpg';
  }
  return '/preview/ungraded.jpg';
}

let installed = false;

export function installBrowserPreviewMocksIfNeeded(): void {
  const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
  if (isTauri || installed) return;
  installed = true;

  let connected = false;
  let streaming = false;
  let splitPreview = false;
  let sequence = 0;
  let activeCameraId = MOCK_CAMERAS[0].id;
  let hdmiInputId: string | null = null;
  let caps = createInitialCapabilities();
  let sysConfig: SystemConfig = {
    resolution: '1080p',
    fpsCap: 60,
    autoSleepSensor: true,
  };
  let activeLut: LutSummary = {
    title: 'Neutral (Bypass)',
    size: 33,
    domain_min: [0, 0, 0],
    domain_max: [1, 1, 1],
    texel_count: 35937,
  };

  mockIPC((cmd, args) => {
    const p = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case 'list_cameras': {
        const includeHdmi =
          typeof window !== 'undefined' &&
          new URLSearchParams(window.location.search).get('hdmi') === '1';
        return includeHdmi ? [...MOCK_CAMERAS, MOCK_HDMI_PREVIEW_CAMERA] : MOCK_CAMERAS;
      }
      case 'connect_camera': {
        const requestedId = String(p.cameraId ?? MOCK_CAMERAS[0].id);
        const match = MOCK_CAMERAS.find((c) => c.id === requestedId) ?? MOCK_CAMERAS[0];
        activeCameraId = match.id;
        connected = true;
        caps = createInitialCapabilities();
        return caps;
      }
      case 'disconnect_camera':
        connected = false;
        streaming = false;
        hdmiInputId = null;
        sysConfig = {
          resolution: '1080p',
          fpsCap: 60,
          autoSleepSensor: true,
        };
        return null;
      case 'start_stream':
        streaming = true;
        return null;
      case 'stop_stream':
        streaming = false;
        return null;
      case 'set_split_preview':
        splitPreview = Boolean(p.enabled);
        return null;
      case 'get_latest_frame': {
        if (!connected || !streaming) return null;
        sequence += 1;
        const srcW = hdmiInputId ? (sysConfig.resolution === '4k' ? 3840 : 1920) : 960;
        const srcH = hdmiInputId ? (sysConfig.resolution === '4k' ? 2160 : 1080) : 640;
        const targetWidth =
          sysConfig.resolution === 'native' ? srcW : sysConfig.resolution === '4k' ? 3840 : 1920;
        const targetHeight =
          sysConfig.resolution === 'native' ? srcH : sysConfig.resolution === '4k' ? 2160 : 1080;
        const payload: FramePayload = {
          gradedDataUrl: previewPhotoForLut(activeLut.title),
          ungradedDataUrl: splitPreview ? '/preview/ungraded.jpg' : null,
          width: targetWidth,
          height: targetHeight,
          sourceWidth: srcW,
          sourceHeight: srcH,
          sequence,
          latencyMs: 28.4,
          fps: sysConfig.fpsCap === 30 ? 30.0 : 59.94,
          capabilities: caps,
        };
        return payload;
      }
      case 'set_camera_property': {
        const propId = p.property as CameraPropertyId;
        const raw = Number(p.rawValue);
        const desc = caps.properties[propId];
        if (desc) {
          const match = desc.allowed_values.find((v) => v.raw === raw);
          desc.current = match ?? { raw, label: `0x${raw.toString(16)}` };
        }
        return caps;
      }
      case 'trigger_autofocus': {
        const areaDesc = caps.properties.focus_area;
        if (areaDesc) {
          if (p.x !== null && p.x !== undefined && p.y !== null && p.y !== undefined) {
            areaDesc.current = { raw: 1, label: '1-Point AF' };
          } else {
            areaDesc.current = { raw: 14, label: 'Whole Area AF' };
          }
        }
        return caps;
      }
      case 'drive_focus':
        return null;
      case 'set_lut_config': {
        const preset = String(p.presetOrCube ?? 'neutral');
        const isCustom = Boolean(p.isCustomCube);
        const titleMap: Record<string, string> = {
          neutral: 'Neutral (Bypass)',
          rec709_crisp: 'Rec.709 Crisp',
          teal_orange: 'Cinematic Teal & Orange',
          kodak_2383_warm: 'Warm Kodak 2383',
          fuji_eterna: 'Fujifilm Eterna Cinema',
          classic_chrome: 'Classic Chrome Documentary',
          sony_s_cinetone: 'S-Cinetone Natural Skin',
          golden_hour: 'Golden Hour Amber',
          bleach_bypass: 'Silver Bleach Bypass',
          monochrome_acros: 'Monochrome Acros',
        };
        activeLut = {
          title: isCustom ? 'Custom .cube LUT' : titleMap[preset] ?? preset,
          size: 33,
          domain_min: [0, 0, 0],
          domain_max: [1, 1, 1],
          texel_count: 35937,
        };
        return activeLut;
      }
      case 'set_system_config':
        sysConfig = {
          resolution: String(p.resolution ?? '1080p'),
          fpsCap: Number(p.fpsCap ?? 60),
          autoSleepSensor: Boolean(p.autoSleepSensor ?? true),
        };
        return sysConfig;
      case 'activate_camera_extension':
        return 'macOS CMIO System Extension requires a signed Apple Developer ID + notarized build. Use the built-in Local MJPEG Stream (http://127.0.0.1:49210/stream.mjpeg) or Clean Output Mode in OBS right now.';
      case 'update_feed_status':
        return 'ok';
      case 'runtime_shutdown_for_update':
        return null;
      case 'plugin:app|version':
        return '1.0.0';
      case 'set_hdmi_input': {
        const val = p.deviceId;
        hdmiInputId = typeof val === 'string' && val.trim().length > 0 ? val : null;
        return null;
      }
      case 'get_telemetry': {
        const srcW = hdmiInputId ? (sysConfig.resolution === '4k' ? 3840 : 1920) : 960;
        const srcH = hdmiInputId ? (sysConfig.resolution === '4k' ? 2160 : 1080) : 640;
        const targetWidth =
          sysConfig.resolution === 'native' ? srcW : sysConfig.resolution === '4k' ? 3840 : 1920;
        const targetHeight =
          sysConfig.resolution === 'native' ? srcH : sysConfig.resolution === '4k' ? 2160 : 1080;
        const tel: PipelineTelemetry = {
          connected,
          streaming,
          cameraId: connected ? activeCameraId : null,
          hdmiInputId,
          sourceFormat: hdmiInputId ? 'Uncompressed HDMI' : 'USB PTP MJPEG',
          width: targetWidth,
          height: targetHeight,
          sourceWidth: srcW,
          sourceHeight: srcH,
          fps: streaming ? (sysConfig.fpsCap === 30 ? 30.0 : 59.94) : 0,
          latencyMs: streaming ? 28.4 : 0,
          usbMs: streaming ? 11.2 : 0,
          gpuMs: streaming ? 3.1 : 0,
          ringPushed: sequence,
          ringPopped: sequence,
          ringDropped: 0,
          sharedSurfaceId: streaming ? 248 : 0,
          streamUrl: 'http://127.0.0.1:49210/stream.mjpeg',
          virtualCam: {
            active: streaming,
            device_name: 'TruongStream Virtual Camera',
            backend: 'CoreMediaIO (CMIOExtension)',
            consumer_count: streaming ? 1 : 0,
            output_width: targetWidth,
            output_height: targetHeight,
            fps_cap: sysConfig.fpsCap,
          },
          systemConfig: sysConfig,
          activeLut,
        };
        return tel;
      }
      default:
        return null;
    }
  });
}
