pub mod canon;
pub mod fujifilm;
pub mod nikon;
#[cfg(test)]
pub mod simulator;
pub mod sony;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use rusb::{GlobalContext, UsbContext};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::ptp::USB_CLASS_PTP;
use crate::ring_buffer::MailboxRingBuffer;

/// Supported camera manufacturers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CameraBrand {
    Canon,
    Sony,
    Nikon,
    Fujifilm,
}

impl CameraBrand {
    pub fn from_vendor_id(vendor_id: u16) -> Option<Self> {
        match vendor_id {
            0x04A9 => Some(Self::Canon),
            0x054C => Some(Self::Sony),
            0x04B0 => Some(Self::Nikon),
            0x04CB => Some(Self::Fujifilm),
            _ => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Canon => "Canon",
            Self::Sony => "Sony",
            Self::Nikon => "Nikon",
            Self::Fujifilm => "Fujifilm",
        }
    }
}

/// Camera enumerated over USB prior to opening an exclusive PTP session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredCamera {
    pub id: String,
    pub brand: CameraBrand,
    pub vendor_id: u16,
    pub product_id: u16,
    pub model_name: String,
    pub serial_number: String,
    pub usb_speed_mbps: u32,
}

/// Standardized camera properties exposed to the UI and IPC layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraPropertyId {
    ShutterSpeed,
    Aperture,
    Iso,
    ExposureCompensation,
    WhiteBalanceMode,
    ColorTemperatureKelvin,
    FocusMode,
    FocusArea,
    FilmSimulation,
}

/// A single raw vendor property value paired with its human-readable label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyValue {
    pub raw: u32,
    pub label: String,
}

/// Dynamic property descriptor populated from the connected body & lens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyDescriptor {
    pub id: CameraPropertyId,
    pub raw_code: u32,
    pub current: PropertyValue,
    pub writable: bool,
    pub allowed_values: Vec<PropertyValue>,
}

/// Runtime capabilities discovered from the connected camera after `connect()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraCapabilities {
    pub brand: CameraBrand,
    pub camera_model: String,
    pub lens_model: String,
    pub firmware_version: String,
    pub serial_number: String,
    pub battery_percent: Option<u8>,
    pub exposure_mode: String,
    pub manual_mode_active: bool,
    pub properties: HashMap<CameraPropertyId, PropertyDescriptor>,
    pub supports_autofocus: bool,
    pub supports_touch_autofocus: bool,
    pub supports_manual_focus_drive: bool,
}

/// Compressed Live View frame captured from the camera's USB bulk stream.
#[derive(Debug, Clone)]
pub struct LiveViewFrame {
    pub jpeg_bytes: Vec<u8>,
    pub sequence: u64,
    pub timestamp_us: u64,
    pub width: u32,
    pub height: u32,
    pub usb_transfer_ms: f32,
}

/// Unified asynchronous/worker-friendly camera driver trait implemented by every vendor adapter.
pub trait CameraDriver: Send {
    fn brand(&self) -> CameraBrand;
    fn connect(&mut self) -> anyhow::Result<CameraCapabilities>;
    fn start_live_view(&mut self) -> anyhow::Result<()>;
    fn stop_live_view(&mut self) -> anyhow::Result<()>;
    fn next_frame(&mut self) -> anyhow::Result<LiveViewFrame>;
    fn set_property(&mut self, prop: CameraPropertyId, raw_value: u32) -> anyhow::Result<()>;
    fn poll_events(&mut self) -> anyhow::Result<Vec<(CameraPropertyId, PropertyDescriptor)>>;
    fn trigger_autofocus(&mut self, norm_x: Option<f32>, norm_y: Option<f32>)
        -> anyhow::Result<()>;
    fn drive_manual_focus(&mut self, step: i32) -> anyhow::Result<()>;
    fn capabilities(&self) -> &CameraCapabilities;
    fn disconnect(&mut self) -> anyhow::Result<()>;
}

/// Enumerates connected USB cameras from Canon, Sony, Nikon, and Fujifilm that expose a PTP interface.
pub fn discover_usb_cameras() -> anyhow::Result<Vec<DiscoveredCamera>> {
    let devices = GlobalContext::default().devices()?;
    let mut cameras = Vec::new();

    for device in devices.iter() {
        let Ok(desc) = device.device_descriptor() else {
            continue;
        };
        let Some(brand) = CameraBrand::from_vendor_id(desc.vendor_id()) else {
            continue;
        };

        let has_ptp = device
            .active_config_descriptor()
            .or_else(|_| device.config_descriptor(0))
            .map(|cfg| {
                cfg.interfaces()
                    .any(|iface| iface.descriptors().any(|d| d.class_code() == USB_CLASS_PTP))
            })
            .unwrap_or(false);
        if !has_ptp {
            continue;
        }

        let known = known_model_name(desc.vendor_id(), desc.product_id());
        let mut model_name = known
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} Camera", brand.display_name()));
        let mut serial_number = format!(
            "usb-{:04x}-{:04x}-b{}a{}",
            desc.vendor_id(),
            desc.product_id(),
            device.bus_number(),
            device.address()
        );

        if known.is_none() {
            if let Ok(handle) = device.open() {
                if let Ok(product) = handle.read_product_string_ascii(&desc) {
                    let trimmed = product.trim();
                    if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("Canon Digital Camera")
                    {
                        model_name = trimmed.to_string();
                    }
                }
                if let Ok(serial) = handle.read_serial_number_string_ascii(&desc) {
                    let trimmed = serial.trim();
                    if !trimmed.is_empty() {
                        serial_number = trimmed.to_string();
                    }
                }
            }
        } else if let Ok(handle) = device.open() {
            if let Ok(serial) = handle.read_serial_number_string_ascii(&desc) {
                let trimmed = serial.trim();
                if !trimmed.is_empty() {
                    serial_number = trimmed.to_string();
                }
            }
        }

        let usb_speed_mbps = match device.speed() {
            rusb::Speed::SuperPlus => 10_000,
            rusb::Speed::Super => 5_000,
            rusb::Speed::High => 480,
            rusb::Speed::Full => 12,
            rusb::Speed::Low => 1,
            _ => 480,
        };

        cameras.push(DiscoveredCamera {
            id: format!(
                "{}:{:04x}:{:04x}:{}",
                brand.display_name().to_lowercase(),
                desc.vendor_id(),
                desc.product_id(),
                serial_number
            ),
            brand,
            vendor_id: desc.vendor_id(),
            product_id: desc.product_id(),
            model_name,
            serial_number,
            usb_speed_mbps,
        });
    }

    Ok(cameras)
}

/// Instantiates the appropriate live USB [`CameraDriver`] for a [`DiscoveredCamera`].
pub fn open_usb_driver(camera: &DiscoveredCamera) -> anyhow::Result<Box<dyn CameraDriver>> {
    let serial = Some(camera.serial_number.as_str());
    match camera.brand {
        CameraBrand::Canon => Ok(Box::new(canon::CanonDriver::open_usb(
            Some(camera.product_id),
            serial,
        )?)),
        CameraBrand::Sony => Ok(Box::new(sony::SonyDriver::open_usb(
            Some(camera.product_id),
            serial,
        )?)),
        CameraBrand::Nikon => Ok(Box::new(nikon::NikonDriver::open_usb(
            Some(camera.product_id),
            serial,
        )?)),
        CameraBrand::Fujifilm => Ok(Box::new(fujifilm::FujifilmDriver::open_usb(
            Some(camera.product_id),
            serial,
        )?)),
    }
}

fn known_model_name(vendor_id: u16, product_id: u16) -> Option<&'static str> {
    match (vendor_id, product_id) {
        (0x04A9, 0x32F7) => Some("Canon EOS R7"),
        (0x04A9, 0x32DA) => Some("Canon EOS R5"),
        (0x04A9, 0x32EC) => Some("Canon EOS R6 Mark II"),
        (0x04A9, 0x32F8) => Some("Canon EOS R10"),
        (0x04A9, 0x3304) => Some("Canon EOS R8"),
        (0x054C, 0x0DC9) => Some("Sony Alpha 7 IV"),
        (0x054C, 0x0D6E) => Some("Sony Alpha 7S III"),
        (0x04B0, 0x0450) => Some("Nikon Z8"),
        (0x04B0, 0x044C) => Some("Nikon Z9"),
        (0x04CB, 0x02F3) => Some("Fujifilm X-T5"),
        (0x04CB, 0x02EF) => Some("Fujifilm X-H2S"),
        _ => None,
    }
}

/// Locates the first embedded JPEG image (`FF D8 ... FF D9`) inside a vendor Live View payload.
pub fn extract_jpeg_slice(payload: &[u8]) -> Option<&[u8]> {
    let start = payload.windows(2).position(|w| w == [0xFF, 0xD8])?;
    let tail = &payload[start..];
    let end_rel = tail.windows(2).rposition(|w| w == [0xFF, 0xD9])?;
    if end_rel + 2 <= tail.len() && end_rel >= 2 {
        Some(&tail[..end_rel + 2])
    } else {
        None
    }
}

/// Extracts `(width, height)` directly from JPEG SOF0/SOF2 markers without decoding pixels.
pub fn parse_jpeg_dimensions(jpeg: &[u8]) -> (u32, u32) {
    let mut i = 0usize;
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return (0, 0);
    }
    i += 2;
    while i + 3 < jpeg.len() {
        if jpeg[i] != 0xFF {
            i += 1;
            continue;
        }
        while i < jpeg.len() && jpeg[i] == 0xFF {
            i += 1;
        }
        if i >= jpeg.len() {
            break;
        }
        let marker = jpeg[i];
        i += 1;
        if marker == 0xD9 || marker == 0xDA || marker == 0x00 {
            break;
        }
        if i + 1 >= jpeg.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([jpeg[i], jpeg[i + 1]]) as usize;
        if seg_len < 2 || i + seg_len > jpeg.len() {
            break;
        }
        // SOF0 (0xC0), SOF1 (0xC1), SOF2 (0xC2)
        if matches!(marker, 0xC0..=0xC2) && seg_len >= 7 {
            let height = u16::from_be_bytes([jpeg[i + 3], jpeg[i + 4]]) as u32;
            let width = u16::from_be_bytes([jpeg[i + 5], jpeg[i + 6]]) as u32;
            return (width, height);
        }
        i += seg_len;
    }
    (0, 0)
}

pub(crate) fn unix_timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

enum WorkerCommand {
    SetLiveView(bool, mpsc::Sender<anyhow::Result<()>>),
    SetProperty(
        CameraPropertyId,
        u32,
        mpsc::Sender<anyhow::Result<CameraCapabilities>>,
    ),
    TriggerAutofocus(Option<f32>, Option<f32>, mpsc::Sender<anyhow::Result<()>>),
    DriveManualFocus(i32, mpsc::Sender<anyhow::Result<()>>),
    Shutdown(mpsc::Sender<()>),
}

/// Dedicated single-threaded half-duplex USB session worker for one connected camera.
///
/// Enforces PTP half-duplex serialization by executing `next_frame()`, `poll_events()`,
/// and UI-originated `set_property()` commands on the same thread, forwarding captured
/// frames into a lock-free [`MailboxRingBuffer`] and closing the session cleanly on drop.
pub struct CameraSessionWorker {
    tx: mpsc::Sender<WorkerCommand>,
    capabilities: Arc<RwLock<CameraCapabilities>>,
    ring_buffer: Arc<MailboxRingBuffer<LiveViewFrame>>,
    streaming: Arc<AtomicBool>,
    fps_cap: Arc<AtomicU32>,
    join_handle: Option<JoinHandle<()>>,
}

impl CameraSessionWorker {
    /// Connects to `driver`, launches the dedicated USB worker thread, and returns its handle.
    pub fn start(
        mut driver: Box<dyn CameraDriver>,
        ring_buffer: Arc<MailboxRingBuffer<LiveViewFrame>>,
    ) -> anyhow::Result<Self> {
        let initial_caps = driver.connect()?;
        let capabilities = Arc::new(RwLock::new(initial_caps));
        let caps_clone = Arc::clone(&capabilities);
        let ring_clone = Arc::clone(&ring_buffer);
        let streaming = Arc::new(AtomicBool::new(false));
        let streaming_clone = Arc::clone(&streaming);
        let fps_cap = Arc::new(AtomicU32::new(60));
        let fps_cap_clone = Arc::clone(&fps_cap);
        let (tx, rx) = mpsc::channel::<WorkerCommand>();

        let join_handle = thread::Builder::new()
            .name(format!(
                "truongstream-usb-{}",
                driver.brand().display_name().to_lowercase()
            ))
            .spawn(move || {
                Self::run_loop(
                    driver,
                    rx,
                    caps_clone,
                    ring_clone,
                    streaming_clone,
                    fps_cap_clone,
                );
            })?;

        Ok(Self {
            tx,
            capabilities,
            ring_buffer,
            streaming,
            fps_cap,
            join_handle: Some(join_handle),
        })
    }

    fn run_loop(
        mut driver: Box<dyn CameraDriver>,
        rx: mpsc::Receiver<WorkerCommand>,
        capabilities: Arc<RwLock<CameraCapabilities>>,
        ring_buffer: Arc<MailboxRingBuffer<LiveViewFrame>>,
        streaming: Arc<AtomicBool>,
        fps_cap: Arc<AtomicU32>,
    ) {
        let mut live_view_active = false;
        let mut last_event_poll = Instant::now();

        loop {
            // Drain all pending control commands before polling the next Live View frame.
            while let Ok(cmd) = rx.try_recv() {
                if Self::handle_command(
                    cmd,
                    &mut *driver,
                    &mut live_view_active,
                    &capabilities,
                    &streaming,
                ) {
                    return;
                }
            }

            if live_view_active {
                let frame_start = Instant::now();
                match driver.next_frame() {
                    Ok(frame) => {
                        ring_buffer.push(frame);
                    }
                    Err(err) => {
                        debug!(?err, "live view frame poll transient error");
                        thread::sleep(Duration::from_millis(8));
                    }
                }

                if last_event_poll.elapsed() >= Duration::from_millis(250) {
                    if let Ok(updates) = driver.poll_events() {
                        if !updates.is_empty() {
                            *capabilities.write() = driver.capabilities().clone();
                        }
                    }
                    last_event_poll = Instant::now();
                }

                let cap = fps_cap.load(Ordering::Relaxed).clamp(15, 60);
                let min_interval = Duration::from_secs_f32(1.0 / cap as f32);
                while let Some(rem) = min_interval.checked_sub(frame_start.elapsed()) {
                    if rem.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(rem) {
                        Ok(cmd) => {
                            if Self::handle_command(
                                cmd,
                                &mut *driver,
                                &mut live_view_active,
                                &capabilities,
                                &streaming,
                            ) {
                                return;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            let _ = driver.disconnect();
                            return;
                        }
                    }
                }
            } else {
                // Idle standby: block briefly for commands while polling physical dial events at 4 Hz.
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(cmd) => {
                        if Self::handle_command(
                            cmd,
                            &mut *driver,
                            &mut live_view_active,
                            &capabilities,
                            &streaming,
                        ) {
                            return;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Ok(updates) = driver.poll_events() {
                            if !updates.is_empty() {
                                *capabilities.write() = driver.capabilities().clone();
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        let _ = driver.disconnect();
                        return;
                    }
                }
            }
        }
    }

    fn handle_command(
        cmd: WorkerCommand,
        driver: &mut dyn CameraDriver,
        live_view_active: &mut bool,
        capabilities: &RwLock<CameraCapabilities>,
        streaming: &AtomicBool,
    ) -> bool {
        match cmd {
            WorkerCommand::SetLiveView(enable, reply) => {
                let res = if enable && !*live_view_active {
                    driver.start_live_view().inspect(|()| {
                        *live_view_active = true;
                        streaming.store(true, Ordering::Relaxed);
                    })
                } else if !enable && *live_view_active {
                    driver.stop_live_view().inspect(|()| {
                        *live_view_active = false;
                        streaming.store(false, Ordering::Relaxed);
                    })
                } else {
                    Ok(())
                };
                let _ = reply.send(res);
                false
            }
            WorkerCommand::SetProperty(prop, raw_val, reply) => {
                let res = driver.set_property(prop, raw_val).map(|()| {
                    let _ = driver.poll_events();
                    driver.capabilities().clone()
                });
                if let Ok(ref updated) = res {
                    *capabilities.write() = updated.clone();
                }
                let _ = reply.send(res);
                false
            }
            WorkerCommand::TriggerAutofocus(x, y, reply) => {
                let res = driver.trigger_autofocus(x, y);
                if res.is_ok() {
                    *capabilities.write() = driver.capabilities().clone();
                }
                let _ = reply.send(res);
                false
            }
            WorkerCommand::DriveManualFocus(step, reply) => {
                let _ = reply.send(driver.drive_manual_focus(step));
                false
            }
            WorkerCommand::Shutdown(reply) => {
                if *live_view_active {
                    if let Err(err) = driver.stop_live_view() {
                        warn!(?err, "failed to stop live view during shutdown");
                    }
                    streaming.store(false, Ordering::Relaxed);
                }
                if let Err(err) = driver.disconnect() {
                    warn!(?err, "failed to close camera session cleanly");
                }
                let _ = reply.send(());
                true
            }
        }
    }

    pub fn capabilities(&self) -> CameraCapabilities {
        self.capabilities.read().clone()
    }

    pub fn ring_buffer(&self) -> &Arc<MailboxRingBuffer<LiveViewFrame>> {
        &self.ring_buffer
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming.load(Ordering::Relaxed)
    }

    pub fn set_fps_cap(&self, fps_cap: u32) {
        self.fps_cap.store(fps_cap.clamp(15, 60), Ordering::Relaxed);
    }

    pub fn start_live_view(&self) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx.send(WorkerCommand::SetLiveView(true, reply_tx))?;
        reply_rx.recv()?
    }

    pub fn stop_live_view(&self) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx.send(WorkerCommand::SetLiveView(false, reply_tx))?;
        reply_rx.recv()?
    }

    pub fn set_property(
        &self,
        prop: CameraPropertyId,
        raw_value: u32,
    ) -> anyhow::Result<CameraCapabilities> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(WorkerCommand::SetProperty(prop, raw_value, reply_tx))?;
        reply_rx.recv()?
    }

    pub fn trigger_autofocus(
        &self,
        norm_x: Option<f32>,
        norm_y: Option<f32>,
    ) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(WorkerCommand::TriggerAutofocus(norm_x, norm_y, reply_tx))?;
        reply_rx.recv()?
    }

    pub fn drive_manual_focus(&self, step: i32) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(WorkerCommand::DriveManualFocus(step, reply_tx))?;
        reply_rx.recv()?
    }

    pub fn shutdown(&mut self) {
        if let Some(handle) = self.join_handle.take() {
            let (reply_tx, reply_rx) = mpsc::channel();
            if self.tx.send(WorkerCommand::Shutdown(reply_tx)).is_ok() {
                let _ = reply_rx.recv_timeout(Duration::from_secs(3));
            }
            let _ = handle.join();
            info!("camera session worker stopped cleanly");
        }
    }
}

impl Drop for CameraSessionWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_embedded_jpeg_and_reads_sof_dimensions() {
        let mut payload = vec![0x00; 16];
        // Minimal valid JPEG SOI + SOF0 (1920x1080) + EOI
        payload.extend_from_slice(&[
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x04, 0x38, 0x07, 0x80, 0x03, 0x01, 0x22,
            0x00, 0xFF, 0xD9,
        ]);
        payload.extend_from_slice(&[0xAA; 8]);

        let jpeg = extract_jpeg_slice(&payload).expect("JPEG slice found");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9]);
        assert_eq!(parse_jpeg_dimensions(jpeg), (1920, 1080));
    }
}
