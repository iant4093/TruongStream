use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use rusb::{GlobalContext, UsbContext};
use tracing::debug;

use super::{
    unix_timestamp_us, CameraBrand, CameraCapabilities, CameraDriver, CameraPropertyId,
    DiscoveredCamera, LiveViewFrame, PropertyDescriptor,
};

const USB_CLASS_VIDEO: u8 = 0x0E;

/// Native HDMI / UVC capture device metadata enumerated from OS media APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdmiDeviceInfo {
    pub device_id: String,
    pub model_name: String,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub usb_speed_mbps: u32,
}

/// Active uncompressed HDMI / UVC capture stream returning raw BGRA8/NV12/YUY2 frames.
pub trait HdmiCaptureStream: Send {
    fn next_uncompressed_frame(&mut self) -> anyhow::Result<LiveViewFrame>;
    fn set_format(&mut self, width: u32, height: u32, fps: u32) -> anyhow::Result<()>;
    fn stop(&mut self);
}

/// Platform backend provider (AVFoundation on macOS, Media Foundation on Windows).
pub trait HdmiCaptureProvider: Send + Sync {
    fn enumerate_devices(&self) -> Vec<HdmiDeviceInfo>;
    fn open_stream(
        &self,
        device_id: &str,
        width: u32,
        height: u32,
        fps: u32,
    ) -> anyhow::Result<Box<dyn HdmiCaptureStream>>;
}

static HDMI_PROVIDER: OnceLock<RwLock<Option<Arc<dyn HdmiCaptureProvider>>>> = OnceLock::new();

fn provider_slot() -> &'static RwLock<Option<Arc<dyn HdmiCaptureProvider>>> {
    HDMI_PROVIDER.get_or_init(|| RwLock::new(None))
}

/// Registers the OS-native HDMI capture provider (`AVFoundation` or `Media Foundation`).
pub fn register_hdmi_provider(provider: Arc<dyn HdmiCaptureProvider>) {
    *provider_slot().write() = Some(provider);
}

pub fn strip_hdmi_id_prefix(id: &str) -> &str {
    id.strip_prefix("hdmi:").unwrap_or(id)
}

/// Opens a live uncompressed capture stream on the registered platform provider.
pub fn open_hdmi_stream(
    device_id: &str,
    width: u32,
    height: u32,
    fps: u32,
) -> anyhow::Result<Box<dyn HdmiCaptureStream>> {
    let clean_id = strip_hdmi_id_prefix(device_id);
    let provider = provider_slot()
        .read()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("HDMI capture provider is not initialized"))?;
    provider.open_stream(clean_id, width, height, fps)
}

/// Lists all available HDMI / external capture devices from the registered OS provider.
pub fn list_hdmi_device_infos() -> Vec<HdmiDeviceInfo> {
    provider_slot()
        .read()
        .as_ref()
        .map(|p| p.enumerate_devices())
        .unwrap_or_default()
}

fn known_capture_card_brand_pid(vendor_id: u16, product_id: u16) -> Option<&'static str> {
    match (vendor_id, product_id) {
        (0x0FD9, 0x0066) => Some("Elgato Cam Link 4K HDMI"),
        (0x0FD9, 0x007B) => Some("Elgato HD60 X HDMI"),
        (0x0FD9, 0x006C) => Some("Elgato HD60 S+ HDMI"),
        (0x0FD9, _) => Some("Elgato HDMI Capture"),
        (0x2935, _) => Some("Magewell USB Capture HDMI"),
        (0x1EDB, _) => Some("Blackmagic HDMI Capture"),
        (0x07CA, _) => Some("AVerMedia Live Gamer HDMI"),
        (0x534D, 0x2109) | (0x534D, 0x2121) | (0x534D, 0x2130) => Some("USB 3.0 HDMI Capture"),
        (0x32ED, _) => Some("Hagibis / UVC HDMI Capture"),
        _ => None,
    }
}

/// Enumerates connected uncompressed HDMI / UVC capture devices alongside fallback USB UVC capture dongles.
pub fn discover_hdmi_cameras() -> Vec<DiscoveredCamera> {
    let os_devices = list_hdmi_device_infos();
    let mut cameras: Vec<DiscoveredCamera> = os_devices
        .iter()
        .map(|dev| DiscoveredCamera {
            id: format!("hdmi:{}", dev.device_id),
            brand: CameraBrand::Hdmi,
            vendor_id: 0,
            product_id: 0,
            model_name: format!("{} (HDMI {}p)", dev.model_name, dev.max_height),
            serial_number: dev.device_id.clone(),
            usb_speed_mbps: dev.usb_speed_mbps.max(5000),
        })
        .collect();

    // Only synthesize raw USB UVC entries when the OS media provider did not already surface them
    // (or for known capture-card VIDs when running without an OS media session).
    if cameras.is_empty() {
        if let Ok(devices) = GlobalContext::default().devices() {
            for device in devices.iter() {
                let Ok(desc) = device.device_descriptor() else {
                    continue;
                };
                let Some(card_name) =
                    known_capture_card_brand_pid(desc.vendor_id(), desc.product_id())
                else {
                    continue;
                };
                let has_uvc = device
                    .active_config_descriptor()
                    .or_else(|_| device.config_descriptor(0))
                    .map(|cfg| {
                        cfg.interfaces().any(|iface| {
                            iface
                                .descriptors()
                                .any(|d| d.class_code() == USB_CLASS_VIDEO)
                        })
                    })
                    .unwrap_or(false);
                if !has_uvc {
                    continue;
                }
                let usb_speed_mbps = match device.speed() {
                    rusb::Speed::SuperPlus => 10_000,
                    rusb::Speed::Super => 5_000,
                    rusb::Speed::High => 480,
                    _ => 5_000,
                };
                let serial = device
                    .open()
                    .ok()
                    .and_then(|h| h.read_serial_number_string_ascii(&desc).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        format!("uvc-{:04x}-{:04x}", desc.vendor_id(), desc.product_id())
                    });
                cameras.push(DiscoveredCamera {
                    id: format!(
                        "hdmi:uvc:{:04x}:{:04x}:{}",
                        desc.vendor_id(),
                        desc.product_id(),
                        serial
                    ),
                    brand: CameraBrand::Hdmi,
                    vendor_id: desc.vendor_id(),
                    product_id: desc.product_id(),
                    model_name: card_name.to_string(),
                    serial_number: serial,
                    usb_speed_mbps,
                });
            }
        }
    }

    cameras
}

/// Standalone uncompressed 1080p / 4K HDMI capture card driver (`CameraBrand::Hdmi`).
pub struct HdmiDriver {
    device_id: String,
    capabilities: CameraCapabilities,
    stream: Option<Box<dyn HdmiCaptureStream>>,
    requested_width: u32,
    requested_height: u32,
    requested_fps: u32,
}

impl HdmiDriver {
    pub fn open(camera: &DiscoveredCamera) -> anyhow::Result<Self> {
        let device_id = strip_hdmi_id_prefix(&camera.id).to_string();
        let matched = list_hdmi_device_infos()
            .into_iter()
            .find(|d| d.device_id == device_id || camera.serial_number == d.device_id);

        let (req_w, req_h, req_fps) = match matched {
            Some(ref info) if info.max_width >= 3840 && info.max_height >= 2160 => {
                (3840, 2160, info.max_fps.clamp(30, 60))
            }
            Some(ref info) => (
                info.max_width.max(1920),
                info.max_height.max(1080),
                info.max_fps.clamp(30, 60),
            ),
            None => (1920, 1080, 60),
        };

        let capabilities = CameraCapabilities {
            brand: CameraBrand::Hdmi,
            camera_model: camera.model_name.clone(),
            lens_model: String::from("Uncompressed HDMI Input"),
            firmware_version: format!("{req_w}×{req_h} @ {req_fps} fps"),
            serial_number: camera.serial_number.clone(),
            battery_percent: None,
            exposure_mode: String::from("HDMI Direct (Manual on Camera)"),
            manual_mode_active: true,
            properties: HashMap::new(),
            supports_autofocus: false,
            supports_touch_autofocus: false,
            supports_manual_focus_drive: false,
        };

        Ok(Self {
            device_id,
            capabilities,
            stream: None,
            requested_width: req_w,
            requested_height: req_h,
            requested_fps: req_fps,
        })
    }

    pub fn set_capture_format(&mut self, width: u32, height: u32, fps: u32) -> anyhow::Result<()> {
        let w = if width >= 3840 { 3840 } else { 1920 };
        let h = if height >= 2160 { 2160 } else { 1080 };
        let f = fps.clamp(15, 60);
        self.requested_width = w;
        self.requested_height = h;
        self.requested_fps = f;
        self.capabilities.firmware_version = format!("{w}×{h} @ {f} fps");
        if let Some(ref mut stream) = self.stream {
            stream.set_format(w, h, f)?;
        }
        Ok(())
    }
}

impl CameraDriver for HdmiDriver {
    fn brand(&self) -> CameraBrand {
        CameraBrand::Hdmi
    }

    fn connect(&mut self) -> anyhow::Result<CameraCapabilities> {
        Ok(self.capabilities.clone())
    }

    fn start_live_view(&mut self) -> anyhow::Result<()> {
        if self.stream.is_none() {
            let stream = open_hdmi_stream(
                &self.device_id,
                self.requested_width,
                self.requested_height,
                self.requested_fps,
            )?;
            self.stream = Some(stream);
            debug!(
                device = %self.device_id,
                width = self.requested_width,
                height = self.requested_height,
                fps = self.requested_fps,
                "started uncompressed HDMI capture stream"
            );
        }
        Ok(())
    }

    fn stop_live_view(&mut self) -> anyhow::Result<()> {
        if let Some(mut stream) = self.stream.take() {
            stream.stop();
        }
        Ok(())
    }

    fn next_frame(&mut self) -> anyhow::Result<LiveViewFrame> {
        if self.stream.is_none() {
            self.start_live_view()?;
        }
        let started = Instant::now();
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("HDMI capture stream unavailable"))?;
        match stream.next_uncompressed_frame() {
            Ok(mut frame) => {
                if frame.usb_transfer_ms <= 0.0 {
                    frame.usb_transfer_ms = started.elapsed().as_secs_f32() * 1000.0;
                }
                if frame.timestamp_us == 0 {
                    frame.timestamp_us = unix_timestamp_us();
                }
                Ok(frame)
            }
            Err(err) => {
                std::thread::sleep(Duration::from_millis(2));
                Err(err)
            }
        }
    }

    fn set_property(&mut self, _prop: CameraPropertyId, _raw_value: u32) -> anyhow::Result<()> {
        anyhow::bail!(
            "HDMI capture feeds do not carry camera dial controls; connect your camera USB cable simultaneously for hardware control"
        )
    }

    fn poll_events(&mut self) -> anyhow::Result<Vec<(CameraPropertyId, PropertyDescriptor)>> {
        Ok(Vec::new())
    }

    fn trigger_autofocus(
        &mut self,
        _norm_x: Option<f32>,
        _norm_y: Option<f32>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("Touch AF requires a simultaneous USB PTP connection to the camera body")
    }

    fn drive_manual_focus(&mut self, _step: i32) -> anyhow::Result<()> {
        anyhow::bail!("Focus drive requires a simultaneous USB PTP connection to the camera body")
    }

    fn capabilities(&self) -> &CameraCapabilities {
        &self.capabilities
    }

    fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop_live_view()
    }
}

/// Converts raw YUY2 (`Y0 U0 Y1 V0` 4:2:2) into tightly packed `BGRA8`.
pub fn yuy2_to_bgra8(yuy2: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut out = vec![255u8; w * h * 4];
    let pairs = (w / 2) * h;
    for pair_idx in 0..pairs {
        let src = pair_idx * 4;
        if src + 3 >= yuy2.len() {
            break;
        }
        let y0 = yuy2[src] as i32 - 16;
        let u = yuy2[src + 1] as i32 - 128;
        let y1 = yuy2[src + 2] as i32 - 16;
        let v = yuy2[src + 3] as i32 - 128;

        for (px_offset, y) in [(0usize, y0), (1usize, y1)] {
            let c = y.max(0) * 298;
            let r = ((c + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            let g = ((c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            let b = ((c + 516 * u + 128) >> 8).clamp(0, 255) as u8;
            let dst = (pair_idx * 2 + px_offset) * 4;
            if dst + 3 < out.len() {
                out[dst] = b;
                out[dst + 1] = g;
                out[dst + 2] = r;
                out[dst + 3] = 255;
            }
        }
    }
    out
}

/// Converts semi-planar BT.601/709 `NV12` (Y plane followed by interleaved UV plane) into `BGRA8`.
pub fn nv12_to_bgra8(nv12: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let y_size = w * h;
    let mut out = vec![255u8; y_size * 4];
    for row in 0..h {
        for col in 0..w {
            let y_idx = row * w + col;
            if y_idx >= nv12.len() {
                continue;
            }
            let uv_idx = y_size + (row / 2) * w + (col & !1);
            if uv_idx + 1 >= nv12.len() {
                continue;
            }
            let y = (nv12[y_idx] as i32 - 16).max(0) * 298;
            let u = nv12[uv_idx] as i32 - 128;
            let v = nv12[uv_idx + 1] as i32 - 128;
            let r = ((y + 459 * v + 128) >> 8).clamp(0, 255) as u8;
            let g = ((y - 55 * u - 136 * v + 128) >> 8).clamp(0, 255) as u8;
            let b = ((y + 541 * u + 128) >> 8).clamp(0, 255) as u8;
            let dst = y_idx * 4;
            out[dst] = b;
            out[dst + 1] = g;
            out[dst + 2] = r;
            out[dst + 3] = 255;
        }
    }
    out
}

/// Helper utility to generate a synthetic test pattern `LiveViewFrame` (only used in tests).
#[cfg(test)]
pub fn synthetic_uncompressed_frame(
    width: u32,
    height: u32,
    seq: u64,
    format: super::FramePixelFormat,
) -> LiveViewFrame {
    use super::FramePixelFormat;
    let byte_len = match format {
        FramePixelFormat::Bgra8 => (width * height * 4) as usize,
        FramePixelFormat::Nv12 => (width * height * 3 / 2) as usize,
        FramePixelFormat::Yuy2 => (width * height * 2) as usize,
        FramePixelFormat::Jpeg => 0,
    };
    LiveViewFrame {
        jpeg_bytes: Vec::new(),
        raw_pixels: vec![128u8; byte_len],
        pixel_format: format,
        sequence: seq,
        timestamp_us: unix_timestamp_us(),
        width,
        height,
        usb_transfer_ms: 0.4,
    }
}

#[cfg(test)]
mod tests {
    use super::super::FramePixelFormat;
    use super::*;

    struct MockHdmiStream {
        width: u32,
        height: u32,
        seq: u64,
    }

    impl HdmiCaptureStream for MockHdmiStream {
        fn next_uncompressed_frame(&mut self) -> anyhow::Result<LiveViewFrame> {
            self.seq += 1;
            Ok(synthetic_uncompressed_frame(
                self.width,
                self.height,
                self.seq,
                FramePixelFormat::Bgra8,
            ))
        }

        fn set_format(&mut self, width: u32, height: u32, _fps: u32) -> anyhow::Result<()> {
            self.width = width;
            self.height = height;
            Ok(())
        }

        fn stop(&mut self) {}
    }

    #[test]
    fn converts_yuy2_and_nv12_to_bgra8() {
        // White YUY2 macropixel (Y=235, U=128, V=128)
        let bgra = yuy2_to_bgra8(&[235, 128, 235, 128], 2, 1);
        assert_eq!(bgra.len(), 8);
        assert!(bgra[0] >= 250 && bgra[1] >= 250 && bgra[2] >= 250);

        let mut nv12 = vec![235u8; 4];
        nv12.extend_from_slice(&[128, 128]);
        let bgra_nv12 = nv12_to_bgra8(&nv12, 2, 2);
        assert_eq!(bgra_nv12.len(), 16);
        assert!(bgra_nv12[0] >= 250 && bgra_nv12[1] >= 250 && bgra_nv12[2] >= 250);
    }

    #[test]
    fn mock_hdmi_stream_produces_uncompressed_1080p_and_4k_frames() {
        let mut stream = MockHdmiStream {
            width: 1920,
            height: 1080,
            seq: 0,
        };
        let f1080 = stream.next_uncompressed_frame().unwrap();
        assert_eq!(f1080.pixel_format, FramePixelFormat::Bgra8);
        assert_eq!((f1080.width, f1080.height), (1920, 1080));
        assert_eq!(f1080.raw_pixels.len(), 1920 * 1080 * 4);

        stream.set_format(3840, 2160, 60).unwrap();
        let f4k = stream.next_uncompressed_frame().unwrap();
        assert_eq!((f4k.width, f4k.height), (3840, 2160));
        assert_eq!(f4k.raw_pixels.len(), 3840 * 2160 * 4);
    }
}
