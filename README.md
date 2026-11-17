<div align="center">

<img src="app-ui/public/app-icon.png" width="96" alt="TruongStream app icon">

# TruongStream

A zero-configuration, low-latency cross-platform desktop utility built in Rust, Metal, and Direct3D 11 that turns interchangeable-lens cameras into native macOS and Windows webcams over standard USB—with full hardware camera controls and real-time GPU `.cube` 3D LUT color grading.

[Download for macOS](https://github.com/iant4093/TruongStream/releases/latest/download/TruongStream-macOS.dmg) · [Download for Windows](https://github.com/iant4093/TruongStream/releases/latest/download/TruongStream-Windows-Setup.exe) · [Releases](https://github.com/iant4093/TruongStream/releases) · [Build status](https://github.com/iant4093/TruongStream/actions) · [Report an issue](https://github.com/iant4093/TruongStream/issues)

[![Download for macOS](https://img.shields.io/badge/Download_for_macOS-0A84FF?style=for-the-badge&logo=apple&logoColor=white)](https://github.com/iant4093/TruongStream/releases/latest/download/TruongStream-macOS.dmg)
[![Download for Windows](https://img.shields.io/badge/Download_for_Windows-0A84FF?style=for-the-badge&logo=windows&logoColor=white)](https://github.com/iant4093/TruongStream/releases/latest/download/TruongStream-Windows-Setup.exe)

![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)
![React](https://img.shields.io/badge/React-19-61DAFB?logo=react&logoColor=black)
![Rust](https://img.shields.io/badge/Rust-1.82%2B-000000?logo=rust&logoColor=white)
![GPU](https://img.shields.io/badge/pipeline-Metal_%7C_D3D11-6E3FA3)
![Platforms](https://img.shields.io/badge/platforms-macOS_(Apple_Silicon)_%7C_Windows_x64-555555)
[![CI](https://github.com/iant4093/TruongStream/actions/workflows/ci.yml/badge.svg)](https://github.com/iant4093/TruongStream/actions/workflows/ci.yml)

</div>

---

TruongStream streams live video from Canon, Sony, Nikon, Fujifilm cameras over USB **or uncompressed 1080p / 4K HDMI capture cards** straight into Zoom, OBS, Chrome, and FaceTime—with full hardware dial controls, autofocus, and real-time GPU `.cube` 3D LUT color grading inside a neutral dark studio interface matching [TruongPaper](https://github.com/iant4093/TruongPaper).

## Architecture

```text
[Camera USB PTP (Canon / Sony / Nikon / Fujifilm) and/or Uncompressed 1080p/4K HDMI Capture]
       │
       ▼  (Async Bulk PTP / AVFoundation macOS / Media Foundation Windows)
┌──────────────────────────────────────────────────────────┐
│ 1. Core Engine (Rust — core/)                            │
│    - Device Discovery & Exclusive USB Claiming           │
│    - Brand Adapters: Canon | Sony | Nikon | Fujifilm     │
│    - HDMI / UVC Input (BGRA8/NV12/YUY2 + Hybrid Mode)    │
│    - Half-Duplex Command Queue & Event Poller            │
└──────────────────────────┬───────────────────────────────┘
                           │ (Drop-Oldest Mailbox Ring Buffer)
                           ▼
┌──────────────────────────────────────────────────────────┐
│ 2. Native GPU Media Pipeline (platform-macos / windows)  │
│    - macOS: VideoToolbox/AVFoundation -> Metal -> IOSurf │
│    - Windows: Media Foundation -> D3D11 HLSL Compute     │
│    - 3D .cube LUT Texture Sampling + NV12 Conversion     │
└──────────────────────────┬───────────────────────────────┘
                           │ (Zero-Copy Shared GPU Surface)
                           ▼
┌──────────────────────────────────────────────────────────┐
│ 3. OS Virtual Camera Sink & Local Loopback               │
│    - macOS: CoreMediaIO (CMIOExtension) + MJPEG Loopback │
│    - Windows: Media Foundation (MFCreateVirtualCamera)   │
└──────────────────────────┬───────────────────────────────┘
                           ▲
                           │ (Local IPC: Tauri Commands & State)
┌──────────────────────────┴───────────────────────────────┐
│ 4. Desktop Control UI (Tauri + React + TypeScript)       │
│    - Live Preview Viewport, Exposure Controls, 3D LUTs   │
└──────────────────────────────────────────────────────────┘
```

## Supported cameras

Every brand implements the unified `CameraDriver` trait (`connect`, `start_live_view`, `stop_live_view`, `next_frame`, `set_property`, `poll_events`, `trigger_autofocus`) with dynamic property descriptor parsing so dropdowns always match the attached camera body and lens.

| Source / Brand | Interface & Protocol | Key Models | Status |
|---|---|---|---|
| **Canon** | USB `0x04A9` EOS PTP (`0x9115`, `0x9114`, `0x9116`, `0x9110`, `0x9153`, `0x9154`, `0x9160`) | EOS R7, R5, R6 II, R8, R10, 5D/6D/90D | **Live Verified (EOS R7 `0x32F7`)** |
| **Sony** | USB `0x054C` PC Remote SDIO (`0x9201`–`0x9209`, `0x1009`) | a7 IV, a7S III, a7R V, a7C II, a6700, ZV-E1, FX3/FX30 | Protocol Implemented |
| **Nikon** | USB `0x04B0` Nikon PTP (`0x9201`–`0x9205`, `0x90C1`, `0x90C7`) | Z9, Z8, Z6 III, Z6 II, Z7 II, Zf, Zfc, D850 | Protocol Implemented |
| **Fujifilm** | USB `0x04CB` Fuji Tether (`0x1009`, `0x1015`, `0x1016`, `0xD001`) | X-T5, X-H2S, X-H2, X-S20, X-T4, GFX100 II | Protocol Implemented |
| **HDMI Capture** | Native `AVFoundation` (macOS) / `Media Foundation` `IMFSourceReader` (Windows) | Cam Link 4K, HD60 X, Magewell, Blackmagic, UVC HDMI | **Uncompressed 1080p / 4K + Hybrid PTP** |

## Features

**USB PTP Engine, Uncompressed HDMI Capture & Hardware Control**

- Single dedicated half-duplex USB worker thread per camera: strictly serializes `next_frame()`, `poll_events()`, and queued `set_property()` commands so UI interactions never collide on PTP bulk pipes
- **Uncompressed 1080p / 4K HDMI capture**: Enumerates external HDMI / UVC capture devices natively via `AVFoundation` on macOS and `Media Foundation` (`MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING`) on Windows, feeding raw `BGRA8`/`YUY2`/`NV12` frames directly into Metal / Direct3D 11 3D LUT compute shaders without JPEG roundtrips
- **Hybrid USB PTP + HDMI mode**: Keep a camera connected via USB PTP for aperture, shutter speed, ISO, white balance, and tap-to-focus while routing uncompressed `1080p` or `4K UHD` HDMI video under **System › Video Input Source**
- **USB vs. HDMI resolution**: Over USB PTP Live View (`GetViewFinderData`), cameras output camera-scaled EVF JPEGs (`960×640` on EOS R7); GPU upscaling uses 5-tap Contrast-Adaptive Sharpening (`LutPipeline.metal` / `LutPipeline.hlsl`), while HDMI capture delivers native uncompressed `1920×1080` and `3840×2160`
- Dynamic capability discovery: parses camera availability tables (`0xC18A` on Canon, `SDIO_GetAllExtDevicePropInfo` on Sony, `GetDevicePropDesc` on Nikon/Fujifilm) on connect—never hardcoding shutter speeds, apertures, or ISOs
- Two-way physical dial sync: turning an ISO, shutter, or aperture dial on the camera body updates the Studio UI in real time
- OS daemon & driver conflict handling: detects competing `ptpcamerad` / `mscamerad-xpc` locks on macOS and uses native WPD (`IPortableDevice`) pass-through on Windows so users never need Zadig/WinUSB driver replacement
- Clean session lifecycle: guarantees `stop_live_view()` and `CloseSession (0x1003)` on exit or `Ctrl+C` so cameras never stay locked in PC Remote mode
- **AF & Exposure ergonomics**: Manual (`M`) mode check with dial lock banner, `Auto (180° Rule)` shutter tracking (`1/60` at 30 FPS, `1/125` at 60 FPS), real USB + UI frame-rate pacing (`30`/`60` FPS), click-to-focus coordinate mapping with `1-Point AF`, and one-click unlock back to `Whole Area AF`.
- **Zero-Copy GPU Pipeline & 10 Built-In 3D LUTs**: Drop-oldest lock-free SPSC mailbox ring buffer (`ring_buffer.rs`), `.cube` parser (`17³`, `33³`, `65³`), and 10 bundled GPU LUT presets (`Neutral (Bypass)`, `Rec.709 Crisp`, `Cinematic Teal & Orange`, `Warm Kodak 2383`, `Fujifilm Eterna Cinema`, `Classic Chrome Documentary`, `S-Cinetone Natural Skin`, `Golden Hour Amber`, `Silver Bleach Bypass`, `Monochrome Acros`) rendered in a single pass (`LutPipeline.metal` / `LutPipeline.hlsl`) to `IOSurface` / `DXGI` NV12 textures.

**OS Virtual Camera Sinks, Local MJPEG Stream & Studio UI**

- **macOS (`CMIOExtension`) & Local MJPEG Loopback**: Exposes `"TruongStream Virtual Camera"` (`CoreMediaIO` for Apple Developer ID notarized builds) alongside an always-on local loopback MJPEG stream (`http://127.0.0.1:49210/stream.mjpeg`) and borderless **Clean Output Mode** (`Esc` to exit) for zero-setup ingestion into OBS, browsers, and ad-hoc local builds.
- **Windows (`MFCreateVirtualCamera`)**: Media Foundation software camera source consuming shared `DXGI` NV12 textures, with DirectShow fallback support.
- **Studio UI & Auto-Updater**: Neutral dark theme matching `TruongPaper`, macOS native fullscreen and hover traffic-light icons, custom right-click studio context menu, before/after LUT split-screen wipe, auto-sleep sensor protection, and signed GitHub Releases auto-updates under **System › Updates**.

## Install

Download installers from [GitHub Releases](https://github.com/iant4093/TruongStream/releases/latest).

**macOS** (Apple Silicon, macOS 14.2 or later)

1. Download [TruongStream-macOS.dmg](https://github.com/iant4093/TruongStream/releases/latest/download/TruongStream-macOS.dmg) and drag TruongStream into Applications.
2. Connect your camera via USB and set its mode dial to **M** (Manual) for full exposure control.
3. For direct virtual camera ingestion without a Developer ID notarized system extension, use the built-in **Local MJPEG Stream** (`http://127.0.0.1:49210/stream.mjpeg`) or **Clean Output Mode** (`Esc` to exit) in **System › Virtual Camera Output**. If a notarized Camera Extension is present, click **Activate macOS Camera Extension** and approve under **System Settings › Privacy & Security › Camera Extensions**.

**Windows** (x64, Windows 10 1809 or later / Windows 11)

1. Download [TruongStream-Windows-Setup.exe](https://github.com/iant4093/TruongStream/releases/latest/download/TruongStream-Windows-Setup.exe) and run the installer.
2. Connect your camera via USB in standard MTP/PTP mode—no Zadig driver replacement required.

## Tech stack

| Area | Technologies |
|---|---|
| App shell | Tauri 2 (Rust), WKWebView on macOS, WebView2 on Windows |
| Interface | React 19, TypeScript, Vite, Zustand, lucide-react, `TruongPaper` neutral dark design system |
| Core PTP & HDMI Engine | Rust (`rusb` / `libusb-1.0`, WPD `IPortableDevice`), `AVFoundation` & `Media Foundation` uncompressed HDMI capture, lock-free SPSC mailbox ring buffer |
| GPU Media Pipeline | macOS `VideoToolbox` + `Metal` Compute (`MTLTextureType3D` + `IOSurface`), Windows native C++ `D3D11` + `HLSL` DirectCompute (`cs_5_0`) + `DXGI` |
| Virtual Camera Sink | macOS `CoreMediaIO` (`CMIOExtension`), Windows Media Foundation (`MFCreateVirtualCamera`) |
| Delivery | GitHub Actions, macOS `.dmg`, Windows NSIS installer, signed auto-updates, SHA-256 checksums |

## Build locally

Requires **Node 22.12+**, **Rust 1.82+**, `libusb` (`brew install libusb` on macOS), and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```bash
cd app-ui
npm install

# Desktop app with hot reload
npm run app:dev

# UI-only preview in a browser with simulated camera & telemetry controls
npm run dev

# Production desktop installer build
npx tauri build
```

## Tests

```bash
# Rust formatting, strict clippy lints, and workspace unit tests
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Live Canon EOS R7 USB hardware proof-of-concept (connect EOS R7 over USB first)
cargo run -p truongstream-core --example eos_r7_probe

# End-to-end Canon EOS R7 + VideoToolbox + Metal 3D LUT + IOSurface benchmark (macOS)
cargo run -p truongstream-macos --example eos_r7_metal_benchmark

# Frontend unit/component tests (Vitest) and TypeScript/Vite build
cd app-ui
npm test
npm run build
```

## CI and releases

Pull requests and pushes to `main` run:

1. Vitest unit/component tests, TypeScript checks, and production frontend build (`app-ui/`).
2. `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and Tauri installer builds on macOS and Windows.

Manually running **CI** on `main` with **publish** enabled publishes a GitHub Release `v<major>.<minor>.<run number>` containing `TruongStream-macOS.dmg`, `TruongStream-Windows-Setup.exe`, `latest.json`, and `SHA256SUMS`.

## Repository layout

```text
.
├── core/                         Platform-agnostic Rust PTP & camera core
│   ├── examples/                 Live hardware probes (eos_r7_probe.rs)
│   └── src/
│       ├── ptp/                  Base PTP USB packet framing & macOS daemon guard
│       ├── drivers/              CameraDriver trait + canon.rs, sony.rs, nikon.rs, fujifilm.rs, hdmi.rs
│       ├── lut/                  .cube 3D LUT parser, presets & trilinear sampler
│       └── ring_buffer.rs        Lock-free SPSC drop-oldest mailbox ring buffer
├── platform-macos/               macOS native GPU & CoreMediaIO sink layer
│   ├── VideoPipeline/            VideoToolbox/AVFoundation HDMI + Metal 3D LUT NV12 compute shader
│   └── VirtualCamExtension/      CMIOExtension system extension (IOSurface sink)
├── platform-windows/             Windows native WPD, D3D11 & Media Foundation layer
│   ├── Transport/                WPD (IPortableDevice) raw PTP pass-through
│   ├── VideoPipeline/            D3D11 DirectCompute + HLSL 3D LUT shader + MfHdmiCapture.cpp
│   └── VirtualCamSink/           MFCreateVirtualCamera + DirectShow fallback
├── app-ui/                       Tauri + React + TypeScript Studio UI
│   ├── src/
│   │   ├── components/           TopBar, LiveViewport, RightInspector, Dropdown, ContextMenu, WindowControls
│   │   ├── services/             GitHub Releases auto-updater (updates.ts)
│   │   ├── styles/               Design tokens & components (tokens.css)
│   │   └── state/                Zustand synced camera, LUT & telemetry store
│   ├── tests/                    Vitest frontend & UI tests
│   └── src-tauri/                Tauri v2 commands & core session bridge
├── .github/                      CI/release workflow and Dependabot
└── README.md                     Sole repository documentation
```

TruongStream is an independent project and is not affiliated with or endorsed by Canon, Sony, Nikon, or Fujifilm.
