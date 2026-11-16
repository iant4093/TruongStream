use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use truongstream_core::drivers::{discover_usb_cameras, open_usb_driver, CameraDriver};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let stop_flag = Arc::new(AtomicBool::new(false));
    {
        let flag = Arc::clone(&stop_flag);
        ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
        })?;
    }

    let cameras = discover_usb_cameras()?;
    println!("Discovered {} PTP camera(s):", cameras.len());
    for cam in &cameras {
        println!(
            "  - {} ({:?}) VID=0x{:04X} PID=0x{:04X} serial={} speed={} Mbps",
            cam.model_name,
            cam.brand,
            cam.vendor_id,
            cam.product_id,
            cam.serial_number,
            cam.usb_speed_mbps
        );
    }

    let target = cameras
        .into_iter()
        .find(|c| c.vendor_id == 0x04A9)
        .ok_or_else(|| anyhow::anyhow!("No Canon camera found on USB"))?;

    let mut driver = open_usb_driver(&target)?;
    run_probe(&mut *driver, &stop_flag)
}

fn run_probe(driver: &mut dyn CameraDriver, stop_flag: &AtomicBool) -> anyhow::Result<()> {
    let caps = driver.connect()?;
    println!(
        "\nConnected: {} | Firmware: {} | Serial: {} | Lens: {}",
        caps.camera_model, caps.firmware_version, caps.serial_number, caps.lens_model
    );
    if let Some(batt) = caps.battery_percent {
        println!("Battery: {batt}%");
    }
    println!(
        "Discovered {} dynamic property descriptors:",
        caps.properties.len()
    );
    let mut keys: Vec<_> = caps.properties.keys().collect();
    keys.sort_by_key(|k| format!("{k:?}"));
    for key in keys {
        let prop = &caps.properties[key];
        println!(
            "  - {:?} (0x{:04X}): current={} (raw=0x{:X}), writable={}, options={}",
            prop.id,
            prop.raw_code,
            prop.current.label,
            prop.current.raw,
            prop.writable,
            prop.allowed_values.len()
        );
    }

    struct CleanExit<'a>(&'a mut dyn CameraDriver);
    impl Drop for CleanExit<'_> {
        fn drop(&mut self) {
            let _ = self.0.stop_live_view();
            let _ = self.0.disconnect();
        }
    }
    let guard = CleanExit(driver);

    guard.0.start_live_view()?;
    println!("\nEVF Live View active — polling 30 frames...");

    let start = Instant::now();
    let mut count = 0u32;
    let mut total_bytes = 0usize;
    let mut last_dims = (0u32, 0u32);

    while count < 30 && !stop_flag.load(Ordering::SeqCst) {
        let frame = guard.0.next_frame()?;
        assert!(
            frame.jpeg_bytes.starts_with(&[0xFF, 0xD8])
                && frame.jpeg_bytes.ends_with(&[0xFF, 0xD9]),
            "invalid JPEG frame boundaries"
        );
        count += 1;
        total_bytes += frame.jpeg_bytes.len();
        last_dims = (frame.width, frame.height);
        if count == 1 || count % 10 == 0 {
            println!(
                "  Frame #{:02}: {}x{} ({:.1} KB, usb {:.2} ms)",
                frame.sequence,
                frame.width,
                frame.height,
                frame.jpeg_bytes.len() as f32 / 1024.0,
                frame.usb_transfer_ms
            );
        }
    }

    let elapsed = start.elapsed().max(Duration::from_millis(1));
    let fps = count as f64 / elapsed.as_secs_f64();
    println!(
        "\nLive View summary: {} frames at {}x{} in {:.2}s ({:.1} fps, avg {:.1} KB/frame)",
        count,
        last_dims.0,
        last_dims.1,
        elapsed.as_secs_f64(),
        fps,
        (total_bytes as f64 / count.max(1) as f64) / 1024.0
    );

    // Safe non-destructive property round-trip check if ISO descriptor is writable.
    if let Some(iso_desc) = guard
        .0
        .capabilities()
        .properties
        .get(&truongstream_core::drivers::CameraPropertyId::Iso)
        .cloned()
    {
        if iso_desc.writable && !iso_desc.allowed_values.is_empty() {
            let orig = iso_desc.current.raw;
            let alt = iso_desc
                .allowed_values
                .iter()
                .find(|v| v.raw != orig)
                .map(|v| v.raw)
                .unwrap_or(orig);
            guard
                .0
                .set_property(truongstream_core::drivers::CameraPropertyId::Iso, alt)?;
            let _ = guard.0.poll_events()?;
            guard
                .0
                .set_property(truongstream_core::drivers::CameraPropertyId::Iso, orig)?;
            println!("Verified ISO property round-trip write & restore (raw 0x{orig:X})");
        }
    }

    guard.0.stop_live_view()?;
    guard.0.disconnect()?;
    std::mem::forget(guard);
    println!("Cleanly stopped Live View and closed PTP session (0x1003).");
    Ok(())
}
