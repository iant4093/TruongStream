use image::ImageFormat;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors returned while parsing or validating an Adobe `.cube` 3D LUT file.
#[derive(Debug, Error, PartialEq)]
pub enum LutError {
    #[error("1D LUT files (LUT_1D_SIZE) are not supported; expected a 3D .cube LUT")]
    Unsupported1dLut,
    #[error("missing required LUT_3D_SIZE directive")]
    MissingSize,
    #[error("invalid LUT_3D_SIZE {0}: expected integer between 2 and 129 (standard: 17, 33, 65)")]
    InvalidSize(usize),
    #[error("invalid {keyword} directive: {detail}")]
    InvalidDirective {
        keyword: &'static str,
        detail: String,
    },
    #[error("invalid RGB triple on line {line}: {detail}")]
    InvalidSample { line: usize, detail: String },
    #[error(
        "LUT entry count mismatch: expected {expected} RGB triples for {size}^3 grid, got {actual}"
    )]
    SampleCountMismatch {
        size: usize,
        expected: usize,
        actual: usize,
    },
}

/// Built-in color grading LUT presets bundled with TruongStream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinLutPreset {
    Neutral,
    Rec709Crisp,
    TealOrange,
    Kodak2383Warm,
    FujiEterna,
    ClassicChrome,
    SonySCinetone,
    GoldenHour,
    BleachBypass,
    MonochromeAcros,
}

impl BuiltinLutPreset {
    pub const ALL: [Self; 10] = [
        Self::Neutral,
        Self::Rec709Crisp,
        Self::TealOrange,
        Self::Kodak2383Warm,
        Self::FujiEterna,
        Self::ClassicChrome,
        Self::SonySCinetone,
        Self::GoldenHour,
        Self::BleachBypass,
        Self::MonochromeAcros,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Rec709Crisp => "rec709_crisp",
            Self::TealOrange => "teal_orange",
            Self::Kodak2383Warm => "kodak_2383_warm",
            Self::FujiEterna => "fuji_eterna",
            Self::ClassicChrome => "classic_chrome",
            Self::SonySCinetone => "sony_s_cinetone",
            Self::GoldenHour => "golden_hour",
            Self::BleachBypass => "bleach_bypass",
            Self::MonochromeAcros => "monochrome_acros",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Neutral => "Neutral (Bypass)",
            Self::Rec709Crisp => "Rec.709 Crisp",
            Self::TealOrange => "Cinematic Teal & Orange",
            Self::Kodak2383Warm => "Warm Kodak 2383",
            Self::FujiEterna => "Fujifilm Eterna Cinema",
            Self::ClassicChrome => "Classic Chrome Documentary",
            Self::SonySCinetone => "S-Cinetone Natural Skin",
            Self::GoldenHour => "Golden Hour Amber",
            Self::BleachBypass => "Silver Bleach Bypass",
            Self::MonochromeAcros => "Monochrome Acros",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    /// Generates a 33x33x33 3D LUT ready for GPU 3D texture upload.
    pub fn build_lut(self) -> CubeLut {
        CubeLut::from_fn(self.display_name(), 33, |r, g, b| {
            let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            match self {
                Self::Neutral => [r, g, b],
                Self::Rec709Crisp => [s_curve(r, 0.18), s_curve(g, 0.18), s_curve(b, 0.18)],
                Self::TealOrange => {
                    let shadow = (1.0 - lum).powf(1.6);
                    let highlight = lum.powf(1.4);
                    [
                        clamp01(s_curve(r, 0.2) + 0.09 * highlight - 0.06 * shadow),
                        clamp01(s_curve(g, 0.2) + 0.02 * highlight + 0.01 * shadow),
                        clamp01(s_curve(b, 0.2) - 0.08 * highlight + 0.09 * shadow),
                    ]
                }
                Self::Kodak2383Warm => {
                    let warm = smoothstep(0.15, 0.9, lum);
                    [
                        clamp01(s_curve(r, 0.24) + 0.06 * warm + 0.015),
                        clamp01(s_curve(g, 0.22) + 0.02 * warm + 0.005),
                        clamp01(s_curve(b, 0.22) - 0.06 * warm),
                    ]
                }
                Self::FujiEterna => {
                    // Soft highlight roll-off with desaturated cinema shadows and subtle cyan greens.
                    let lift_r = clamp01(lum + (r - lum) * 0.76);
                    let lift_g = clamp01(lum + (g - lum) * 0.78);
                    let lift_b = clamp01(lum + (b - lum) * 0.80);
                    let soft = |v: f32| clamp01(0.03 + v * 0.92 - 0.03 * smoothstep(0.75, 1.0, v));
                    [soft(lift_r), soft(lift_g), clamp01(soft(lift_b) + 0.015)]
                }
                Self::ClassicChrome => {
                    // Documentary style: firm midtone contrast, muted reds/blues, slate shadows.
                    let mut out = [
                        lum + (r - lum) * 0.72,
                        lum + (g - lum) * 0.78,
                        lum + (b - lum) * 0.70,
                    ];
                    let hi = smoothstep(0.55, 1.0, lum);
                    let sh = 1.0 - smoothstep(0.0, 0.45, lum);
                    out[0] = clamp01(s_curve(out[0], 0.22) + 0.03 * hi - 0.02 * sh);
                    out[1] = clamp01(s_curve(out[1], 0.20) + 0.01 * hi);
                    out[2] = clamp01(s_curve(out[2], 0.20) - 0.03 * hi + 0.025 * sh);
                    out
                }
                Self::SonySCinetone => {
                    // Pleasant natural skin tones with gentle highlight shoulder compression.
                    let shoulder = 1.0 - 0.06 * smoothstep(0.7, 1.0, lum);
                    let skin = (r - b).clamp(0.0, 1.0);
                    [
                        clamp01(s_curve(r, 0.14) * shoulder + 0.025 * skin),
                        clamp01(s_curve(g, 0.14) * shoulder + 0.01 * skin),
                        clamp01(s_curve(b, 0.14) * shoulder),
                    ]
                }
                Self::GoldenHour => {
                    let mid = smoothstep(0.05, 0.85, lum);
                    [
                        clamp01(s_curve(r, 0.16) + 0.08 * mid + 0.02),
                        clamp01(s_curve(g, 0.16) + 0.03 * mid + 0.01),
                        clamp01(s_curve(b, 0.14) - 0.09 * mid),
                    ]
                }
                Self::BleachBypass => {
                    // Partial silver-retention desaturation with bold darks & crisp whites.
                    let desat = |ch: f32| clamp01(lum * 0.58 + ch * 0.42);
                    [
                        s_curve(desat(r), 0.34),
                        s_curve(desat(g), 0.34),
                        s_curve(desat(b), 0.32),
                    ]
                }
                Self::MonochromeAcros => {
                    // Red-filtered film monochrome weighting with punchy contrast curve.
                    let bw = clamp01(0.28 * r + 0.62 * g + 0.10 * b);
                    let c = s_curve(bw, 0.28);
                    [c, c, c]
                }
            }
        })
    }
}

/// Summary sent over IPC to populate UI selectors without transferring the full 3D grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LutSummary {
    pub title: String,
    pub size: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub texel_count: usize,
}

/// Parsed 3D Look-Up Table stored in RGBA32F layout (`[r, g, b, 1.0]`) where R changes
/// fastest, then G, then B — matching Adobe `.cube`, Metal `MTLTextureType3D`, and
/// Direct3D 11 `ID3D11Texture3D` memory ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct CubeLut {
    pub title: String,
    pub size: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub data: Vec<[f32; 4]>,
}

impl CubeLut {
    /// Parses an Adobe `.cube` 3D LUT specification string.
    pub fn parse(input: &str) -> Result<Self, LutError> {
        let mut title = String::from("Custom LUT");
        let mut size: Option<usize> = None;
        let mut domain_min = [0.0f32, 0.0, 0.0];
        let mut domain_max = [1.0f32, 1.0, 1.0];
        let mut data: Vec<[f32; 4]> = Vec::new();

        for (idx, raw_line) in input.lines().enumerate() {
            let line_no = idx + 1;
            let without_comment = raw_line.split('#').next().unwrap_or("").trim();
            if without_comment.is_empty() {
                continue;
            }

            if let Some(rest) = strip_keyword(without_comment, "TITLE") {
                title = rest.trim().trim_matches('"').to_string();
                if title.is_empty() {
                    title = String::from("Custom LUT");
                }
                continue;
            }

            if strip_keyword(without_comment, "LUT_1D_SIZE").is_some() {
                return Err(LutError::Unsupported1dLut);
            }

            if let Some(rest) = strip_keyword(without_comment, "LUT_3D_SIZE") {
                let parsed: usize =
                    rest.trim()
                        .parse()
                        .map_err(|_| LutError::InvalidDirective {
                            keyword: "LUT_3D_SIZE",
                            detail: format!("invalid integer on line {line_no}: {rest}"),
                        })?;
                if !(2..=129).contains(&parsed) {
                    return Err(LutError::InvalidSize(parsed));
                }
                size = Some(parsed);
                data.reserve_exact(parsed * parsed * parsed);
                continue;
            }

            if let Some(rest) = strip_keyword(without_comment, "DOMAIN_MIN") {
                domain_min = parse_triple(rest).map_err(|detail| LutError::InvalidDirective {
                    keyword: "DOMAIN_MIN",
                    detail: format!("line {line_no}: {detail}"),
                })?;
                continue;
            }

            if let Some(rest) = strip_keyword(without_comment, "DOMAIN_MAX") {
                domain_max = parse_triple(rest).map_err(|detail| LutError::InvalidDirective {
                    keyword: "DOMAIN_MAX",
                    detail: format!("line {line_no}: {detail}"),
                })?;
                continue;
            }

            if strip_keyword(without_comment, "LUT_1D_INPUT_RANGE").is_some()
                || strip_keyword(without_comment, "LUT_3D_INPUT_RANGE").is_some()
            {
                let vals: Vec<f32> = without_comment
                    .split_whitespace()
                    .skip(1)
                    .map(|s| s.parse::<f32>())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| LutError::InvalidDirective {
                        keyword: "LUT_3D_INPUT_RANGE",
                        detail: format!("line {line_no}: {e}"),
                    })?;
                if vals.len() != 2 {
                    return Err(LutError::InvalidDirective {
                        keyword: "LUT_3D_INPUT_RANGE",
                        detail: format!("line {line_no}: expected 2 floats"),
                    });
                }
                domain_min = [vals[0]; 3];
                domain_max = [vals[1]; 3];
                continue;
            }

            let rgb = parse_triple(without_comment).map_err(|detail| LutError::InvalidSample {
                line: line_no,
                detail,
            })?;
            data.push([rgb[0], rgb[1], rgb[2], 1.0]);
        }

        let size = size.ok_or(LutError::MissingSize)?;
        for ch in 0..3 {
            if !domain_min[ch].is_finite()
                || !domain_max[ch].is_finite()
                || domain_max[ch] <= domain_min[ch]
            {
                return Err(LutError::InvalidDirective {
                    keyword: "DOMAIN_MAX",
                    detail: format!(
                        "domain_max[{ch}] ({}) must be > domain_min[{ch}] ({})",
                        domain_max[ch], domain_min[ch]
                    ),
                });
            }
        }

        let expected = size * size * size;
        if data.len() != expected {
            return Err(LutError::SampleCountMismatch {
                size,
                expected,
                actual: data.len(),
            });
        }

        Ok(Self {
            title,
            size,
            domain_min,
            domain_max,
            data,
        })
    }

    /// Generates a procedural 3D LUT by evaluating `eval(r, g, b)` across a `size^3` lattice.
    pub fn from_fn(
        title: impl Into<String>,
        size: usize,
        mut eval: impl FnMut(f32, f32, f32) -> [f32; 3],
    ) -> Self {
        assert!((2..=129).contains(&size), "LUT size must be 2..=129");
        let denom = (size - 1) as f32;
        let mut data = Vec::with_capacity(size * size * size);
        for bi in 0..size {
            let b = bi as f32 / denom;
            for gi in 0..size {
                let g = gi as f32 / denom;
                for ri in 0..size {
                    let r = ri as f32 / denom;
                    let [or, og, ob] = eval(r, g, b);
                    data.push([or, og, ob, 1.0]);
                }
            }
        }
        Self {
            title: title.into(),
            size,
            domain_min: [0.0, 0.0, 0.0],
            domain_max: [1.0, 1.0, 1.0],
            data,
        }
    }

    pub fn summary(&self) -> LutSummary {
        LutSummary {
            title: self.title.clone(),
            size: self.size,
            domain_min: self.domain_min,
            domain_max: self.domain_max,
            texel_count: self.data.len(),
        }
    }

    /// Returns true if `size` matches one of the standard 3D LUT grid resolutions (17, 33, 65).
    pub fn is_standard_size(&self) -> bool {
        matches!(self.size, 17 | 33 | 65)
    }

    /// CPU reference trilinear LUT interpolation + intensity blend (`0.0..=1.0`).
    pub fn sample_trilinear(&self, rgb: [f32; 3], intensity: f32) -> [f32; 3] {
        let blend = intensity.clamp(0.0, 1.0);
        if blend == 0.0 {
            return rgb;
        }
        let max_idx = (self.size - 1) as f32;

        let nx = ((rgb[0] - self.domain_min[0]) / (self.domain_max[0] - self.domain_min[0]))
            .clamp(0.0, 1.0)
            * max_idx;
        let ny = ((rgb[1] - self.domain_min[1]) / (self.domain_max[1] - self.domain_min[1]))
            .clamp(0.0, 1.0)
            * max_idx;
        let nz = ((rgb[2] - self.domain_min[2]) / (self.domain_max[2] - self.domain_min[2]))
            .clamp(0.0, 1.0)
            * max_idx;

        let x0 = (nx.floor() as usize).min(self.size - 1);
        let y0 = (ny.floor() as usize).min(self.size - 1);
        let z0 = (nz.floor() as usize).min(self.size - 1);
        let x1 = (x0 + 1).min(self.size - 1);
        let y1 = (y0 + 1).min(self.size - 1);
        let z1 = (z0 + 1).min(self.size - 1);

        let fx = nx - x0 as f32;
        let fy = ny - y0 as f32;
        let fz = nz - z0 as f32;

        let c000 = self.texel(x0, y0, z0);
        let c100 = self.texel(x1, y0, z0);
        let c010 = self.texel(x0, y1, z0);
        let c110 = self.texel(x1, y1, z0);
        let c001 = self.texel(x0, y0, z1);
        let c101 = self.texel(x1, y0, z1);
        let c011 = self.texel(x0, y1, z1);
        let c111 = self.texel(x1, y1, z1);

        let mut graded = [0.0f32; 3];
        for i in 0..3 {
            let c00 = c000[i] + (c100[i] - c000[i]) * fx;
            let c10 = c010[i] + (c110[i] - c010[i]) * fx;
            let c01 = c001[i] + (c101[i] - c001[i]) * fx;
            let c11 = c011[i] + (c111[i] - c011[i]) * fx;
            let c0 = c00 + (c10 - c00) * fy;
            let c1 = c01 + (c11 - c01) * fy;
            graded[i] = c0 + (c1 - c0) * fz;
        }

        [
            rgb[0] + (graded[0] - rgb[0]) * blend,
            rgb[1] + (graded[1] - rgb[1]) * blend,
            rgb[2] + (graded[2] - rgb[2]) * blend,
        ]
    }

    #[inline]
    fn texel(&self, r: usize, g: usize, b: usize) -> [f32; 4] {
        self.data[(b * self.size + g) * self.size + r]
    }
}

fn strip_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let mut parts = line.splitn(2, char::is_whitespace);
    let head = parts.next()?;
    if head.eq_ignore_ascii_case(keyword) {
        Some(parts.next().unwrap_or(""))
    } else {
        None
    }
}

fn parse_triple(line: &str) -> Result<[f32; 3], String> {
    let mut iter = line.split_whitespace();
    let a = iter.next().ok_or_else(|| "missing R value".to_string())?;
    let b = iter.next().ok_or_else(|| "missing G value".to_string())?;
    let c = iter.next().ok_or_else(|| "missing B value".to_string())?;
    if iter.next().is_some() {
        return Err(format!("expected 3 floats, found extra tokens in '{line}'"));
    }
    let rf: f32 = a.parse().map_err(|e| format!("bad float '{a}': {e}"))?;
    let gf: f32 = b.parse().map_err(|e| format!("bad float '{b}': {e}"))?;
    let bf: f32 = c.parse().map_err(|e| format!("bad float '{c}': {e}"))?;
    if !rf.is_finite() || !gf.is_finite() || !bf.is_finite() {
        return Err("non-finite LUT value".to_string());
    }
    Ok([rf, gf, bf])
}

#[inline]
fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp01((x - edge0) / (edge1 - edge0));
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn s_curve(x: f32, amount: f32) -> f32 {
    let x = clamp01(x);
    let s = x * x * (3.0 - 2.0 * x);
    clamp01(x + (s - x) * amount)
}

/// Real-time GPU/CPU color grading & framing settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LutRenderSettings {
    pub intensity: f32,
    pub mirror_horizontal: bool,
    pub flip_vertical: bool,
    pub crop_zoom: f32,
    /// Target output width in pixels (`0` preserves the camera frame's native width).
    pub output_width: u32,
    /// Target output height in pixels (`0` preserves the camera frame's native height).
    pub output_height: u32,
}

impl Default for LutRenderSettings {
    fn default() -> Self {
        Self {
            intensity: 1.0,
            mirror_horizontal: false,
            flip_vertical: false,
            crop_zoom: 1.0,
            output_width: 0,
            output_height: 0,
        }
    }
}

/// Virtual camera sink status shared across macOS CMIOExtension and Windows Media Foundation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualCamStatus {
    pub active: bool,
    pub device_name: String,
    pub backend: String,
    pub consumer_count: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub fps_cap: u32,
}

/// Graded output frame containing NV12 planes / shared GPU surface handle and preview JPEG bytes.
#[derive(Debug, Clone)]
pub struct ProcessedFrame {
    pub width: u32,
    pub height: u32,
    pub iosurface_id: u32,
    pub y_plane: Vec<u8>,
    pub uv_plane: Vec<u8>,
    pub preview_jpeg: Vec<u8>,
    pub pipeline_ms: f32,
}

/// Applies 3D LUT trilinear grading, mirror/flip/digital crop, and BT.709 RGB->NV12 conversion
/// to an incoming camera JPEG frame. Used by platform backends alongside native GPU shaders.
pub fn process_jpeg_with_lut(
    jpeg_bytes: &[u8],
    lut: &CubeLut,
    settings: &LutRenderSettings,
    need_nv12: bool,
) -> anyhow::Result<ProcessedFrame> {
    let start = std::time::Instant::now();

    let (src_hint_w, src_hint_h) = crate::drivers::parse_jpeg_dimensions(jpeg_bytes);
    let target_override = settings.output_width > 1 && settings.output_height > 1;
    let resolution_differs = target_override
        && src_hint_w > 0
        && src_hint_h > 0
        && (settings.output_width != src_hint_w || settings.output_height != src_hint_h);

    let needs_pixel_work = settings.intensity > 0.001
        || settings.mirror_horizontal
        || settings.flip_vertical
        || settings.crop_zoom > 1.001
        || resolution_differs
        || need_nv12;

    if !needs_pixel_work {
        return Ok(ProcessedFrame {
            width: src_hint_w,
            height: src_hint_h,
            iosurface_id: 0,
            y_plane: Vec::new(),
            uv_plane: Vec::new(),
            preview_jpeg: jpeg_bytes.to_vec(),
            pipeline_ms: start.elapsed().as_secs_f32() * 1000.0,
        });
    }

    let decoded = image::load_from_memory_with_format(jpeg_bytes, ImageFormat::Jpeg)?.to_rgb8();
    let src_width = decoded.width();
    let src_height = decoded.height();
    let src = decoded.as_raw();

    let width = if target_override {
        settings.output_width & !1
    } else {
        src_width
    };
    let height = if target_override {
        settings.output_height & !1
    } else {
        src_height
    };

    let src_aspect = src_width as f32 / src_height.max(1) as f32;
    let dst_aspect = width as f32 / height.max(1) as f32;
    let (aspect_x, aspect_y) = if dst_aspect > src_aspect {
        (1.0f32, src_aspect / dst_aspect)
    } else if src_aspect > dst_aspect {
        (dst_aspect / src_aspect, 1.0f32)
    } else {
        (1.0f32, 1.0f32)
    };

    let mut graded_rgb = vec![0u8; (width * height * 3) as usize];
    let zoom = settings.crop_zoom.max(1.0);
    let y_len = (width * height) as usize;
    let uv_len = ((width / 2) * (height / 2) * 2) as usize;
    let mut y_plane = if need_nv12 {
        vec![0u8; y_len]
    } else {
        Vec::new()
    };
    let mut uv_plane = if need_nv12 {
        vec![0u8; uv_len]
    } else {
        Vec::new()
    };

    for dy in 0..height {
        let mut v = (dy as f32 + 0.5) / height as f32;
        if settings.flip_vertical {
            v = 1.0 - v;
        }
        v = ((v - 0.5) * (aspect_y / zoom) + 0.5).clamp(0.0, 1.0);
        let sy = ((v * (src_height as f32 - 1.0)).round() as u32).min(src_height - 1);

        for dx in 0..width {
            let mut u = (dx as f32 + 0.5) / width as f32;
            if settings.mirror_horizontal {
                u = 1.0 - u;
            }
            u = ((u - 0.5) * (aspect_x / zoom) + 0.5).clamp(0.0, 1.0);
            let sx = ((u * (src_width as f32 - 1.0)).round() as u32).min(src_width - 1);

            let s_idx = ((sy * src_width + sx) * 3) as usize;
            let d_idx = ((dy * width + dx) * 3) as usize;

            let in_rgb = [
                src[s_idx] as f32 / 255.0,
                src[s_idx + 1] as f32 / 255.0,
                src[s_idx + 2] as f32 / 255.0,
            ];
            let out_rgb = lut.sample_trilinear(in_rgb, settings.intensity);

            let r_u8 = (out_rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8;
            let g_u8 = (out_rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8;
            let b_u8 = (out_rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8;

            graded_rgb[d_idx] = r_u8;
            graded_rgb[d_idx + 1] = g_u8;
            graded_rgb[d_idx + 2] = b_u8;

            if need_nv12 {
                let y = 16.0 + (46.563 * out_rgb[0] + 156.621 * out_rgb[1] + 15.810 * out_rgb[2]);
                y_plane[(dy * width + dx) as usize] = y.clamp(16.0, 235.0).round() as u8;

                if (dx & 1) == 0 && (dy & 1) == 0 && dx + 1 < width && dy + 1 < height {
                    let cb =
                        128.0 + (-25.653 * out_rgb[0] - 86.343 * out_rgb[1] + 111.996 * out_rgb[2]);
                    let cr =
                        128.0 + (111.996 * out_rgb[0] - 101.720 * out_rgb[1] - 10.276 * out_rgb[2]);
                    let uv_idx = (((dy / 2) * (width / 2) + (dx / 2)) * 2) as usize;
                    uv_plane[uv_idx] = cb.clamp(16.0, 240.0).round() as u8;
                    uv_plane[uv_idx + 1] = cr.clamp(16.0, 240.0).round() as u8;
                }
            }
        }
    }

    let mut preview_jpeg = Vec::with_capacity(jpeg_bytes.len());
    let mut cursor = std::io::Cursor::new(&mut preview_jpeg);
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 86);
    encoder.encode(&graded_rgb, width, height, image::ExtendedColorType::Rgb8)?;

    Ok(ProcessedFrame {
        width,
        height,
        iosurface_id: 0,
        y_plane,
        uv_plane,
        preview_jpeg,
        pipeline_ms: start.elapsed().as_secs_f32() * 1000.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serialize_identity_cube(size: usize) -> String {
        let mut out = format!("TITLE \"Identity {size}\"\nLUT_3D_SIZE {size}\nDOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0\n");
        let denom = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    out.push_str(&format!(
                        "{:.6} {:.6} {:.6}\n",
                        r as f32 / denom,
                        g as f32 / denom,
                        b as f32 / denom
                    ));
                }
            }
        }
        out
    }

    #[test]
    fn parses_standard_17_33_65_cube_luts() {
        for size in [17usize, 33, 65] {
            let text = serialize_identity_cube(size);
            let lut = CubeLut::parse(&text).expect("valid cube LUT");
            assert_eq!(lut.size, size);
            assert!(lut.is_standard_size());
            assert_eq!(lut.data.len(), size * size * size);
            assert_eq!(lut.title, format!("Identity {size}"));

            let sample = lut.sample_trilinear([0.37, 0.61, 0.84], 1.0);
            assert!((sample[0] - 0.37).abs() < 1e-4);
            assert!((sample[1] - 0.61).abs() < 1e-4);
            assert!((sample[2] - 0.84).abs() < 1e-4);
        }
    }

    #[test]
    fn rejects_invalid_cube_inputs() {
        assert_eq!(
            CubeLut::parse("LUT_1D_SIZE 256\n0 0 0\n"),
            Err(LutError::Unsupported1dLut)
        );
        assert_eq!(
            CubeLut::parse("TITLE \"No Size\"\n0 0 0\n"),
            Err(LutError::MissingSize)
        );
        assert!(matches!(
            CubeLut::parse("LUT_3D_SIZE 2\n0 0 0\n"),
            Err(LutError::SampleCountMismatch {
                size: 2,
                expected: 8,
                actual: 1
            })
        ));
    }

    #[test]
    fn builtin_presets_grade_and_blend_cleanly() {
        for preset in BuiltinLutPreset::ALL {
            let lut = preset.build_lut();
            assert_eq!(lut.size, 33);
            let input = [0.42, 0.55, 0.71];
            assert_eq!(lut.sample_trilinear(input, 0.0), input);
            let graded = lut.sample_trilinear(input, 1.0);
            for ch in graded {
                assert!((0.0..=1.0).contains(&ch));
            }
        }
    }
}
