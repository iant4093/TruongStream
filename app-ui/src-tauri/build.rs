const APP_COMMANDS: &[&str] = &[
    "list_cameras",
    "connect_camera",
    "disconnect_camera",
    "start_stream",
    "stop_stream",
    "get_latest_frame",
    "set_camera_property",
    "trigger_autofocus",
    "drive_focus",
    "set_lut_config",
    "set_split_preview",
    "set_system_config",
    "get_telemetry",
    "activate_camera_extension",
    "update_feed_status",
    "runtime_shutdown_for_update",
];

fn attributes() -> tauri_build::Attributes {
    tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(APP_COMMANDS))
}

fn main() {
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");

    if windows_msvc {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());

        let windows = tauri_build::WindowsAttributes::new_without_app_manifest();
        tauri_build::try_build(attributes().windows_attributes(windows))
            .expect("failed to run tauri-build");
    } else {
        tauri_build::try_build(attributes()).expect("failed to run tauri-build");
    }
}
