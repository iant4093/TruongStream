#[cfg(target_os = "macos")]
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Once};
#[cfg(target_os = "macos")]
use std::time::Instant;
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use tracing::{debug, warn};
use truongstream_core::drivers::hdmi::{
    register_hdmi_provider, HdmiCaptureProvider, HdmiCaptureStream, HdmiDeviceInfo,
};
use truongstream_core::drivers::{parse_jpeg_dimensions, FramePixelFormat, LiveViewFrame};
use truongstream_core::lut::{
    process_jpeg_with_lut, process_live_frame_with_lut, BuiltinLutPreset, CubeLut,
    LutRenderSettings, ProcessedFrame, VirtualCamStatus,
};

/// Embedded Metal compute kernel source (`apply_lut_and_convert_nv12`).
pub const METAL_LUT_SHADER_SOURCE: &str = include_str!("../VideoPipeline/LutPipeline.metal");

/// Embedded Swift VideoToolbox + Metal + IOSurface decoder source.
pub const SWIFT_VIDEOTOOLBOX_SOURCE: &str =
    include_str!("../VideoPipeline/VideoToolboxDecoder.swift");

/// Embedded Swift CMIOExtension provider source.
pub const SWIFT_CMIO_EXTENSION_SOURCE: &str =
    include_str!("../VirtualCamExtension/ExtensionProvider.swift");

const CMIO_BRIDGE_PATH: &str = "/tmp/truongstream-cmio-bridge.bin";
const CMIO_BRIDGE_MAGIC: u32 = 0x5453_434D;

#[cfg(target_os = "macos")]
mod ffi {
    use std::os::raw::c_char;

    extern "C" {
        pub fn ts_macos_pipeline_create(shader_source: *const c_char) -> *mut std::ffi::c_void;
        pub fn ts_macos_pipeline_destroy(ptr: *mut std::ffi::c_void);
        pub fn ts_macos_pipeline_upload_lut(
            ptr: *mut std::ffi::c_void,
            size: i32,
            rgba32_float_ptr: *const f32,
        ) -> bool;
        pub fn ts_macos_free_buffer(ptr: *mut u8);
        pub fn ts_macos_pipeline_process_jpeg(
            ptr: *mut std::ffi::c_void,
            jpeg_ptr: *const u8,
            jpeg_len: usize,
            width_hint: u32,
            height_hint: u32,
            target_width: u32,
            target_height: u32,
            intensity: f32,
            mirror_h: bool,
            flip_v: bool,
            crop_zoom: f32,
            dom_min_r: f32,
            dom_min_g: f32,
            dom_min_b: f32,
            dom_max_r: f32,
            dom_max_g: f32,
            dom_max_b: f32,
            want_nv12_bytes: bool,
            want_preview_jpeg: bool,
            out_width: *mut u32,
            out_height: *mut u32,
            out_iosurface_id: *mut u32,
            out_gpu_ms: *mut f32,
            out_y_ptr: *mut *mut u8,
            out_y_len: *mut usize,
            out_uv_ptr: *mut *mut u8,
            out_uv_len: *mut usize,
            out_preview_ptr: *mut *mut u8,
            out_preview_len: *mut usize,
        ) -> bool;
        pub fn ts_macos_pipeline_process_bgra(
            ptr: *mut std::ffi::c_void,
            bgra_ptr: *const u8,
            bgra_len: usize,
            src_width: u32,
            src_height: u32,
            target_width: u32,
            target_height: u32,
            intensity: f32,
            mirror_h: bool,
            flip_v: bool,
            crop_zoom: f32,
            dom_min_r: f32,
            dom_min_g: f32,
            dom_min_b: f32,
            dom_max_r: f32,
            dom_max_g: f32,
            dom_max_b: f32,
            want_nv12_bytes: bool,
            want_preview_jpeg: bool,
            out_width: *mut u32,
            out_height: *mut u32,
            out_iosurface_id: *mut u32,
            out_gpu_ms: *mut f32,
            out_y_ptr: *mut *mut u8,
            out_y_len: *mut usize,
            out_uv_ptr: *mut *mut u8,
            out_uv_len: *mut usize,
            out_preview_ptr: *mut *mut u8,
            out_preview_len: *mut usize,
        ) -> bool;
        pub fn ts_macos_activate_camera_extension() -> *mut c_char;
        pub fn ts_macos_free_cstr(ptr: *mut c_char);
        pub fn ts_macos_hdmi_list_devices() -> *mut c_char;
        pub fn ts_macos_hdmi_open(
            device_id: *const c_char,
            width: u32,
            height: u32,
            fps: u32,
        ) -> *mut std::ffi::c_void;
        pub fn ts_macos_hdmi_set_format(
            ptr: *mut std::ffi::c_void,
            width: u32,
            height: u32,
            fps: u32,
        ) -> bool;
        pub fn ts_macos_hdmi_next_frame(
            ptr: *mut std::ffi::c_void,
            out_bgra_ptr: *mut *mut u8,
            out_bgra_len: *mut usize,
            out_width: *mut u32,
            out_height: *mut u32,
            out_seq: *mut u64,
        ) -> bool;
        pub fn ts_macos_hdmi_close(ptr: *mut std::ffi::c_void);
    }
}

#[cfg(target_os = "macos")]
struct NativeMetalPipeline {
    ptr: *mut std::ffi::c_void,
}

#[cfg(target_os = "macos")]
unsafe impl Send for NativeMetalPipeline {}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct FfiOutputs {
    width: u32,
    height: u32,
    iosurface_id: u32,
    gpu_ms: f32,
    y_ptr: *mut u8,
    y_len: usize,
    uv_ptr: *mut u8,
    uv_len: usize,
    preview_ptr: *mut u8,
    preview_len: usize,
}

#[cfg(target_os = "macos")]
impl FfiOutputs {
    fn into_processed_frame(self, ok: bool) -> Option<ProcessedFrame> {
        if !ok {
            unsafe {
                if !self.y_ptr.is_null() {
                    ffi::ts_macos_free_buffer(self.y_ptr);
                }
                if !self.uv_ptr.is_null() {
                    ffi::ts_macos_free_buffer(self.uv_ptr);
                }
                if !self.preview_ptr.is_null() {
                    ffi::ts_macos_free_buffer(self.preview_ptr);
                }
            }
            return None;
        }

        let y_plane = unsafe { take_c_vec(self.y_ptr, self.y_len) };
        let uv_plane = unsafe { take_c_vec(self.uv_ptr, self.uv_len) };
        let preview_jpeg = unsafe { take_c_vec(self.preview_ptr, self.preview_len) };
        Some(ProcessedFrame {
            width: self.width,
            height: self.height,
            iosurface_id: self.iosurface_id,
            y_plane,
            uv_plane,
            preview_jpeg,
            pipeline_ms: self.gpu_ms,
        })
    }
}

#[cfg(target_os = "macos")]
impl NativeMetalPipeline {
    fn new(shader_source: &str, initial_lut: &CubeLut) -> Option<Self> {
        let c_shader = CString::new(shader_source).ok()?;
        let ptr = unsafe { ffi::ts_macos_pipeline_create(c_shader.as_ptr()) };
        if ptr.is_null() {
            return None;
        }
        let native = Self { ptr };
        native.upload_lut(initial_lut);
        Some(native)
    }

    fn upload_lut(&self, lut: &CubeLut) {
        let ok = unsafe {
            ffi::ts_macos_pipeline_upload_lut(
                self.ptr,
                lut.size as i32,
                lut.data.as_ptr().cast::<f32>(),
            )
        };
        if !ok {
            warn!(size = lut.size, "failed uploading 3D LUT to Metal texture");
        }
    }

    fn process_jpeg(
        &self,
        jpeg_bytes: &[u8],
        lut: &CubeLut,
        settings: &LutRenderSettings,
        want_nv12_bytes: bool,
        want_preview_jpeg: bool,
    ) -> Option<ProcessedFrame> {
        let (w_hint, h_hint) = parse_jpeg_dimensions(jpeg_bytes);
        let mut out = FfiOutputs::default();
        let ok = unsafe {
            ffi::ts_macos_pipeline_process_jpeg(
                self.ptr,
                jpeg_bytes.as_ptr(),
                jpeg_bytes.len(),
                w_hint,
                h_hint,
                settings.output_width,
                settings.output_height,
                settings.intensity,
                settings.mirror_horizontal,
                settings.flip_vertical,
                settings.crop_zoom,
                lut.domain_min[0],
                lut.domain_min[1],
                lut.domain_min[2],
                lut.domain_max[0],
                lut.domain_max[1],
                lut.domain_max[2],
                want_nv12_bytes,
                want_preview_jpeg,
                &mut out.width,
                &mut out.height,
                &mut out.iosurface_id,
                &mut out.gpu_ms,
                &mut out.y_ptr,
                &mut out.y_len,
                &mut out.uv_ptr,
                &mut out.uv_len,
                &mut out.preview_ptr,
                &mut out.preview_len,
            )
        };
        out.into_processed_frame(ok)
    }

    fn process_bgra(
        &self,
        bgra_bytes: &[u8],
        (src_width, src_height): (u32, u32),
        lut: &CubeLut,
        settings: &LutRenderSettings,
        want_nv12_bytes: bool,
        want_preview_jpeg: bool,
    ) -> Option<ProcessedFrame> {
        let mut out = FfiOutputs::default();
        let ok = unsafe {
            ffi::ts_macos_pipeline_process_bgra(
                self.ptr,
                bgra_bytes.as_ptr(),
                bgra_bytes.len(),
                src_width,
                src_height,
                settings.output_width,
                settings.output_height,
                settings.intensity,
                settings.mirror_horizontal,
                settings.flip_vertical,
                settings.crop_zoom,
                lut.domain_min[0],
                lut.domain_min[1],
                lut.domain_min[2],
                lut.domain_max[0],
                lut.domain_max[1],
                lut.domain_max[2],
                want_nv12_bytes,
                want_preview_jpeg,
                &mut out.width,
                &mut out.height,
                &mut out.iosurface_id,
                &mut out.gpu_ms,
                &mut out.y_ptr,
                &mut out.y_len,
                &mut out.uv_ptr,
                &mut out.uv_len,
                &mut out.preview_ptr,
                &mut out.preview_len,
            )
        };
        out.into_processed_frame(ok)
    }
}

#[cfg(target_os = "macos")]
unsafe fn take_c_vec(ptr: *mut u8, len: usize) -> Vec<u8> {
    if ptr.is_null() || len == 0 {
        if !ptr.is_null() {
            ffi::ts_macos_free_buffer(ptr);
        }
        return Vec::new();
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    let vec = slice.to_vec();
    ffi::ts_macos_free_buffer(ptr);
    vec
}

#[cfg(target_os = "macos")]
impl Drop for NativeMetalPipeline {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffi::ts_macos_pipeline_destroy(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

/// macOS GPU media pipeline coordinating VideoToolbox MJPEG decode, Metal 3D LUT compute,
/// and IOSurface NV12 output for CoreMediaIO.
pub struct MacOsGpuPipeline {
    lut: RwLock<CubeLut>,
    settings: RwLock<LutRenderSettings>,
    #[cfg(target_os = "macos")]
    native: Mutex<Option<NativeMetalPipeline>>,
}

impl Default for MacOsGpuPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Registers the macOS AVFoundation HDMI capture provider with `truongstream-core`.
pub fn ensure_hdmi_provider_registered() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        register_hdmi_provider(Arc::new(MacOsHdmiProvider));
    });
}

struct MacOsHdmiProvider;

impl HdmiCaptureProvider for MacOsHdmiProvider {
    fn enumerate_devices(&self) -> Vec<HdmiDeviceInfo> {
        #[cfg(target_os = "macos")]
        unsafe {
            let ptr = ffi::ts_macos_hdmi_list_devices();
            if ptr.is_null() {
                return Vec::new();
            }
            let raw = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            ffi::ts_macos_free_cstr(ptr);
            raw.lines()
                .filter_map(|line| {
                    let mut cols = line.splitn(5, '\t');
                    let device_id = cols.next()?.trim().to_string();
                    let model_name = cols.next()?.trim().to_string();
                    let max_width = cols.next()?.trim().parse::<u32>().unwrap_or(1920);
                    let max_height = cols.next()?.trim().parse::<u32>().unwrap_or(1080);
                    let max_fps = cols.next()?.trim().parse::<u32>().unwrap_or(60);
                    if device_id.is_empty() || model_name.is_empty() {
                        return None;
                    }
                    Some(HdmiDeviceInfo {
                        device_id,
                        model_name,
                        max_width,
                        max_height,
                        max_fps,
                        usb_speed_mbps: 5000,
                    })
                })
                .collect()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Vec::new()
        }
    }

    fn open_stream(
        &self,
        device_id: &str,
        width: u32,
        height: u32,
        fps: u32,
    ) -> anyhow::Result<Box<dyn HdmiCaptureStream>> {
        #[cfg(target_os = "macos")]
        {
            let c_id = CString::new(device_id)?;
            let ptr = unsafe { ffi::ts_macos_hdmi_open(c_id.as_ptr(), width, height, fps) };
            if ptr.is_null() {
                anyhow::bail!(
                    "Failed to open macOS AVFoundation HDMI capture device '{device_id}'"
                );
            }
            Ok(Box::new(MacOsHdmiStream { ptr }))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (device_id, width, height, fps);
            anyhow::bail!("AVFoundation HDMI capture requires macOS")
        }
    }
}

#[cfg(target_os = "macos")]
struct MacOsHdmiStream {
    ptr: *mut std::ffi::c_void,
}

#[cfg(target_os = "macos")]
unsafe impl Send for MacOsHdmiStream {}

#[cfg(target_os = "macos")]
impl HdmiCaptureStream for MacOsHdmiStream {
    fn next_uncompressed_frame(&mut self) -> anyhow::Result<LiveViewFrame> {
        let start = Instant::now();
        let mut bgra_ptr: *mut u8 = std::ptr::null_mut();
        let mut bgra_len = 0usize;
        let mut width = 0u32;
        let mut height = 0u32;
        let mut sequence = 0u64;
        let ok = unsafe {
            ffi::ts_macos_hdmi_next_frame(
                self.ptr,
                &mut bgra_ptr,
                &mut bgra_len,
                &mut width,
                &mut height,
                &mut sequence,
            )
        };
        if !ok || bgra_ptr.is_null() || bgra_len == 0 {
            if !bgra_ptr.is_null() {
                unsafe { ffi::ts_macos_free_buffer(bgra_ptr) };
            }
            anyhow::bail!("waiting for uncompressed HDMI frame");
        }
        let raw_pixels = unsafe { take_c_vec(bgra_ptr, bgra_len) };
        let timestamp_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        Ok(LiveViewFrame {
            jpeg_bytes: Vec::new(),
            raw_pixels,
            pixel_format: FramePixelFormat::Bgra8,
            sequence,
            timestamp_us,
            width,
            height,
            usb_transfer_ms: start.elapsed().as_secs_f32() * 1000.0,
        })
    }

    fn set_format(&mut self, width: u32, height: u32, fps: u32) -> anyhow::Result<()> {
        let ok = unsafe { ffi::ts_macos_hdmi_set_format(self.ptr, width, height, fps) };
        if !ok {
            anyhow::bail!("failed configuring HDMI capture format {width}x{height}@{fps}");
        }
        Ok(())
    }

    fn stop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffi::ts_macos_hdmi_close(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for MacOsHdmiStream {
    fn drop(&mut self) {
        self.stop();
    }
}

impl MacOsGpuPipeline {
    pub fn new() -> Self {
        ensure_hdmi_provider_registered();
        let initial_lut = BuiltinLutPreset::Neutral.build_lut();
        #[cfg(target_os = "macos")]
        let native = {
            let created = NativeMetalPipeline::new(METAL_LUT_SHADER_SOURCE, &initial_lut);
            if created.is_some() {
                debug!("initialized native macOS VideoToolbox + Metal 3D LUT pipeline");
            } else {
                warn!("Metal device unavailable; falling back to portable CPU LUT pipeline");
            }
            Mutex::new(created)
        };

        Self {
            lut: RwLock::new(initial_lut),
            settings: RwLock::new(LutRenderSettings::default()),
            #[cfg(target_os = "macos")]
            native,
        }
    }

    pub fn is_gpu_accelerated(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.native.lock().is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    pub fn set_lut(&self, lut: CubeLut) {
        #[cfg(target_os = "macos")]
        if let Some(native) = self.native.lock().as_ref() {
            native.upload_lut(&lut);
        }
        *self.lut.write() = lut;
    }

    pub fn set_settings(&self, settings: LutRenderSettings) {
        *self.settings.write() = settings;
    }

    pub fn settings(&self) -> LutRenderSettings {
        self.settings.read().clone()
    }

    /// Renders an ungraded preview JPEG matching current geometry and target output resolution.
    pub fn process_ungraded_preview(&self, jpeg_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut settings = self.settings.read().clone();
        settings.intensity = 0.0;
        let lut = self.lut.read();

        #[cfg(target_os = "macos")]
        if let Some(native) = self.native.lock().as_ref() {
            if let Some(frame) = native.process_jpeg(jpeg_bytes, &lut, &settings, false, true) {
                return Ok(frame.preview_jpeg);
            }
        }

        Ok(process_jpeg_with_lut(jpeg_bytes, &lut, &settings, false)?.preview_jpeg)
    }

    /// Renders an ungraded preview JPEG from either USB JPEG or uncompressed HDMI frames.
    pub fn process_ungraded_live_preview(&self, frame: &LiveViewFrame) -> anyhow::Result<Vec<u8>> {
        if frame.pixel_format == FramePixelFormat::Jpeg {
            return self.process_ungraded_preview(&frame.jpeg_bytes);
        }
        let mut settings = self.settings.read().clone();
        settings.intensity = 0.0;
        let lut = self.lut.read();
        Ok(self
            .process_raw_with_settings(frame, &lut, &settings, false, true)?
            .preview_jpeg)
    }

    pub fn metal_shader_source(&self) -> &'static str {
        METAL_LUT_SHADER_SOURCE
    }

    /// Processes either a compressed JPEG frame or an uncompressed HDMI frame (`BGRA8`/`NV12`/`YUY2`).
    pub fn process_live_frame(
        &self,
        frame: &LiveViewFrame,
        export_nv12_bytes: bool,
    ) -> anyhow::Result<ProcessedFrame> {
        if frame.pixel_format == FramePixelFormat::Jpeg {
            return self.process_frame(&frame.jpeg_bytes, export_nv12_bytes);
        }
        let lut = self.lut.read();
        let settings = self.settings.read();
        self.process_raw_with_settings(frame, &lut, &settings, export_nv12_bytes, true)
    }

    fn process_raw_with_settings(
        &self,
        frame: &LiveViewFrame,
        lut: &CubeLut,
        settings: &LutRenderSettings,
        export_nv12_bytes: bool,
        encode_preview_jpeg: bool,
    ) -> anyhow::Result<ProcessedFrame> {
        let owned_bgra;
        let bgra = match frame.pixel_format {
            FramePixelFormat::Bgra8 => frame.raw_pixels.as_slice(),
            FramePixelFormat::Yuy2 => {
                owned_bgra = truongstream_core::drivers::hdmi::yuy2_to_bgra8(
                    &frame.raw_pixels,
                    frame.width,
                    frame.height,
                );
                &owned_bgra
            }
            FramePixelFormat::Nv12 => {
                owned_bgra = truongstream_core::drivers::hdmi::nv12_to_bgra8(
                    &frame.raw_pixels,
                    frame.width,
                    frame.height,
                );
                &owned_bgra
            }
            FramePixelFormat::Jpeg => {
                return process_live_frame_with_lut(frame, lut, settings, export_nv12_bytes);
            }
        };

        #[cfg(target_os = "macos")]
        if let Some(native) = self.native.lock().as_ref() {
            if let Some(processed) = native.process_bgra(
                bgra,
                (frame.width, frame.height),
                lut,
                settings,
                export_nv12_bytes,
                encode_preview_jpeg,
            ) {
                return Ok(processed);
            }
        }

        let _ = encode_preview_jpeg;
        process_live_frame_with_lut(frame, lut, settings, export_nv12_bytes)
    }

    /// Processes a camera MJPEG frame through the active 3D LUT and framing parameters,
    /// exporting a shared GPU `IOSurfaceID` on macOS and optional NV12/JPEG buffers.
    pub fn process_frame(
        &self,
        jpeg_bytes: &[u8],
        export_nv12_bytes: bool,
    ) -> anyhow::Result<ProcessedFrame> {
        self.process_frame_with_options(jpeg_bytes, export_nv12_bytes, true)
    }

    /// Processes a camera MJPEG frame with explicit control over CPU NV12 readback and JPEG preview encoding.
    pub fn process_frame_with_options(
        &self,
        jpeg_bytes: &[u8],
        export_nv12_bytes: bool,
        encode_preview_jpeg: bool,
    ) -> anyhow::Result<ProcessedFrame> {
        let lut = self.lut.read();
        let settings = self.settings.read();

        #[cfg(target_os = "macos")]
        if let Some(native) = self.native.lock().as_ref() {
            if let Some(frame) = native.process_jpeg(
                jpeg_bytes,
                &lut,
                &settings,
                export_nv12_bytes,
                encode_preview_jpeg,
            ) {
                return Ok(frame);
            }
        }

        let _ = encode_preview_jpeg;
        process_jpeg_with_lut(jpeg_bytes, &lut, &settings, export_nv12_bytes)
    }
}

/// Bridge to the macOS CoreMediaIO (`CMIOExtension`) virtual camera device.
#[derive(Clone)]
pub struct MacOsVirtualCamSink {
    active: Arc<AtomicBool>,
    consumers: Arc<AtomicU32>,
    last_iosurface_id: Arc<AtomicU32>,
    resolution: Arc<RwLock<(u32, u32, u32)>>,
    #[cfg(unix)]
    bridge_file: Arc<Mutex<Option<std::fs::File>>>,
}

impl Default for MacOsVirtualCamSink {
    fn default() -> Self {
        Self::new()
    }
}

impl MacOsVirtualCamSink {
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            consumers: Arc::new(AtomicU32::new(0)),
            last_iosurface_id: Arc::new(AtomicU32::new(0)),
            resolution: Arc::new(RwLock::new((1920, 1080, 60))),
            #[cfg(unix)]
            bridge_file: Arc::new(Mutex::new(None)),
        }
    }

    pub fn start(&self, width: u32, height: u32, fps: u32) {
        *self.resolution.write() = (width, height, fps);
        self.active.store(true, Ordering::Relaxed);
        self.write_bridge_header(
            self.last_iosurface_id.load(Ordering::Relaxed),
            width,
            height,
            0,
        );
    }

    pub fn stop(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.last_iosurface_id.store(0, Ordering::Relaxed);
        let (w, h, _) = *self.resolution.read();
        self.write_bridge_header(0, w, h, 0);
    }

    pub fn set_consumer_count(&self, count: u32) {
        self.consumers.store(count, Ordering::Relaxed);
    }

    pub fn consumer_count(&self) -> u32 {
        self.consumers.load(Ordering::Relaxed)
    }

    pub fn last_iosurface_id(&self) -> u32 {
        self.last_iosurface_id.load(Ordering::Relaxed)
    }

    /// Publishes an `IOSurfaceID` to the shared CMIOExtension bridge and reads back active consumer count.
    pub fn publish_surface(&self, iosurface_id: u32, width: u32, height: u32, sequence: u64) {
        self.last_iosurface_id
            .store(iosurface_id, Ordering::Relaxed);
        self.write_bridge_header(iosurface_id, width, height, sequence);
    }

    fn write_bridge_header(&self, iosurface_id: u32, width: u32, height: u32, sequence: u64) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;

            let mut guard = self.bridge_file.lock();
            if guard.is_none() {
                if let Ok(file) = std::fs::OpenOptions::new()
                    .create(true)
                    .read(true)
                    .write(true)
                    .truncate(false)
                    .open(CMIO_BRIDGE_PATH)
                {
                    let _ = file.set_len(64);
                    *guard = Some(file);
                }
            }
            let Some(file) = guard.as_ref() else {
                return;
            };

            let mut existing = [0u8; 64];
            if file.read_at(&mut existing, 0).is_ok() {
                let magic =
                    u32::from_le_bytes([existing[0], existing[1], existing[2], existing[3]]);
                if magic == CMIO_BRIDGE_MAGIC {
                    let ext_consumers = u32::from_le_bytes([
                        existing[12],
                        existing[13],
                        existing[14],
                        existing[15],
                    ]);
                    if ext_consumers > 0 {
                        self.consumers.store(ext_consumers, Ordering::Relaxed);
                    }
                }
            }

            let (_, _, fps) = *self.resolution.read();
            let active = u32::from(self.active.load(Ordering::Relaxed));
            let now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);

            let mut buf = [0u8; 64];
            buf[0..4].copy_from_slice(&CMIO_BRIDGE_MAGIC.to_le_bytes());
            buf[4..8].copy_from_slice(&1u32.to_le_bytes());
            buf[8..12].copy_from_slice(&active.to_le_bytes());
            buf[12..16].copy_from_slice(&self.consumer_count().to_le_bytes());
            buf[16..20].copy_from_slice(&width.to_le_bytes());
            buf[20..24].copy_from_slice(&height.to_le_bytes());
            buf[24..28].copy_from_slice(&fps.to_le_bytes());
            buf[28..32].copy_from_slice(&iosurface_id.to_le_bytes());
            buf[32..40].copy_from_slice(&sequence.to_le_bytes());
            buf[40..48].copy_from_slice(&now_ns.to_le_bytes());

            let _ = file.write_at(&buf[0..12], 0);
            let _ = file.write_at(&buf[16..48], 16);
        }

        #[cfg(not(unix))]
        {
            let _ = (iosurface_id, width, height, sequence);
        }
    }

    pub fn status(&self) -> VirtualCamStatus {
        let (output_width, output_height, fps_cap) = *self.resolution.read();
        VirtualCamStatus {
            active: self.active.load(Ordering::Relaxed),
            device_name: String::from("TruongStream Virtual Camera"),
            backend: String::from("CoreMediaIO (CMIOExtension)"),
            consumer_count: self.consumer_count(),
            output_width,
            output_height,
            fps_cap,
        }
    }

    pub fn activate_camera_extension(&self) -> String {
        #[cfg(target_os = "macos")]
        unsafe {
            let ptr = ffi::ts_macos_activate_camera_extension();
            if ptr.is_null() {
                return String::from("Failed to query macOS Camera Extension status.");
            }
            let msg = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            ffi::ts_macos_free_cstr(ptr);
            msg
        }
        #[cfg(not(target_os = "macos"))]
        {
            String::from("macOS Camera Extension is only available on macOS.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;

    fn sample_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([(x * 10) as u8, (y * 10) as u8, 128])
        });
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
            .unwrap();
        out
    }

    #[test]
    fn metal_shader_and_swift_sources_embedded() {
        assert!(METAL_LUT_SHADER_SOURCE.contains("apply_lut_and_convert_nv12"));
        assert!(SWIFT_VIDEOTOOLBOX_SOURCE.contains("VTDecompressionSession"));
        assert!(SWIFT_CMIO_EXTENSION_SOURCE.contains("CMIOExtensionProviderSource"));
    }

    #[test]
    fn grades_frame_and_exports_bt709_nv12_planes() {
        let pipeline = MacOsGpuPipeline::new();
        pipeline.set_lut(BuiltinLutPreset::TealOrange.build_lut());
        pipeline.set_settings(LutRenderSettings {
            intensity: 0.75,
            mirror_horizontal: true,
            flip_vertical: false,
            crop_zoom: 1.0,
            ..Default::default()
        });

        let jpeg = sample_jpeg(32, 24);
        let frame = pipeline.process_frame(&jpeg, true).unwrap();
        assert_eq!((frame.width, frame.height), (32, 24));
        assert_eq!(frame.y_plane.len(), 32 * 24);
        assert_eq!(frame.uv_plane.len(), 16 * 12 * 2);
        assert!(frame.preview_jpeg.starts_with(&[0xFF, 0xD8]));
        if pipeline.is_gpu_accelerated() {
            assert_ne!(frame.iosurface_id, 0);
        }

        // Verify hardware scaling to explicit 1080p and 4K UHD target dimensions.
        pipeline.set_settings(LutRenderSettings {
            output_width: 1920,
            output_height: 1080,
            ..Default::default()
        });
        let scaled_1080 = pipeline.process_frame(&jpeg, false).unwrap();
        assert_eq!((scaled_1080.width, scaled_1080.height), (1920, 1080));
        assert!(scaled_1080.preview_jpeg.starts_with(&[0xFF, 0xD8]));

        pipeline.set_settings(LutRenderSettings {
            output_width: 3840,
            output_height: 2160,
            ..Default::default()
        });
        let scaled_4k = pipeline.process_frame(&jpeg, false).unwrap();
        assert_eq!((scaled_4k.width, scaled_4k.height), (3840, 2160));
        assert!(scaled_4k.preview_jpeg.starts_with(&[0xFF, 0xD8]));
        eprintln!(
            "Metal scaling timings: 1080p={:.2}ms, 4K={:.2}ms",
            scaled_1080.pipeline_ms, scaled_4k.pipeline_ms
        );
    }

    #[test]
    fn virtual_cam_tracks_consumers_and_publishes_surface() {
        let sink = MacOsVirtualCamSink::new();
        sink.start(1920, 1080, 60);
        sink.set_consumer_count(2);
        sink.publish_surface(42, 1920, 1080, 1);
        let status = sink.status();
        assert!(status.active);
        assert_eq!(status.consumer_count, 2);
        assert_eq!(sink.last_iosurface_id(), 42);
        assert_eq!(status.backend, "CoreMediaIO (CMIOExtension)");
    }

    #[test]
    fn grades_uncompressed_bgra_frame_on_metal_pipeline() {
        let pipeline = MacOsGpuPipeline::new();
        pipeline.set_lut(BuiltinLutPreset::TealOrange.build_lut());
        pipeline.set_settings(LutRenderSettings {
            output_width: 1920,
            output_height: 1080,
            intensity: 1.0,
            ..Default::default()
        });
        let raw_frame = LiveViewFrame {
            jpeg_bytes: Vec::new(),
            raw_pixels: vec![120u8; 64 * 36 * 4],
            pixel_format: FramePixelFormat::Bgra8,
            sequence: 1,
            timestamp_us: 1,
            width: 64,
            height: 36,
            usb_transfer_ms: 0.2,
        };
        let out_1080 = pipeline.process_live_frame(&raw_frame, true).unwrap();
        assert_eq!((out_1080.width, out_1080.height), (1920, 1080));
        assert_eq!(out_1080.y_plane.len(), 1920 * 1080);
        assert!(out_1080.preview_jpeg.starts_with(&[0xFF, 0xD8]));

        pipeline.set_settings(LutRenderSettings {
            output_width: 3840,
            output_height: 2160,
            intensity: 0.9,
            ..Default::default()
        });
        let out_4k = pipeline.process_live_frame(&raw_frame, false).unwrap();
        assert_eq!((out_4k.width, out_4k.height), (3840, 2160));
        assert!(out_4k.preview_jpeg.starts_with(&[0xFF, 0xD8]));
    }
}
