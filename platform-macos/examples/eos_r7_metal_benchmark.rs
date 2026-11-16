use std::sync::Arc;
use std::time::{Duration, Instant};

use truongstream_core::drivers::{discover_usb_cameras, open_usb_driver, CameraSessionWorker};
use truongstream_core::lut::{BuiltinLutPreset, LutRenderSettings};
use truongstream_core::ring_buffer::MailboxRingBuffer;
use truongstream_macos::{MacOsGpuPipeline, MacOsVirtualCamSink};

fn main() -> anyhow::Result<()> {
    let cameras = discover_usb_cameras()?;
    let target = cameras
        .into_iter()
        .find(|c| c.vendor_id == 0x04A9)
        .ok_or_else(|| anyhow::anyhow!("No Canon camera found on USB"))?;

    let ring = Arc::new(MailboxRingBuffer::new(2));
    let driver = open_usb_driver(&target)?;
    let mut worker = CameraSessionWorker::start(driver, Arc::clone(&ring))?;
    let caps = worker.capabilities();

    println!(
        "Connected: {} | Lens: {} | Serial: {}",
        caps.camera_model, caps.lens_model, caps.serial_number
    );

    let gpu = MacOsGpuPipeline::new();
    gpu.set_lut(BuiltinLutPreset::TealOrange.build_lut());
    gpu.set_settings(LutRenderSettings {
        intensity: 0.85,
        mirror_horizontal: false,
        flip_vertical: false,
        crop_zoom: 1.0,
        ..Default::default()
    });
    println!(
        "Apple Silicon Metal GPU acceleration: {}",
        gpu.is_gpu_accelerated()
    );

    let sink = MacOsVirtualCamSink::new();
    sink.start(960, 640, 60);

    worker.start_live_view()?;

    let mut count = 0u32;
    let mut sum_usb_ms = 0.0f64;
    let mut sum_gpu_ms = 0.0f64;
    let mut sum_total_ms = 0.0f64;
    let bench_start = Instant::now();
    let deadline = bench_start + Duration::from_secs(12);

    while count < 150 && Instant::now() < deadline {
        if let Some(frame) = ring.pop_latest() {
            // Exercise the full UI preview path (Metal 3D LUT + NV12 IOSurface + JPEG preview).
            let processed = gpu.process_frame(&frame.jpeg_bytes, false)?;
            assert!(
                processed.preview_jpeg.starts_with(&[0xFF, 0xD8]),
                "expected valid JPEG preview"
            );
            sink.publish_surface(
                processed.iosurface_id,
                processed.width,
                processed.height,
                frame.sequence,
            );
            let total_ms = frame.usb_transfer_ms + processed.pipeline_ms;
            count += 1;
            sum_usb_ms += f64::from(frame.usb_transfer_ms);
            sum_gpu_ms += f64::from(processed.pipeline_ms);
            sum_total_ms += f64::from(total_ms);

            if count == 1 || count % 50 == 0 {
                println!(
                    "  Frame #{:03}: {}x{} | IOSurface #{} | preview={:.1}KB | usb={:.2}ms | gpu={:.2}ms | total={:.2}ms",
                    count,
                    processed.width,
                    processed.height,
                    processed.iosurface_id,
                    processed.preview_jpeg.len() as f32 / 1024.0,
                    frame.usb_transfer_ms,
                    processed.pipeline_ms,
                    total_ms
                );
            }
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    let elapsed = bench_start.elapsed().as_secs_f64().max(0.001);
    let stats = ring.stats();
    println!(
        "\nEnd-to-End Summary ({} frames in {:.2}s = {:.1} fps):",
        count,
        elapsed,
        f64::from(count) / elapsed
    );
    if count > 0 {
        println!(
            "  Avg USB Bulk: {:.2} ms | Avg Metal 3D LUT + Preview: {:.2} ms | Avg Total: {:.2} ms",
            sum_usb_ms / f64::from(count),
            sum_gpu_ms / f64::from(count),
            sum_total_ms / f64::from(count),
        );
    }
    println!(
        "  Mailbox Ring: pushed={} popped={} dropped={} | VirtualCam={}",
        stats.pushed,
        stats.popped,
        stats.dropped,
        sink.status().backend
    );

    sink.stop();
    worker.shutdown();
    println!("Disconnected cleanly. Testing immediate reconnect...");

    let ring2 = Arc::new(MailboxRingBuffer::new(2));
    let driver2 = open_usb_driver(&target)?;
    let mut worker2 = CameraSessionWorker::start(driver2, Arc::clone(&ring2))?;
    worker2.start_live_view()?;
    let reconnect_deadline = Instant::now() + Duration::from_secs(4);
    let mut reconnected_frames = 0u32;
    while reconnected_frames < 10 && Instant::now() < reconnect_deadline {
        if let Some(frame) = ring2.pop_latest() {
            let processed = gpu.process_frame(&frame.jpeg_bytes, false)?;
            assert!(!processed.preview_jpeg.is_empty());
            reconnected_frames += 1;
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    worker2.shutdown();
    assert!(
        reconnected_frames >= 10,
        "expected >= 10 frames after immediate reconnect, got {reconnected_frames}"
    );
    println!("Immediate reconnect verified ({reconnected_frames} frames streamed cleanly).");
    Ok(())
}
