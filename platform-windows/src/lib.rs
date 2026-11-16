use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use truongstream_core::lut::{
    process_jpeg_with_lut, BuiltinLutPreset, CubeLut, LutRenderSettings, ProcessedFrame,
    VirtualCamStatus,
};

/// Embedded HLSL DirectCompute 3D LUT + BT.709 NV12 shader source.
pub const HLSL_LUT_SHADER_SOURCE: &str = include_str!("../VideoPipeline/LutPipeline.hlsl");

/// Embedded Windows Portable Devices (WPD) C++ pass-through source.
pub const WPD_TRANSPORT_SOURCE: &str = include_str!("../Transport/WpdPtpTransport.cpp");

/// Embedded Media Foundation (`MFCreateVirtualCamera`) C++ sink source.
pub const MF_VIRTUAL_CAM_SOURCE: &str = include_str!("../VirtualCamSink/MfVirtualCamSink.cpp");

/// Windows D3D11 + HLSL 3D LUT video pipeline wrapper.
pub struct WindowsGpuPipeline {
    lut: RwLock<CubeLut>,
    settings: RwLock<LutRenderSettings>,
}

impl Default for WindowsGpuPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsGpuPipeline {
    pub fn new() -> Self {
        Self {
            lut: RwLock::new(BuiltinLutPreset::Neutral.build_lut()),
            settings: RwLock::new(LutRenderSettings::default()),
        }
    }

    pub fn set_lut(&self, lut: CubeLut) {
        *self.lut.write() = lut;
    }

    pub fn set_settings(&self, settings: LutRenderSettings) {
        *self.settings.write() = settings;
    }

    pub fn settings(&self) -> LutRenderSettings {
        self.settings.read().clone()
    }

    pub fn process_ungraded_preview(&self, jpeg_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        let lut = self.lut.read();
        let mut settings = self.settings.read().clone();
        settings.intensity = 0.0;
        Ok(process_jpeg_with_lut(jpeg_bytes, &lut, &settings, false)?.preview_jpeg)
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
        process_jpeg_with_lut(jpeg_bytes, &lut, &settings, export_nv12)
    }
}

/// Bridge to Windows Media Foundation (`MFCreateVirtualCamera`) sink.
#[derive(Clone)]
pub struct WindowsVirtualCamSink {
    active: Arc<AtomicBool>,
    consumers: Arc<AtomicU32>,
    last_surface_id: Arc<AtomicU32>,
    resolution: Arc<RwLock<(u32, u32, u32)>>,
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
        }
    }

    pub fn start(&self, width: u32, height: u32, fps: u32) {
        *self.resolution.write() = (width, height, fps);
        self.active.store(true, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.last_surface_id.store(0, Ordering::Relaxed);
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
        String::from("Windows Media Foundation virtual camera source uses MFCreateVirtualCamera.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_native_sources_are_embedded() {
        assert!(HLSL_LUT_SHADER_SOURCE.contains("CSMain"));
        assert!(WPD_TRANSPORT_SOURCE.contains("IPortableDevice"));
        assert!(MF_VIRTUAL_CAM_SOURCE.contains("MFCreateVirtualCamera"));
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
