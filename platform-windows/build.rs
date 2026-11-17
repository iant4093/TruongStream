use std::env;

fn main() {
    println!("cargo:rerun-if-changed=Transport/WpdPtpTransport.cpp");
    println!("cargo:rerun-if-changed=VideoPipeline/D3D11VideoPipeline.cpp");
    println!("cargo:rerun-if-changed=VideoPipeline/MfHdmiCapture.cpp");
    println!("cargo:rerun-if-changed=VideoPipeline/LutPipeline.hlsl");
    println!("cargo:rerun-if-changed=VirtualCamSink/MfVirtualCamSink.cpp");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .define("NOMINMAX", None)
        .file("Transport/WpdPtpTransport.cpp")
        .file("VideoPipeline/D3D11VideoPipeline.cpp")
        .file("VideoPipeline/MfHdmiCapture.cpp")
        .file("VirtualCamSink/MfVirtualCamSink.cpp")
        .warnings(false)
        .compile("truongstream_windows_native");

    for lib in [
        "d3d11",
        "dxgi",
        "d3dcompiler",
        "mf",
        "mfplat",
        "mfreadwrite",
        "mfuuid",
        "ole32",
        "oleaut32",
        "uuid",
        "propsys",
        "portabledeviceguids",
        "user32",
    ] {
        println!("cargo:rustc-link-lib={lib}");
    }
}
