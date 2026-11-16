export type CameraBrand = 'canon' | 'sony' | 'nikon' | 'fujifilm';

export interface DiscoveredCamera {
  id: string;
  brand: CameraBrand;
  vendor_id: number;
  product_id: number;
  model_name: string;
  serial_number: string;
  usb_speed_mbps: number;
}

export type CameraPropertyId =
  | 'shutter_speed'
  | 'aperture'
  | 'iso'
  | 'exposure_compensation'
  | 'white_balance_mode'
  | 'color_temperature_kelvin'
  | 'focus_mode'
  | 'focus_area'
  | 'film_simulation';

export interface PropertyValue {
  raw: number;
  label: string;
}

export interface PropertyDescriptor {
  id: CameraPropertyId;
  raw_code: number;
  current: PropertyValue;
  writable: boolean;
  allowed_values: PropertyValue[];
}

export interface CameraCapabilities {
  brand: CameraBrand;
  camera_model: string;
  lens_model: string;
  firmware_version: string;
  serial_number: string;
  battery_percent: number | null;
  exposure_mode: string;
  manual_mode_active: boolean;
  properties: Partial<Record<CameraPropertyId, PropertyDescriptor>>;
  supports_autofocus: boolean;
  supports_touch_autofocus: boolean;
  supports_manual_focus_drive: boolean;
}

export interface LutSummary {
  title: string;
  size: number;
  domain_min: [number, number, number];
  domain_max: [number, number, number];
  texel_count: number;
}

export interface VirtualCamStatus {
  active: boolean;
  device_name: string;
  backend: string;
  consumer_count: number;
  output_width: number;
  output_height: number;
  fps_cap: number;
}

export interface SystemConfig {
  resolution: '1080p' | '4k' | 'native' | string;
  fpsCap: number;
  autoSleepSensor: boolean;
}

export interface FramePayload {
  gradedDataUrl: string;
  ungradedDataUrl: string | null;
  width: number;
  height: number;
  sourceWidth: number;
  sourceHeight: number;
  sequence: number;
  latencyMs: number;
  fps: number;
  capabilities: CameraCapabilities;
}

export interface PipelineTelemetry {
  connected: boolean;
  streaming: boolean;
  cameraId: string | null;
  width: number;
  height: number;
  sourceWidth: number;
  sourceHeight: number;
  fps: number;
  latencyMs: number;
  usbMs: number;
  gpuMs: number;
  ringPushed: number;
  ringPopped: number;
  ringDropped: number;
  sharedSurfaceId: number;
  streamUrl: string;
  virtualCam: VirtualCamStatus;
  systemConfig: SystemConfig;
  activeLut: LutSummary;
}
