#[cfg(target_os = "windows")]
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Once};
#[cfg(target_os = "windows")]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "windows")]
use image::ImageFormat;
#[cfg(target_os = "windows")]
use parking_lot::Mutex;
use parking_lot::RwLock;
#[cfg(target_os = "windows")]
use tracing::{debug, warn};
use truongstream_core::drivers::hdmi::{
    register_hdmi_provider, HdmiCaptureProvider, HdmiCaptureStream, HdmiDeviceInfo,
};
use truongstream_core::drivers::{FramePixelFormat, LiveViewFrame};
use truongstream_core::lut::{
    process_jpeg_with_lut, process_live_frame_with_lut, BuiltinLutPreset, CubeLut,
    LutRenderSettings, ProcessedFrame, VirtualCamStatus,
};

/// Embedded HLSL DirectCompute 3D LUT + BT.709 NV12 shader source.
pub const HLSL_LUT_SHADER_SOURCE: &str = include_str!("../VideoPipeline/LutPipeline.hlsl");

/// Embedded Windows Portable Devices (WPD) C++ pass-through source.
pub const WPD_TRANSPORT_SOURCE: &str = include_str!("../Transport/WpdPtpTransport.cpp");

/// Embedded Windows Media Foundation HDMI capture C++ source.
pub const MF_HDMI_CAPTURE_SOURCE: &str = include_str!("../VideoPipeline/MfHdmiCapture.cpp");

/// Embedded Media Foundation (`MFCreateVirtualCamera`) C++ sink source.
pub const MF_VIRTUAL_CAM_SOURCE: &str = include_str!("../VirtualCamSink/MfVirtualCamSink.cpp");

#[cfg(target_os = "windows")]
mod ffi {
    use std::os::raw::c_char;

    extern "C" {
        pub fn ts_win_pipeline_create(shader_source: *const c_char) -> *mut std::ffi::c_void;
        pub fn ts_win_pipeline_destroy(ptr: *mut std::ffi::c_void);
        pub fn ts_win_pipeline_upload_lut(
            ptr: *mut std::ffi::c_void,
            size: i32,
            rgba32_data: *const f32,
        ) -> bool;
        pub fn ts_win_pipeline_process_bgra(
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
            want_preview_rgb: bool,
            out_width: *mut u32,
            out_height: *mut u32,
            out_gpu_ms: *mut f32,
            out_y_ptr: *mut *mut u8,
            out_y_len: *mut usize,
            out_uv_ptr: *mut *mut u8,
            out_uv_len: *mut usize,
            out_rgb_ptr: *mut *mut u8,
            out_rgb_len: *mut usize,
        ) -> bool;
        pub fn ts_win_free_buffer(ptr: *mut u8);

        pub fn ts_win_hdmi_list_devices() -> *mut c_char;
        pub fn ts_win_hdmi_open(
            device_id: *const c_char,
            width: u32,
            height: u32,
            fps: u32,
        ) -> *mut std::ffi::c_void;
        pub fn ts_win_hdmi_set_format(
            ptr: *mut std::ffi::c_void,
            width: u32,
            height: u32,
            fps: u32,
        ) -> bool;
        pub fn ts_win_hdmi_read_frame(
            ptr: *mut std::ffi::c_void,
            out_bgra_ptr: *mut *mut u8,
            out_bgra_len: *mut usize,
            out_width: *mut u32,
            out_height: *mut u32,
            out_seq: *mut u64,
        ) -> bool;
        pub fn ts_win_hdmi_close(ptr: *mut std::ffi::c_void);
        pub fn ts_win_free_cstr(ptr: *mut c_char);

        pub fn ts_win_vcam_create() -> *mut std::ffi::c_void;
        pub fn ts_win_vcam_start(ptr: *mut std::ffi::c_void) -> bool;
        pub fn ts_win_vcam_stop(ptr: *mut std::ffi::c_void);
        pub fn ts_win_vcam_destroy(ptr: *mut std::ffi::c_void);

        pub fn ts_win_wpd_probe() -> u32;
    }
}

#[cfg(target_os = "windows")]
unsafe fn take_c_vec(ptr: *mut u8, len: usize) -> Vec<u8> {
    if ptr.is_null() || len == 0 {
        if !ptr.is_null() {
            ffi::ts_win_free_buffer(ptr);
        }
        return Vec::new();
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    let vec = slice.to_vec();
    ffi::ts_win_free_buffer(ptr);
    vec
}

#[cfg(target_os = "windows")]
fn encode_preview_jpeg_from_rgb(rgb: &[u8], width: u32, height: u32) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(65_536);
    let mut cursor = std::io::Cursor::new(&mut out);
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 86);
    if width > 1920 {
        let prev_w = 1920u32;
        let prev_h = ((height as u64 * prev_w as u64) / width as u64).max(2) as u32 & !1;
        let mut downscaled = vec![0u8; (prev_w * prev_h * 3) as usize];
        for py in 0..prev_h {
            let sy = ((py as u64 * height as u64) / prev_h as u64) as u32;
            for px in 0..prev_w {
                let sx = ((px as u64 * width as u64) / prev_w as u64) as u32;
                let s = ((sy * width + sx) * 3) as usize;
                let d = ((py * prev_w + px) * 3) as usize;
                downscaled[d..d + 3].copy_from_slice(&rgb[s..s + 3]);
            }
        }
        encoder.encode(&downscaled, prev_w, prev_h, image::ExtendedColorType::Rgb8)?;
    } else {
        encoder.encode(rgb, width, height, image::ExtendedColorType::Rgb8)?;
    }
    Ok(out)
}

#[cfg(target_os = "windows")]
struct NativeD3D11Pipeline {
    ptr: *mut std::ffi::c_void,
}

#[cfg(target_os = "windows")]
unsafe impl Send for NativeD3D11Pipeline {}

#[cfg(target_os = "windows")]
impl Drop for NativeD3D11Pipeline {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffi::ts_win_pipeline_destroy(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

#[cfg(target_os = "windows")]
impl NativeD3D11Pipeline {
    fn new(shader_source: &str, initial_lut: &CubeLut) -> Option<Self> {
        let c_shader = CString::new(shader_source).ok()?;
        let ptr = unsafe { ffi::ts_win_pipeline_create(c_shader.as_ptr()) };
        if ptr.is_null() {
            return None;
        }
        let native = Self { ptr };
        native.upload_lut(initial_lut);
        Some(native)
    }

    fn upload_lut(&self, lut: &CubeLut) {
        let ok = unsafe {
            ffi::ts_win_pipeline_upload_lut(
                self.ptr,
                lut.size as i32,
                lut.data.as_ptr().cast::<f32>(),
            )
        };
        if !ok {
            warn!(size = lut.size, "failed uploading 3D LUT to D3D11 texture");
        }
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
        let mut out_w = 0u32;
        let mut out_h = 0u32;
        let mut out_gpu_ms = 0f32;
        let mut out_y_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_y_len = 0usize;
        let mut out_uv_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_uv_len = 0usize;
        let mut out_rgb_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_rgb_len = 0usize;

        let ok = unsafe {
            ffi::ts_win_pipeline_process_bgra(
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
                &mut out_w,
                &mut out_h,
                &mut out_gpu_ms,
                &mut out_y_ptr,
                &mut out_y_len,
                &mut out_uv_ptr,
                &mut out_uv_len,
                &mut out_rgb_ptr,
                &mut out_rgb_len,
            )
        };

        if !ok {
            unsafe {
                if !out_y_ptr.is_null() {
                    ffi::ts_win_free_buffer(out_y_ptr);
                }
                if !out_uv_ptr.is_null() {
                    ffi::ts_win_free_buffer(out_uv_ptr);
                }
                if !out_rgb_ptr.is_null() {
                    ffi::ts_win_free_buffer(out_rgb_ptr);
                }
            }
            return None;
        }

        let y_plane = unsafe { take_c_vec(out_y_ptr, out_y_len) };
        let uv_plane = unsafe { take_c_vec(out_uv_ptr, out_uv_len) };
        let rgb_bytes = unsafe { take_c_vec(out_rgb_ptr, out_rgb_len) };
        let preview_jpeg = if want_preview_jpeg && !rgb_bytes.is_empty() {
            encode_preview_jpeg_from_rgb(&rgb_bytes, out_w, out_h).ok()?
        } else {
            Vec::new()
        };

        Some(ProcessedFrame {
            width: out_w,
            height: out_h,
            iosurface_id: 0,
            y_plane,
            uv_plane,
            preview_jpeg,
            pipeline_ms: out_gpu_ms,
        })
    }
}

/// Queries connected Windows Portable Devices (WPD / MTP / PTP) camera count.
pub fn probe_wpd_camera_count() -> u32 {
    #[cfg(target_os = "windows")]
    unsafe {
        ffi::ts_win_wpd_probe()
    }
    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

/// Registers the Windows Media Foundation HDMI capture provider with `truongstream-core`.
pub fn ensure_hdmi_provider_registered() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        register_hdmi_provider(Arc::new(WindowsHdmiProvider));
    });
}

struct WindowsHdmiProvider;

impl HdmiCaptureProvider for WindowsHdmiProvider {
    fn enumerate_devices(&self) -> Vec<HdmiDeviceInfo> {
        #[cfg(target_os = "windows")]
        unsafe {
            let ptr = ffi::ts_win_hdmi_list_devices();
            if ptr.is_null() {
                return Vec::new();
            }
            let raw = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            ffi::ts_win_free_cstr(ptr);
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
        #[cfg(not(target_os = "windows"))]
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
        #[cfg(target_os = "windows")]
        {
            let c_id = CString::new(device_id)?;
            let ptr = unsafe { ffi::ts_win_hdmi_open(c_id.as_ptr(), width, height, fps) };
            if ptr.is_null() {
                anyhow::bail!(
                    "Failed to open Windows Media Foundation HDMI capture device '{device_id}'"
                );
            }
            Ok(Box::new(WindowsHdmiStream { ptr }))
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (device_id, width, height, fps);
            anyhow::bail!("Media Foundation HDMI capture requires Windows")
        }
    }
}

#[cfg(target_os = "windows")]
struct WindowsHdmiStream {
    ptr: *mut std::ffi::c_void,
}

#[cfg(target_os = "windows")]
unsafe impl Send for WindowsHdmiStream {}

#[cfg(target_os = "windows")]
impl HdmiCaptureStream for WindowsHdmiStream {
    fn next_uncompressed_frame(&mut self) -> anyhow::Result<LiveViewFrame> {
        let start = Instant::now();
        let mut bgra_ptr: *mut u8 = std::ptr::null_mut();
        let mut bgra_len = 0usize;
        let mut width = 0u32;
        let mut height = 0u32;
        let mut sequence = 0u64;
        let ok = unsafe {
            ffi::ts_win_hdmi_read_frame(
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
                unsafe { ffi::ts_win_free_buffer(bgra_ptr) };
            }
            anyhow::bail!("waiting for uncompressed Windows HDMI frame");
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
        let ok = unsafe { ffi::ts_win_hdmi_set_format(self.ptr, width, height, fps) };
        if !ok {
            anyhow::bail!("failed configuring Windows HDMI format {width}x{height}@{fps}");
        }
        Ok(())
    }

    fn stop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffi::ts_win_hdmi_close(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsHdmiStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Windows D3D11 + HLSL 3D LUT video pipeline wrapper.
pub struct WindowsGpuPipeline {
    lut: RwLock<CubeLut>,
    settings: RwLock<LutRenderSettings>,
    #[cfg(target_os = "windows")]
    native: Mutex<Option<NativeD3D11Pipeline>>,
}

impl Default for WindowsGpuPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsGpuPipeline {
    pub fn new() -> Self {
        ensure_hdmi_provider_registered();
        let initial_lut = BuiltinLutPreset::Neutral.build_lut();
        #[cfg(target_os = "windows")]
        let native = {
            let created = NativeD3D11Pipeline::new(HLSL_LUT_SHADER_SOURCE, &initial_lut);
            if created.is_some() {
                debug!("initialized native Windows D3D11 + HLSL DirectCompute 3D LUT pipeline");
            } else {
                warn!("D3D11 compute unavailable; falling back to portable CPU LUT pipeline");
            }
            Mutex::new(created)
        };

        Self {
            lut: RwLock::new(initial_lut),
            settings: RwLock::new(LutRenderSettings::default()),
            #[cfg(target_os = "windows")]
            native,
        }
    }

    pub fn is_gpu_accelerated(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            self.native.lock().is_some()
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    pub fn set_lut(&self, lut: CubeLut) {
        #[cfg(target_os = "windows")]
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

    pub fn process_ungraded_preview(&self, jpeg_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut settings = self.settings.read().clone();
        settings.intensity = 0.0;
        let lut = self.lut.read();

        #[cfg(target_os = "windows")]
        if let Some(frame) = self.try_gpu_jpeg(jpeg_bytes, &lut, &settings, false, true)? {
            return Ok(frame.preview_jpeg);
        }

        Ok(process_jpeg_with_lut(jpeg_bytes, &lut, &settings, false)?.preview_jpeg)
    }

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

    pub fn hlsl_shader_source(&self) -> &'static str {
        HLSL_LUT_SHADER_SOURCE
    }

    pub fn process_frame(
        &self,
        jpeg_bytes: &[u8],
        export_nv12: bool,
    ) -> anyhow::Result<ProcessedFrame> {
        let lut = self.lut.read();
        let settings = self.settings.read();

        #[cfg(target_os = "windows")]
        if let Some(frame) = self.try_gpu_jpeg(jpeg_bytes, &lut, &settings, export_nv12, true)? {
            return Ok(frame);
        }

        process_jpeg_with_lut(jpeg_bytes, &lut, &settings, export_nv12)
    }

    pub fn process_live_frame(
        &self,
        frame: &LiveViewFrame,
        export_nv12: bool,
    ) -> anyhow::Result<ProcessedFrame> {
        if frame.pixel_format == FramePixelFormat::Jpeg {
            return self.process_frame(&frame.jpeg_bytes, export_nv12);
        }
        let lut = self.lut.read();
        let settings = self.settings.read();
        self.process_raw_with_settings(frame, &lut, &settings, export_nv12, true)
    }

    #[cfg(target_os = "windows")]
    fn try_gpu_jpeg(
        &self,
        jpeg_bytes: &[u8],
        lut: &CubeLut,
        settings: &LutRenderSettings,
        want_nv12: bool,
        want_preview: bool,
    ) -> anyhow::Result<Option<ProcessedFrame>> {
        if self.native.lock().is_none() {
            return Ok(None);
        }
        let decoded =
            image::load_from_memory_with_format(jpeg_bytes, ImageFormat::Jpeg)?.to_rgba8();
        let w = decoded.width();
        let h = decoded.height();
        let mut bgra = decoded.into_raw();
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let guard = self.native.lock();
        if let Some(native) = guard.as_ref() {
            return Ok(native.process_bgra(&bgra, (w, h), lut, settings, want_nv12, want_preview));
        }
        Ok(None)
    }

    fn process_raw_with_settings(
        &self,
        frame: &LiveViewFrame,
        lut: &CubeLut,
        settings: &LutRenderSettings,
        export_nv12: bool,
        encode_preview_jpeg: bool,
    ) -> anyhow::Result<ProcessedFrame> {
        #[cfg(target_os = "windows")]
        {
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
                    return process_live_frame_with_lut(frame, lut, settings, export_nv12);
                }
            };

            if let Some(native) = self.native.lock().as_ref() {
                if let Some(processed) = native.process_bgra(
                    bgra,
                    (frame.width, frame.height),
                    lut,
                    settings,
                    export_nv12,
                    encode_preview_jpeg,
                ) {
                    return Ok(processed);
                }
            }
        }

        let _ = encode_preview_jpeg;
        process_live_frame_with_lut(frame, lut, settings, export_nv12)
    }
}

#[cfg(target_os = "windows")]
struct NativeVcamHandle {
    ptr: *mut std::ffi::c_void,
}

#[cfg(target_os = "windows")]
unsafe impl Send for NativeVcamHandle {}

#[cfg(target_os = "windows")]
impl Drop for NativeVcamHandle {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffi::ts_win_vcam_destroy(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

/// Bridge to Windows Media Foundation (`MFCreateVirtualCamera`) sink.
#[derive(Clone)]
pub struct WindowsVirtualCamSink {
    active: Arc<AtomicBool>,
    consumers: Arc<AtomicU32>,
    last_surface_id: Arc<AtomicU32>,
    resolution: Arc<RwLock<(u32, u32, u32)>>,
    #[cfg(target_os = "windows")]
    native: Arc<Mutex<NativeVcamHandle>>,
}

impl Default for WindowsVirtualCamSink {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsVirtualCamSink {
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            consumers: Arc::new(AtomicU32::new(0)),
            last_surface_id: Arc::new(AtomicU32::new(0)),
            resolution: Arc::new(RwLock::new((1920, 1080, 60))),
            #[cfg(target_os = "windows")]
            native: Arc::new(Mutex::new(NativeVcamHandle {
                ptr: unsafe { ffi::ts_win_vcam_create() },
            })),
        }
    }

    pub fn start(&self, width: u32, height: u32, fps: u32) {
        *self.resolution.write() = (width, height, fps);
        self.active.store(true, Ordering::Relaxed);
        #[cfg(target_os = "windows")]
        {
            let handle = self.native.lock();
            if !handle.ptr.is_null() {
                let _ = unsafe { ffi::ts_win_vcam_start(handle.ptr) };
            }
        }
    }

    pub fn stop(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.last_surface_id.store(0, Ordering::Relaxed);
        #[cfg(target_os = "windows")]
        {
            let handle = self.native.lock();
            if !handle.ptr.is_null() {
                unsafe { ffi::ts_win_vcam_stop(handle.ptr) };
            }
        }
    }

    pub fn set_consumer_count(&self, count: u32) {
        self.consumers.store(count, Ordering::Relaxed);
    }

    pub fn consumer_count(&self) -> u32 {
        self.consumers.load(Ordering::Relaxed)
    }

    pub fn publish_surface(&self, surface_id: u32, _width: u32, _height: u32, _sequence: u64) {
        self.last_surface_id.store(surface_id, Ordering::Relaxed);
    }

    pub fn last_iosurface_id(&self) -> u32 {
        self.last_surface_id.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> VirtualCamStatus {
        let (output_width, output_height, fps_cap) = *self.resolution.read();
        VirtualCamStatus {
            active: self.active.load(Ordering::Relaxed),
            device_name: String::from("TruongStream Virtual Camera"),
            backend: String::from("Media Foundation (MFCreateVirtualCamera)"),
            consumer_count: self.consumer_count(),
            output_width,
            output_height,
            fps_cap,
        }
    }

    pub fn activate_camera_extension(&self) -> String {
        let wpd_count = probe_wpd_camera_count();
        format!(
            "Windows Media Foundation virtual camera uses MFCreateVirtualCamera (WPD/PTP camera interfaces detected: {wpd_count}). For OBS / browser capture on any Windows 10/11 system, you can also use http://127.0.0.1:49210/stream.mjpeg or Clean Output Mode."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_native_sources_are_embedded() {
        assert!(HLSL_LUT_SHADER_SOURCE.contains("CSMain"));
        assert!(WPD_TRANSPORT_SOURCE.contains("IPortableDevice"));
        assert!(MF_HDMI_CAPTURE_SOURCE.contains("MFCreateSourceReaderFromMediaSource"));
        assert!(MF_VIRTUAL_CAM_SOURCE.contains("MFCreateVirtualCamera"));
    }

    #[test]
    fn grades_uncompressed_bgra_frame_to_1080p_and_4k() {
        let pipeline = WindowsGpuPipeline::new();
        pipeline.set_lut(BuiltinLutPreset::TealOrange.build_lut());
        pipeline.set_settings(LutRenderSettings {
            output_width: 1920,
            output_height: 1080,
            intensity: 1.0,
            ..Default::default()
        });
        let raw_frame = LiveViewFrame {
            jpeg_bytes: Vec::new(),
            raw_pixels: vec![140u8; 64 * 36 * 4],
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
            intensity: 0.85,
            ..Default::default()
        });
        let out_4k = pipeline.process_live_frame(&raw_frame, false).unwrap();
        assert_eq!((out_4k.width, out_4k.height), (3840, 2160));
        assert!(out_4k.preview_jpeg.starts_with(&[0xFF, 0xD8]));
    }

    #[test]
    fn virtual_cam_reports_media_foundation_status() {
        let sink = WindowsVirtualCamSink::new();
        sink.start(3840, 2160, 30);
        let status = sink.status();
        assert!(status.active);
        assert_eq!(status.output_width, 3840);
        assert_eq!(status.output_height, 2160);
        assert_eq!(status.fps_cap, 30);
    }
}
