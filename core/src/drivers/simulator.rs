use std::collections::HashMap;
use std::io::Cursor;
use std::time::Duration;

use image::codecs::jpeg::JpegEncoder;
use image::{ColorType, Rgb, RgbImage};

use super::fujifilm::fuji_prop;
use super::nikon::{nikon_opcode, nikon_prop};
use super::sony::{sony_opcode, sony_prop};
use super::CameraBrand;
use crate::ptp::{opcode, response, PtpDevicePropDesc, PtpError, PtpResponse, PtpTransportBackend};

struct PropDef {
    data_type: u16,
    writable: bool,
    current: u32,
    allowed: Vec<u32>,
}

/// Software-emulated hardware PTP responder for Sony, Nikon, and Fujifilm cameras.
///
/// Implements binary PTP containers, vendor handshakes, property tables, and dynamic
/// `960×640` Live View MJPEG synthesis so every driver can be tested end-to-end without hardware.
pub struct SimulatedPtpTransport {
    brand: CameraBrand,
    model_name: &'static str,
    manufacturer: &'static str,
    firmware: &'static str,
    serial: &'static str,
    session_open: bool,
    live_view_active: bool,
    tx_id: u32,
    props: HashMap<u16, PropDef>,
    prop_order: Vec<u16>,
    af_state: u32,
    focus_bias: i32,
    state_version: u64,
    cached_version: u64,
    cached_jpeg: Vec<u8>,
}

impl SimulatedPtpTransport {
    /// Creates a new PTP hardware simulator configured for `brand`.
    pub fn new(brand: CameraBrand) -> Self {
        let mut props = HashMap::new();
        let mut prop_order = Vec::new();
        let mut push_prop = |code: u16, data_type: u16, current: u32, allowed: Vec<u32>| {
            props.insert(
                code,
                PropDef {
                    data_type,
                    writable: true,
                    current,
                    allowed,
                },
            );
            prop_order.push(code);
        };

        let (model_name, manufacturer, firmware, serial) = match brand {
            CameraBrand::Sony => {
                push_prop(
                    sony_prop::SHUTTER_SPEED,
                    0x0006,
                    (1 << 16) | 125,
                    vec![
                        (1 << 16) | 30,
                        (1 << 16) | 60,
                        (1 << 16) | 125,
                        (1 << 16) | 250,
                        (1 << 16) | 500,
                        (1 << 16) | 1000,
                    ],
                );
                push_prop(
                    sony_prop::APERTURE,
                    0x0004,
                    280,
                    vec![140, 200, 280, 400, 560, 800, 1100],
                );
                push_prop(
                    sony_prop::ISO,
                    0x0006,
                    800,
                    vec![0x00FF_FFFF, 100, 200, 400, 800, 1600, 3200, 6400],
                );
                push_prop(
                    sony_prop::EXPOSURE_COMPENSATION,
                    0x0003,
                    0,
                    vec![
                        (-1000i16 as u16) as u32,
                        (-300i16 as u16) as u32,
                        0,
                        300,
                        1000,
                    ],
                );
                push_prop(
                    sony_prop::WHITE_BALANCE,
                    0x0004,
                    0x8012,
                    vec![0x0002, 0x0004, 0x8011, 0x8010, 0x0006, 0x8012],
                );
                push_prop(
                    sony_prop::COLOR_TEMPERATURE,
                    0x0006,
                    5600,
                    vec![3200, 4000, 4800, 5200, 5600, 6500],
                );
                push_prop(
                    sony_prop::FOCUS_MODE,
                    0x0004,
                    0x8004,
                    vec![0x0001, 0x0002, 0x8004, 0x8005],
                );
                (
                    "Sony Alpha 7 IV",
                    "Sony Corporation",
                    "3.00",
                    "sim-sony-a7iv",
                )
            }
            CameraBrand::Nikon => {
                push_prop(
                    nikon_prop::SHUTTER_SPEED,
                    0x0006,
                    80,
                    vec![333, 166, 80, 40, 20, 10],
                );
                push_prop(
                    nikon_prop::APERTURE,
                    0x0004,
                    280,
                    vec![140, 200, 280, 400, 560, 800, 1100],
                );
                push_prop(
                    nikon_prop::ISO,
                    0x0004,
                    800,
                    vec![100, 200, 400, 800, 1600, 3200, 6400],
                );
                push_prop(
                    nikon_prop::EXPOSURE_COMPENSATION,
                    0x0003,
                    0,
                    vec![
                        (-1000i16 as u16) as u32,
                        (-300i16 as u16) as u32,
                        0,
                        300,
                        1000,
                    ],
                );
                push_prop(
                    nikon_prop::WHITE_BALANCE,
                    0x0004,
                    0x8012,
                    vec![2, 4, 0x8010, 0x8011, 6, 0x8012],
                );
                push_prop(nikon_prop::FOCUS_MODE, 0x0004, 2, vec![0, 1, 2, 3]);
                ("Nikon Z8", "Nikon Corporation", "2.00", "sim-nikon-z8")
            }
            CameraBrand::Fujifilm | CameraBrand::Canon => {
                push_prop(
                    fuji_prop::FILM_SIMULATION,
                    0x0004,
                    0x0006,
                    vec![1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13],
                );
                push_prop(
                    fuji_prop::SHUTTER_SPEED,
                    0x0006,
                    125,
                    vec![30, 60, 125, 250, 500, 1000],
                );
                push_prop(
                    fuji_prop::APERTURE,
                    0x0004,
                    280,
                    vec![140, 200, 280, 400, 560, 800, 1100],
                );
                push_prop(
                    fuji_prop::ISO,
                    0x0006,
                    400,
                    vec![125, 200, 400, 800, 1600, 3200, 6400],
                );
                push_prop(
                    fuji_prop::EXPOSURE_COMPENSATION,
                    0x0003,
                    0,
                    vec![
                        (-1000i16 as u16) as u32,
                        (-300i16 as u16) as u32,
                        0,
                        300,
                        1000,
                    ],
                );
                push_prop(
                    fuji_prop::WHITE_BALANCE,
                    0x0004,
                    2,
                    vec![2, 4, 0x8001, 6, 0x8002],
                );
                ("Fujifilm X-T5", "FUJIFILM", "3.10", "sim-fuji-xt5")
            }
            CameraBrand::Hdmi => ("HDMI Capture", "Generic", "1.0", "sim-hdmi"),
        };

        Self {
            brand,
            model_name,
            manufacturer,
            firmware,
            serial,
            session_open: false,
            live_view_active: false,
            tx_id: 1,
            props,
            prop_order,
            af_state: 0,
            focus_bias: 0,
            state_version: 1,
            cached_version: 0,
            cached_jpeg: Vec::new(),
        }
    }

    fn ok_resp(&mut self) -> PtpResponse {
        self.tx_id += 1;
        PtpResponse {
            code: response::OK,
            transaction_id: self.tx_id,
            params: Vec::new(),
        }
    }

    fn decode_scalar(data_type: u16, data: &[u8]) -> u32 {
        match data_type {
            0x0001 | 0x0002 if !data.is_empty() => u32::from(data[0]),
            0x0003 | 0x0004 if data.len() >= 2 => u32::from(u16::from_le_bytes([data[0], data[1]])),
            _ if data.len() >= 4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            _ if data.len() >= 2 => u32::from(u16::from_le_bytes([data[0], data[1]])),
            _ if !data.is_empty() => u32::from(data[0]),
            _ => 0,
        }
    }

    fn set_prop_bytes(&mut self, code: u16, data: &[u8]) {
        if let Some(def) = self.props.get_mut(&code) {
            def.current = Self::decode_scalar(def.data_type, data);
            self.state_version += 1;
        }
    }

    fn build_device_info(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&100u16.to_le_bytes());
        buf.extend_from_slice(&0x0000_000Bu32.to_le_bytes());
        buf.extend_from_slice(&100u16.to_le_bytes());
        buf.push(0); // vendor extension description
        buf.extend_from_slice(&0u16.to_le_bytes()); // functional mode
        for _ in 0..5 {
            buf.extend_from_slice(&0u32.to_le_bytes()); // ops, events, props, capture, image arrays
        }
        for s in [
            self.manufacturer,
            self.model_name,
            self.firmware,
            self.serial,
        ] {
            encode_ptp_string(&mut buf, s);
        }
        buf
    }

    fn build_sony_prop_table(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.prop_order.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for code in &self.prop_order {
            if let Some(def) = self.props.get(code) {
                out.extend_from_slice(&code.to_le_bytes());
                out.extend_from_slice(&def.data_type.to_le_bytes());
                out.push(u8::from(def.writable));
                out.push(1);
                out.extend_from_slice(&PtpDevicePropDesc::encode_value(def.data_type, def.current));
                out.extend_from_slice(&PtpDevicePropDesc::encode_value(def.data_type, def.current));
                out.push(0x02);
                out.extend_from_slice(&(def.allowed.len() as u16).to_le_bytes());
                for &val in &def.allowed {
                    out.extend_from_slice(&PtpDevicePropDesc::encode_value(def.data_type, val));
                }
            }
        }
        out
    }

    fn build_standard_prop_desc(&self, code: u16) -> Option<Vec<u8>> {
        let def = self.props.get(&code)?;
        let mut out = Vec::new();
        out.extend_from_slice(&code.to_le_bytes());
        out.extend_from_slice(&def.data_type.to_le_bytes());
        out.push(u8::from(def.writable));
        out.extend_from_slice(&PtpDevicePropDesc::encode_value(def.data_type, def.current));
        out.extend_from_slice(&PtpDevicePropDesc::encode_value(def.data_type, def.current));
        out.push(0x02);
        out.extend_from_slice(&(def.allowed.len() as u16).to_le_bytes());
        for &val in &def.allowed {
            out.extend_from_slice(&PtpDevicePropDesc::encode_value(def.data_type, val));
        }
        Some(out)
    }

    fn live_view_jpeg(&mut self) -> Vec<u8> {
        if self.cached_version == self.state_version && !self.cached_jpeg.is_empty() {
            return self.cached_jpeg.clone();
        }

        let (gain, warmth, saturation, contrast, monochrome, film_badge_idx) = self.visual_params();
        const W: u32 = 960;
        const H: u32 = 640;
        let mut img = RgbImage::new(W, H);

        for (x, y, px) in img.enumerate_pixels_mut() {
            let nx = x as f32 / W as f32;
            let ny = y as f32 / H as f32;

            // Studio gradient backdrop with subtle architectural depth planes.
            let radial = ((nx - 0.5).powi(2) + (ny - 0.45).powi(2)).sqrt();
            let vignette = (1.0 - radial * 0.72).clamp(0.28, 1.0);
            let mut r = (0.14 + 0.30 * (1.0 - ny) + warmth * 0.10) * vignette;
            let mut g = (0.16 + 0.24 * (1.0 - radial)) * vignette;
            let mut b = (0.20 + 0.34 * nx - warmth * 0.08) * vignette;

            // Brand studio subject chart + color swatches in center region.
            if (0.34..=0.66).contains(&nx) && (0.28..=0.72).contains(&ny) {
                let cx = (nx - 0.50).abs();
                let cy = (ny - 0.50).abs();
                let box_dist = cx.max(cy);
                if box_dist > 0.145 {
                    r = 0.72;
                    g = 0.74;
                    b = 0.76;
                } else {
                    // Calibrated color checker strip inside studio card.
                    let cell_x = ((nx - 0.36) / 0.28 * 4.0).floor() as i32;
                    let cell_y = ((ny - 0.30) / 0.40 * 3.0).floor() as i32;
                    match (cell_x, cell_y) {
                        (0, 0) => (r, g, b) = (0.76, 0.24, 0.24),
                        (1, 0) => (r, g, b) = (0.26, 0.64, 0.36),
                        (2, 0) => (r, g, b) = (0.24, 0.44, 0.78),
                        (3, 0) => (r, g, b) = (0.82, 0.68, 0.22),
                        (0, 1) => (r, g, b) = (0.72, 0.42, 0.58),
                        (1, 1) => (r, g, b) = (0.22, 0.62, 0.66),
                        (2, 1) => (r, g, b) = (0.82, 0.48, 0.22),
                        (3, 1) => (r, g, b) = (0.38, 0.38, 0.72),
                        (c, _) => {
                            let gray = 0.16 + (c.clamp(0, 3) as f32) * 0.22;
                            (r, g, b) = (gray, gray, gray);
                        }
                    }
                }
            }

            // Corner color identity accent bar per camera brand.
            if ny < 0.03 {
                match self.brand {
                    CameraBrand::Sony => (r, g, b) = (0.85, 0.42, 0.12),
                    CameraBrand::Nikon => (r, g, b) = (0.86, 0.74, 0.14),
                    CameraBrand::Fujifilm | CameraBrand::Canon | CameraBrand::Hdmi => {
                        let shift = (film_badge_idx % 3) as f32 * 0.15;
                        (r, g, b) = (0.18 + shift, 0.64, 0.42);
                    }
                }
            }

            // Subtle AF focus-peaking border pulse when AF or manual focus adjusts.
            let border = !(4..W - 4).contains(&x) || !(4..H - 4).contains(&y);
            if border && (self.af_state > 0 || self.focus_bias != 0) {
                (r, g, b) = (0.25, 0.78, 0.46);
            }

            // Apply exposure gain, contrast, saturation, or Fujifilm Monochrome/ACROS.
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            if monochrome {
                r = luma;
                g = luma;
                b = luma;
            } else {
                r = luma + (r - luma) * saturation;
                g = luma + (g - luma) * saturation;
                b = luma + (b - luma) * saturation;
            }

            let apply_curve = |v: f32| -> u8 {
                let c = ((v * gain - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
                (c * 255.0).round() as u8
            };
            *px = Rgb([apply_curve(r), apply_curve(g), apply_curve(b)]);
        }

        let mut jpeg_buf = Vec::with_capacity(64 * 1024);
        let mut cursor = Cursor::new(&mut jpeg_buf);
        let mut encoder = JpegEncoder::new_with_quality(&mut cursor, 85);
        let _ = encoder.encode(img.as_raw(), W, H, ColorType::Rgb8.into());
        self.cached_version = self.state_version;
        self.cached_jpeg = jpeg_buf.clone();
        jpeg_buf
    }

    fn visual_params(&self) -> (f32, f32, f32, f32, bool, u32) {
        let (iso, aperture, ev_raw, wb_raw, sim_raw) = match self.brand {
            CameraBrand::Sony => (
                self.props
                    .get(&sony_prop::ISO)
                    .map(|p| p.current)
                    .unwrap_or(800),
                self.props
                    .get(&sony_prop::APERTURE)
                    .map(|p| p.current)
                    .unwrap_or(280),
                self.props
                    .get(&sony_prop::EXPOSURE_COMPENSATION)
                    .map(|p| p.current)
                    .unwrap_or(0),
                self.props
                    .get(&sony_prop::COLOR_TEMPERATURE)
                    .map(|p| p.current)
                    .unwrap_or(5600),
                0,
            ),
            CameraBrand::Nikon => (
                self.props
                    .get(&nikon_prop::ISO)
                    .map(|p| p.current)
                    .unwrap_or(800),
                self.props
                    .get(&nikon_prop::APERTURE)
                    .map(|p| p.current)
                    .unwrap_or(280),
                self.props
                    .get(&nikon_prop::EXPOSURE_COMPENSATION)
                    .map(|p| p.current)
                    .unwrap_or(0),
                self.props
                    .get(&nikon_prop::WHITE_BALANCE)
                    .map(|p| p.current)
                    .unwrap_or(0x8012),
                0,
            ),
            CameraBrand::Fujifilm | CameraBrand::Canon | CameraBrand::Hdmi => (
                self.props
                    .get(&fuji_prop::ISO)
                    .map(|p| p.current)
                    .unwrap_or(400),
                self.props
                    .get(&fuji_prop::APERTURE)
                    .map(|p| p.current)
                    .unwrap_or(280),
                self.props
                    .get(&fuji_prop::EXPOSURE_COMPENSATION)
                    .map(|p| p.current)
                    .unwrap_or(0),
                self.props
                    .get(&fuji_prop::WHITE_BALANCE)
                    .map(|p| p.current)
                    .unwrap_or(2),
                self.props
                    .get(&fuji_prop::FILM_SIMULATION)
                    .map(|p| p.current)
                    .unwrap_or(6),
            ),
        };

        let iso_f = (iso & 0x00FF_FFFF).clamp(100, 6400) as f32;
        let iso_stops = (iso_f / 400.0).log2();
        let fstop = (aperture.clamp(140, 1100) as f32) / 100.0;
        let ap_stops = -(fstop / 2.8).log2();
        let ev_stops = (ev_raw as i16) as f32 / 1000.0;
        let gain = 2f32
            .powf((iso_stops + ap_stops + ev_stops) * 0.25)
            .clamp(0.55, 1.85);

        let warmth = if wb_raw >= 3000 {
            ((wb_raw as f32 - 5400.0) / 2500.0).clamp(-0.6, 0.6)
        } else if wb_raw == 6 {
            -0.35
        } else if wb_raw == 0x8001 || wb_raw == 0x8011 {
            0.25
        } else {
            0.0
        };

        let (sat, contrast, mono) = match (self.brand, sim_raw) {
            (CameraBrand::Fujifilm, 2) => (1.42, 1.18, false), // Velvia
            (CameraBrand::Fujifilm, 4 | 9) => (0.0, 1.22, true), // Monochrome / ACROS
            (CameraBrand::Fujifilm, 6) => (0.78, 1.14, false), // Classic Chrome
            (CameraBrand::Fujifilm, 10) => (0.72, 0.86, false), // ETERNA
            (CameraBrand::Fujifilm, 11) => (0.90, 1.20, false), // Classic Neg.
            _ => (1.0, 1.0, false),
        };
        (gain, warmth, sat, contrast, mono, sim_raw)
    }
}

impl PtpTransportBackend for SimulatedPtpTransport {
    fn reset_transaction_ids(&mut self) {
        self.tx_id = 1;
    }

    fn command_nodata(&mut self, opcode: u16, _params: &[u32]) -> Result<PtpResponse, PtpError> {
        match opcode {
            opcode::OPEN_SESSION => self.session_open = true,
            opcode::CLOSE_SESSION => {
                self.session_open = false;
                self.live_view_active = false;
            }
            c if c == sony_opcode::SDIO_CONNECT => {}
            c if c == nikon_opcode::START_LIVE_VIEW => self.live_view_active = true,
            c if c == nikon_opcode::END_LIVE_VIEW => self.live_view_active = false,
            c if c == nikon_opcode::AF_DRIVE => {
                self.af_state += 1;
                self.state_version += 1;
            }
            c if c == nikon_opcode::MF_DRIVE => {
                self.focus_bias = (self.focus_bias + 1) % 4;
                self.state_version += 1;
            }
            _ => {}
        }
        Ok(self.ok_resp())
    }

    fn command_data_in(
        &mut self,
        opcode: u16,
        params: &[u32],
    ) -> Result<(Vec<u8>, PtpResponse), PtpError> {
        let data = match opcode {
            opcode::GET_DEVICE_INFO => self.build_device_info(),
            opcode::GET_DEVICE_PROP_DESC => {
                let code = params.first().copied().unwrap_or(0) as u16;
                self.build_standard_prop_desc(code)
                    .ok_or(PtpError::ProtocolError {
                        opcode,
                        code: response::PARAMETER_NOT_SUPPORTED,
                    })?
            }
            c if c == sony_opcode::SDIO_GET_EXT_DEVICE_INFO => Vec::new(),
            c if c == sony_opcode::SDIO_GET_ALL_EXT_DEVICE_PROP_INFO => {
                self.build_sony_prop_table()
            }
            opcode::GET_OBJECT => {
                let jpeg = self.live_view_jpeg();
                if self.brand == CameraBrand::Sony {
                    let mut framed = vec![0u8; 8];
                    framed.extend_from_slice(&jpeg);
                    framed
                } else {
                    jpeg
                }
            }
            c if c == nikon_opcode::GET_LIVE_VIEW_IMAGE => {
                let mut framed = vec![0u8; 384];
                framed.extend_from_slice(&self.live_view_jpeg());
                framed
            }
            c if c == nikon_opcode::GET_EVENT => vec![0, 0],
            _ => {
                return Err(PtpError::ProtocolError {
                    opcode,
                    code: response::OPERATION_NOT_SUPPORTED,
                })
            }
        };
        Ok((data, self.ok_resp()))
    }

    fn command_data_out(
        &mut self,
        opcode: u16,
        params: &[u32],
        data: &[u8],
    ) -> Result<PtpResponse, PtpError> {
        match opcode {
            opcode::SET_DEVICE_PROP_VALUE => {
                let code = params.first().copied().unwrap_or(0) as u16;
                if code == fuji_prop::LIVE_VIEW_MODE {
                    self.live_view_active = !data.is_empty() && data[0] != 0;
                } else if code == 0xD209 {
                    self.af_state += 1;
                    self.state_version += 1;
                } else {
                    self.set_prop_bytes(code, data);
                }
            }
            c if c == sony_opcode::SDIO_SET_EXT_DEVICE_PROP_VALUE => {
                let code = params.first().copied().unwrap_or(0) as u16;
                self.set_prop_bytes(code, data);
            }
            c if c == sony_opcode::SDIO_CONTROL_DEVICE => {
                let code = params.first().copied().unwrap_or(0) as u16;
                if code == sony_prop::AF_SHUTTER_HALF_PRESS {
                    self.af_state += 1;
                } else if code == sony_prop::MANUAL_FOCUS_STEP {
                    self.focus_bias = (self.focus_bias + 1) % 4;
                }
                self.state_version += 1;
            }
            _ => {}
        }
        Ok(self.ok_resp())
    }

    fn poll_interrupt(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, PtpError> {
        Ok(None)
    }
}

fn encode_ptp_string(out: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        out.push(0);
        return;
    }
    let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    out.push(units.len().min(255) as u8);
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::fujifilm::FujifilmDriver;
    use crate::drivers::nikon::NikonDriver;
    use crate::drivers::sony::SonyDriver;
    use crate::drivers::{parse_jpeg_dimensions, CameraDriver, CameraPropertyId};

    #[test]
    fn simulated_sony_nikon_and_fujifilm_drivers_connect_stream_and_update_props() {
        // Sony Alpha 7 IV over SimulatedPtpTransport
        let mut sony = SonyDriver::new(SimulatedPtpTransport::new(CameraBrand::Sony));
        let sony_caps = sony.connect().unwrap();
        assert_eq!(sony_caps.camera_model, "Sony Alpha 7 IV");
        assert!(sony_caps.properties.contains_key(&CameraPropertyId::Iso));
        sony.start_live_view().unwrap();
        let frame = sony.next_frame().unwrap();
        assert_eq!(parse_jpeg_dimensions(&frame.jpeg_bytes), (960, 640));
        sony.set_property(CameraPropertyId::Iso, 1600).unwrap();
        assert_eq!(
            sony.capabilities().properties[&CameraPropertyId::Iso]
                .current
                .label,
            "ISO 1600"
        );
        sony.disconnect().unwrap();

        // Nikon Z8 over SimulatedPtpTransport
        let mut nikon = NikonDriver::new(SimulatedPtpTransport::new(CameraBrand::Nikon));
        let nikon_caps = nikon.connect().unwrap();
        assert_eq!(nikon_caps.camera_model, "Nikon Z8");
        nikon.start_live_view().unwrap();
        let frame = nikon.next_frame().unwrap();
        assert_eq!(parse_jpeg_dimensions(&frame.jpeg_bytes), (960, 640));
        nikon.set_property(CameraPropertyId::Iso, 400).unwrap();
        assert_eq!(
            nikon.capabilities().properties[&CameraPropertyId::Iso]
                .current
                .label,
            "ISO 400"
        );
        nikon.disconnect().unwrap();

        // Fujifilm X-T5 over SimulatedPtpTransport
        let mut fuji = FujifilmDriver::new(SimulatedPtpTransport::new(CameraBrand::Fujifilm));
        let fuji_caps = fuji.connect().unwrap();
        assert_eq!(fuji_caps.camera_model, "Fujifilm X-T5");
        assert!(fuji_caps
            .properties
            .contains_key(&CameraPropertyId::FilmSimulation));
        fuji.start_live_view().unwrap();
        let frame = fuji.next_frame().unwrap();
        assert_eq!(parse_jpeg_dimensions(&frame.jpeg_bytes), (960, 640));
        fuji.set_property(CameraPropertyId::FilmSimulation, 0x0009)
            .unwrap();
        assert_eq!(
            fuji.capabilities().properties[&CameraPropertyId::FilmSimulation]
                .current
                .label,
            "ACROS"
        );
        fuji.disconnect().unwrap();
    }
}
