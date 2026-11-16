use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tauri::{Manager, RunEvent, State};
use tracing::info;
use truongstream_core::drivers::{
    discover_usb_cameras, open_usb_driver, CameraCapabilities, CameraPropertyId,
    CameraSessionWorker, DiscoveredCamera, LiveViewFrame,
};
use truongstream_core::lut::{
    BuiltinLutPreset, CubeLut, LutRenderSettings, LutSummary, VirtualCamStatus,
};
use truongstream_core::ring_buffer::MailboxRingBuffer;

#[cfg(target_os = "windows")]
use truongstream_windows::{
    WindowsGpuPipeline as PlatformGpuPipeline, WindowsVirtualCamSink as PlatformVirtualCamSink,
};

#[cfg(not(target_os = "windows"))]
use truongstream_macos::{
    MacOsGpuPipeline as PlatformGpuPipeline, MacOsVirtualCamSink as PlatformVirtualCamSink,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemConfig {
    pub resolution: String,
    pub fps_cap: u32,
    pub auto_sleep_sensor: bool,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            resolution: String::from("1080p"),
            fps_cap: 60,
            auto_sleep_sensor: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FramePayload {
    pub graded_data_url: String,
    pub ungraded_data_url: Option<String>,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub sequence: u64,
    pub latency_ms: f32,
    pub fps: f32,
    pub capabilities: CameraCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineTelemetry {
    pub connected: bool,
    pub streaming: bool,
    pub camera_id: Option<String>,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub fps: f32,
    pub latency_ms: f32,
    pub usb_ms: f32,
    pub gpu_ms: f32,
    pub ring_pushed: u64,
    pub ring_popped: u64,
    pub ring_dropped: u64,
    pub shared_surface_id: u32,
    pub stream_url: String,
    pub virtual_cam: VirtualCamStatus,
    pub system_config: SystemConfig,
    pub active_lut: LutSummary,
}

struct FpsMeter {
    last_frame_at: Option<Instant>,
    ema_fps: f32,
}

impl FpsMeter {
    fn new() -> Self {
        Self {
            last_frame_at: None,
            ema_fps: 0.0,
        }
    }

    fn tick(&mut self) -> f32 {
        let now = Instant::now();
        if let Some(prev) = self.last_frame_at {
            let dt = now.duration_since(prev).as_secs_f32().max(0.001);
            let inst = (1.0 / dt).clamp(1.0, 120.0);
            self.ema_fps = if self.ema_fps == 0.0 {
                inst
            } else {
                self.ema_fps * 0.85 + inst * 0.15
            };
        }
        self.last_frame_at = Some(now);
        self.ema_fps
    }

    fn reset(&mut self) {
        self.last_frame_at = None;
        self.ema_fps = 0.0;
    }
}

const MJPEG_STREAM_URL: &str = "http://127.0.0.1:49210/stream.mjpeg";

pub struct AppState {
    session: Mutex<Option<CameraSessionWorker>>,
    ring_buffer: Mutex<Option<Arc<MailboxRingBuffer<LiveViewFrame>>>>,
    camera_id: Mutex<Option<String>>,
    gpu_pipeline: PlatformGpuPipeline,
    virtual_cam: PlatformVirtualCamSink,
    system_config: RwLock<SystemConfig>,
    active_lut_summary: RwLock<LutSummary>,
    split_preview: AtomicBool,
    fps_meter: Mutex<FpsMeter>,
    last_dims: Mutex<(u32, u32)>,
    last_source_dims: Mutex<(u32, u32)>,
    last_usb_ms_bits: AtomicU32,
    last_gpu_ms_bits: AtomicU32,
    last_total_ms_bits: AtomicU32,
    last_seq: AtomicU64,
    latest_jpeg: Arc<RwLock<Vec<u8>>>,
    frame_tx: tokio::sync::watch::Sender<u64>,
    mjpeg_clients: Arc<AtomicU32>,
}

fn resolution_dimensions(res: &str) -> (u32, u32) {
    match res {
        "native" => (0, 0),
        "4k" => (3840, 2160),
        _ => (1920, 1080),
    }
}

impl Default for AppState {
    fn default() -> Self {
        let default_lut = BuiltinLutPreset::Neutral.build_lut();
        let summary = default_lut.summary();
        let cfg = SystemConfig::default();
        let (target_w, target_h) = resolution_dimensions(&cfg.resolution);
        let gpu_pipeline = PlatformGpuPipeline::new();
        gpu_pipeline.set_settings(LutRenderSettings {
            output_width: target_w,
            output_height: target_h,
            ..Default::default()
        });
        let (frame_tx, _) = tokio::sync::watch::channel(0u64);
        Self {
            session: Mutex::new(None),
            ring_buffer: Mutex::new(None),
            camera_id: Mutex::new(None),
            gpu_pipeline,
            virtual_cam: PlatformVirtualCamSink::new(),
            system_config: RwLock::new(cfg),
            active_lut_summary: RwLock::new(summary),
            split_preview: AtomicBool::new(false),
            fps_meter: Mutex::new(FpsMeter::new()),
            last_dims: Mutex::new((1920, 1080)),
            last_source_dims: Mutex::new((960, 640)),
            last_usb_ms_bits: AtomicU32::new(0),
            last_gpu_ms_bits: AtomicU32::new(0),
            last_total_ms_bits: AtomicU32::new(0),
            last_seq: AtomicU64::new(0),
            latest_jpeg: Arc::new(RwLock::new(Vec::new())),
            frame_tx,
            mjpeg_clients: Arc::new(AtomicU32::new(0)),
        }
    }
}

impl AppState {
    pub fn shutdown_camera(&self) {
        self.virtual_cam.stop();
        if let Some(mut worker) = self.session.lock().take() {
            worker.shutdown();
        }
        *self.ring_buffer.lock() = None;
        *self.camera_id.lock() = None;
        self.latest_jpeg.write().clear();
        self.fps_meter.lock().reset();
    }

    fn apply_resolution_to_sink(&self, cfg: &SystemConfig) {
        let (target_w, target_h) = resolution_dimensions(&cfg.resolution);
        let mut settings = self.gpu_pipeline.settings();
        settings.output_width = target_w;
        settings.output_height = target_h;
        self.gpu_pipeline.set_settings(settings);

        let (sink_w, sink_h) = if target_w > 0 && target_h > 0 {
            (target_w, target_h)
        } else {
            (960, 640)
        };
        *self.last_dims.lock() = (sink_w, sink_h);
        if let Some(worker) = self.session.lock().as_ref() {
            worker.set_fps_cap(cfg.fps_cap);
            self.virtual_cam.start(sink_w, sink_h, cfg.fps_cap);
        }
    }
}

#[tauri::command]
async fn list_cameras() -> Result<Vec<DiscoveredCamera>, String> {
    tokio::task::spawn_blocking(discover_usb_cameras)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn connect_camera(
    camera_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CameraCapabilities, String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        app.shutdown_camera();

        let cameras = discover_usb_cameras().map_err(|e| e.to_string())?;
        let target = cameras
            .into_iter()
            .find(|c| c.id == camera_id)
            .ok_or_else(|| format!("Camera '{camera_id}' is no longer connected"))?;

        let driver = open_usb_driver(&target).map_err(|e| e.to_string())?;
        let ring = Arc::new(MailboxRingBuffer::new(2));
        let worker =
            CameraSessionWorker::start(driver, Arc::clone(&ring)).map_err(|e| e.to_string())?;
        let caps = worker.capabilities();

        *app.session.lock() = Some(worker);
        *app.ring_buffer.lock() = Some(ring);
        *app.camera_id.lock() = Some(camera_id);

        let cfg = app.system_config.read().clone();
        app.apply_resolution_to_sink(&cfg);

        info!(model = %caps.camera_model, "connected camera via Tauri command");
        Ok(caps)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn disconnect_camera(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        app.shutdown_camera();
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn start_stream(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        {
            let guard = app.session.lock();
            let worker = guard
                .as_ref()
                .ok_or_else(|| "No camera connected".to_string())?;
            worker.start_live_view().map_err(|e| e.to_string())?;
        }
        let cfg = app.system_config.read().clone();
        app.apply_resolution_to_sink(&cfg);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn stop_stream(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let guard = app.session.lock();
        if let Some(worker) = guard.as_ref() {
            worker.stop_live_view().map_err(|e| e.to_string())?;
        }
        drop(guard);
        app.virtual_cam.stop();
        app.fps_meter.lock().reset();
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_latest_frame(state: State<'_, Arc<AppState>>) -> Result<Option<FramePayload>, String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let ring_opt = app.ring_buffer.lock().clone();
        let Some(ring) = ring_opt else {
            return Ok(None);
        };

        let Some(frame) = ring.pop_latest() else {
            return Ok(None);
        };

        let caps = {
            let guard = app.session.lock();
            let Some(worker) = guard.as_ref() else {
                return Ok(None);
            };
            worker.capabilities()
        };

        let need_nv12 = app.virtual_cam.consumer_count() > 0;
        let processed = app
            .gpu_pipeline
            .process_frame(&frame.jpeg_bytes, need_nv12)
            .map_err(|e| e.to_string())?;

        app.virtual_cam.publish_surface(
            processed.iosurface_id,
            processed.width,
            processed.height,
            frame.sequence,
        );

        let fps = app.fps_meter.lock().tick();
        let total_latency = frame.usb_transfer_ms + processed.pipeline_ms;

        *app.last_dims.lock() = (processed.width, processed.height);
        let (source_width, source_height) = if frame.width > 0 && frame.height > 0 {
            *app.last_source_dims.lock() = (frame.width, frame.height);
            (frame.width, frame.height)
        } else {
            *app.last_source_dims.lock()
        };
        app.last_usb_ms_bits
            .store(frame.usb_transfer_ms.to_bits(), Ordering::Relaxed);
        app.last_gpu_ms_bits
            .store(processed.pipeline_ms.to_bits(), Ordering::Relaxed);
        app.last_total_ms_bits
            .store(total_latency.to_bits(), Ordering::Relaxed);
        app.last_seq.store(frame.sequence, Ordering::Relaxed);

        let preview_bytes = if processed.preview_jpeg.is_empty() {
            &frame.jpeg_bytes
        } else {
            &processed.preview_jpeg
        };
        *app.latest_jpeg.write() = preview_bytes.to_vec();
        let _ = app.frame_tx.send(frame.sequence);
        let graded_data_url = format!("data:image/jpeg;base64,{}", B64.encode(preview_bytes));
        let ungraded_data_url = if app.split_preview.load(Ordering::Relaxed) {
            let ungraded_bytes = app
                .gpu_pipeline
                .process_ungraded_preview(&frame.jpeg_bytes)
                .unwrap_or_else(|_| frame.jpeg_bytes.clone());
            Some(format!(
                "data:image/jpeg;base64,{}",
                B64.encode(&ungraded_bytes)
            ))
        } else {
            None
        };

        Ok(Some(FramePayload {
            graded_data_url,
            ungraded_data_url,
            width: processed.width,
            height: processed.height,
            source_width,
            source_height,
            sequence: frame.sequence,
            latency_ms: total_latency,
            fps,
            capabilities: caps,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_camera_property(
    property: CameraPropertyId,
    raw_value: u32,
    state: State<'_, Arc<AppState>>,
) -> Result<CameraCapabilities, String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let guard = app.session.lock();
        let worker = guard
            .as_ref()
            .ok_or_else(|| "No camera connected".to_string())?;
        worker
            .set_property(property, raw_value)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn trigger_autofocus(
    x: Option<f32>,
    y: Option<f32>,
    state: State<'_, Arc<AppState>>,
) -> Result<CameraCapabilities, String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let guard = app.session.lock();
        let worker = guard
            .as_ref()
            .ok_or_else(|| "No camera connected".to_string())?;
        worker.trigger_autofocus(x, y).map_err(|e| e.to_string())?;
        Ok(worker.capabilities())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn drive_focus(step: i32, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let guard = app.session.lock();
        let worker = guard
            .as_ref()
            .ok_or_else(|| "No camera connected".to_string())?;
        worker.drive_manual_focus(step).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_lut_config(
    preset_or_cube: String,
    is_custom_cube: bool,
    intensity: f32,
    mirror: bool,
    flip: bool,
    crop: f32,
    state: State<'_, Arc<AppState>>,
) -> Result<LutSummary, String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        let lut = if is_custom_cube {
            CubeLut::parse(&preset_or_cube).map_err(|e| e.to_string())?
        } else {
            BuiltinLutPreset::from_id(&preset_or_cube)
                .unwrap_or(BuiltinLutPreset::Neutral)
                .build_lut()
        };

        let summary = lut.summary();
        let (output_width, output_height) =
            resolution_dimensions(&app.system_config.read().resolution);
        app.gpu_pipeline.set_lut(lut);
        app.gpu_pipeline.set_settings(LutRenderSettings {
            intensity: intensity.clamp(0.0, 1.0),
            mirror_horizontal: mirror,
            flip_vertical: flip,
            crop_zoom: crop.clamp(1.0, 2.0),
            output_width,
            output_height,
        });
        *app.active_lut_summary.write() = summary.clone();
        Ok(summary)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn set_split_preview(enabled: bool, state: State<'_, Arc<AppState>>) {
    state.split_preview.store(enabled, Ordering::Relaxed);
}

#[tauri::command]
fn set_system_config(
    resolution: String,
    fps_cap: u32,
    auto_sleep_sensor: bool,
    state: State<'_, Arc<AppState>>,
) -> SystemConfig {
    let next = SystemConfig {
        resolution,
        fps_cap: fps_cap.clamp(15, 60),
        auto_sleep_sensor,
    };
    let fps_changed = state.system_config.read().fps_cap != next.fps_cap;
    *state.system_config.write() = next.clone();
    if fps_changed {
        state.fps_meter.lock().reset();
    }
    state.apply_resolution_to_sink(&next);
    next
}

#[tauri::command]
fn get_telemetry(state: State<'_, Arc<AppState>>) -> PipelineTelemetry {
    let session_guard = state.session.lock();
    let connected = session_guard.is_some();
    let streaming = session_guard.as_ref().is_some_and(|w| w.is_streaming());
    drop(session_guard);

    let ring_stats = state
        .ring_buffer
        .lock()
        .as_ref()
        .map(|r| r.stats())
        .unwrap_or_default();

    let (width, height) = *state.last_dims.lock();
    let (source_width, source_height) = *state.last_source_dims.lock();
    PipelineTelemetry {
        connected,
        streaming,
        camera_id: state.camera_id.lock().clone(),
        width,
        height,
        source_width,
        source_height,
        fps: state.fps_meter.lock().ema_fps,
        latency_ms: f32::from_bits(state.last_total_ms_bits.load(Ordering::Relaxed)),
        usb_ms: f32::from_bits(state.last_usb_ms_bits.load(Ordering::Relaxed)),
        gpu_ms: f32::from_bits(state.last_gpu_ms_bits.load(Ordering::Relaxed)),
        ring_pushed: ring_stats.pushed,
        ring_popped: ring_stats.popped,
        ring_dropped: ring_stats.dropped,
        shared_surface_id: state.virtual_cam.last_iosurface_id(),
        stream_url: String::from(MJPEG_STREAM_URL),
        virtual_cam: state.virtual_cam.status(),
        system_config: state.system_config.read().clone(),
        active_lut: state.active_lut_summary.read().clone(),
    }
}

#[tauri::command]
fn activate_camera_extension(state: State<'_, Arc<AppState>>) -> String {
    state.virtual_cam.activate_camera_extension()
}

#[tauri::command]
async fn update_feed_status(app: tauri::AppHandle) -> String {
    let url = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("endpoints"))
        .and_then(|e| e.get(0))
        .and_then(|e| e.as_str())
        .unwrap_or_default()
        .to_owned();
    if url.is_empty() {
        return "no update feed is configured".into();
    }
    let client = match reqwest::Client::builder()
        .user_agent("tauri-updater")
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return e.to_string(),
    };
    match client.get(&url).send().await {
        Ok(res) if res.status().is_success() => "ok".into(),
        Ok(res) if res.status() == reqwest::StatusCode::NOT_FOUND => "feed_not_found".into(),
        Ok(res) => format!("GitHub answered HTTP {}", res.status()),
        Err(e) if e.is_timeout() => "GitHub didn’t answer within 15 seconds".into(),
        Err(e) if e.is_connect() => "couldn’t connect to GitHub (offline, VPN or firewall?)".into(),
        Err(e) => e.to_string(),
    }
}

#[tauri::command]
async fn runtime_shutdown_for_update(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let app = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        app.shutdown_camera();
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn spawn_mjpeg_server(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:49210").await else {
            return;
        };
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                continue;
            };
            let app = Arc::clone(&state);
            tauri::async_runtime::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut req_buf = [0u8; 1024];
                let n = socket.read(&mut req_buf).await.unwrap_or(0);
                let req = std::str::from_utf8(&req_buf[..n]).unwrap_or("");
                if req.starts_with("GET /frame.jpg") {
                    let jpg = app.latest_jpeg.read().clone();
                    if jpg.is_empty() {
                        let _ = socket
                            .write_all(
                                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
                            )
                            .await;
                        return;
                    }
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
                        jpg.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&jpg).await;
                    return;
                }

                struct ConsumerGuard {
                    count: Arc<AtomicU32>,
                    sink: PlatformVirtualCamSink,
                }
                impl Drop for ConsumerGuard {
                    fn drop(&mut self) {
                        let prev = self.count.fetch_sub(1, Ordering::Relaxed);
                        self.sink.set_consumer_count(prev.saturating_sub(1));
                    }
                }
                let prev = app.mjpeg_clients.fetch_add(1, Ordering::Relaxed);
                app.virtual_cam.set_consumer_count(prev + 1);
                let _guard = ConsumerGuard {
                    count: Arc::clone(&app.mjpeg_clients),
                    sink: app.virtual_cam.clone(),
                };

                let hdr = b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
                if socket.write_all(hdr).await.is_err() {
                    return;
                }

                let mut rx = app.frame_tx.subscribe();
                loop {
                    let jpg = app.latest_jpeg.read().clone();
                    if !jpg.is_empty() {
                        let part = format!(
                            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                            jpg.len()
                        );
                        if socket.write_all(part.as_bytes()).await.is_err()
                            || socket.write_all(&jpg).await.is_err()
                            || socket.write_all(b"\r\n").await.is_err()
                        {
                            break;
                        }
                    }
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            });
        }
    });
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let state = Arc::new(AppState::default());
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(Arc::clone(&state))
        .setup(|app| {
            let st = app.state::<Arc<AppState>>();
            spawn_mjpeg_server(Arc::clone(&st));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_cameras,
            connect_camera,
            disconnect_camera,
            start_stream,
            stop_stream,
            get_latest_frame,
            set_camera_property,
            trigger_autofocus,
            drive_focus,
            set_lut_config,
            set_split_preview,
            set_system_config,
            get_telemetry,
            activate_camera_extension,
            update_feed_status,
            runtime_shutdown_for_update,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build TruongStream Tauri app");

    app.run(|app_handle, event| {
        if matches!(event, RunEvent::Exit | RunEvent::ExitRequested { .. }) {
            let state = app_handle.state::<Arc<AppState>>();
            state.shutdown_camera();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_app_state_exposes_neutral_lut_and_1080p_config() {
        let state = AppState::default();
        assert_eq!(state.active_lut_summary.read().title, "Neutral (Bypass)");
        assert_eq!(state.system_config.read().resolution, "1080p");
        assert_eq!(state.system_config.read().fps_cap, 60);
    }
}
