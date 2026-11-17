use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let swift_file = manifest_dir.join("VideoPipeline/VideoToolboxDecoder.swift");
    println!("cargo:rerun-if-changed={}", swift_file.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let lib_path = out_dir.join("libtruongstream_vt_metal.a");

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64",
        _ => "arm64",
    };
    let target_triple = format!("{arch}-apple-macosx14.2");

    let status = Command::new("xcrun")
        .args([
            "swiftc",
            "-O",
            "-target",
            &target_triple,
            "-emit-library",
            "-static",
            "-module-name",
            "TruongStreamVTMetal",
            "-o",
        ])
        .arg(&lib_path)
        .arg(&swift_file)
        .status()
        .expect("failed to invoke xcrun swiftc");

    assert!(
        status.success(),
        "xcrun swiftc failed compiling VideoToolboxDecoder.swift"
    );

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=truongstream_vt_metal");

    for fw in [
        "AVFoundation",
        "Metal",
        "VideoToolbox",
        "CoreMedia",
        "CoreVideo",
        "IOSurface",
        "CoreGraphics",
        "CoreImage",
        "ImageIO",
        "Foundation",
        "SystemExtensions",
    ] {
        println!("cargo:rustc-link-lib=framework={fw}");
    }

    if let Ok(output) = Command::new("xcrun")
        .args(["swiftc", "-print-target-info"])
        .output()
    {
        let info = String::from_utf8_lossy(&output.stdout);
        if let Some(start) = info.find("\"runtimeLibraryPaths\"") {
            if let Some(bracket_start) = info[start..].find('[') {
                if let Some(bracket_end) = info[start + bracket_start..].find(']') {
                    let array_slice =
                        &info[start + bracket_start + 1..start + bracket_start + bracket_end];
                    for line in array_slice.split(',') {
                        let cleaned = line
                            .trim()
                            .trim_matches(|c| c == '"' || c == ' ' || c == '\n');
                        if !cleaned.is_empty() {
                            println!("cargo:rustc-link-search=native={cleaned}");
                        }
                    }
                }
            }
        }
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
