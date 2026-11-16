use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context;
use tracing::debug;

use super::{
    extract_jpeg_slice, parse_jpeg_dimensions, unix_timestamp_us, CameraBrand, CameraCapabilities,
    CameraDriver, CameraPropertyId, LiveViewFrame, PropertyDescriptor, PropertyValue,
};
use crate::ptp::{
    retry_on_device_busy, ByteCursor, PtpDevicePropDesc, PtpSession, PtpTransportBackend,
    UsbPtpTransport,
};

pub const SONY_VID: u16 = 0x054C;

pub mod sony_opcode {
    pub const SDIO_CONNECT: u16 = 0x9201;
    pub const SDIO_GET_EXT_DEVICE_INFO: u16 = 0x9202;
    pub const SDIO_SET_EXT_DEVICE_PROP_VALUE: u16 = 0x9205;
    pub const SDIO_CONTROL_DEVICE: u16 = 0x9207;
    pub const SDIO_GET_ALL_EXT_DEVICE_PROP_INFO: u16 = 0x9209;
    pub const GET_OBJECT: u16 = 0x1009;
}

pub mod sony_prop {
    pub const WHITE_BALANCE: u16 = 0x5005;
    pub const EXPOSURE_COMPENSATION: u16 = 0x5010;
    pub const ISO: u16 = 0xD20D;
    pub const SHUTTER_SPEED: u16 = 0xD20E;
    pub const APERTURE: u16 = 0xD20F;
    pub const COLOR_TEMPERATURE: u16 = 0xD21C;
    pub const FOCUS_MODE: u16 = 0xD21D;
    pub const AF_SHUTTER_HALF_PRESS: u16 = 0xD2C1;
    pub const MANUAL_FOCUS_STEP: u16 = 0xD2D1;
    pub const LIVE_VIEW_OBJECT_HANDLE: u32 = 0xFFFF_C002;
}

/// Sony Alpha USB PC Remote (SDIO) PTP driver.
pub struct SonyDriver<T: PtpTransportBackend = UsbPtpTransport> {
    session: PtpSession<T>,
    capabilities: CameraCapabilities,
    prop_data_types: HashMap<CameraPropertyId, u16>,
    live_view_enabled: bool,
    frame_seq: u64,
}

impl SonyDriver<UsbPtpTransport> {
    pub fn open_usb(product_id: Option<u16>, serial: Option<&str>) -> anyhow::Result<Self> {
        let transport = UsbPtpTransport::open_matching(SONY_VID, product_id, serial)?;
        Ok(Self::new(transport))
    }
}

impl<T: PtpTransportBackend> SonyDriver<T> {
    pub fn new(transport: T) -> Self {
        Self {
            session: PtpSession::new(transport),
            capabilities: CameraCapabilities {
                brand: CameraBrand::Sony,
                camera_model: String::from("Sony Alpha"),
                lens_model: String::from("E-Mount Lens"),
                firmware_version: String::new(),
                serial_number: String::new(),
                battery_percent: None,
                exposure_mode: String::from("M (Manual)"),
                manual_mode_active: true,
                properties: HashMap::new(),
                supports_autofocus: true,
                supports_touch_autofocus: false,
                supports_manual_focus_drive: true,
            },
            prop_data_types: HashMap::new(),
            live_view_enabled: false,
            frame_seq: 0,
        }
    }

    fn refresh_ext_props(&mut self) -> anyhow::Result<Vec<(CameraPropertyId, PropertyDescriptor)>> {
        let (data, _) = self
            .session
            .transport
            .command_data_in(sony_opcode::SDIO_GET_ALL_EXT_DEVICE_PROP_INFO, &[0])?;
        let parsed = parse_sony_ext_prop_table_with_types(&data);
        let mut updated = Vec::with_capacity(parsed.len());
        for (id, data_type, desc) in parsed {
            self.prop_data_types.insert(id, data_type);
            self.capabilities.properties.insert(id, desc.clone());
            updated.push((id, desc));
        }
        Ok(updated)
    }
}

impl<T: PtpTransportBackend> CameraDriver for SonyDriver<T> {
    fn brand(&self) -> CameraBrand {
        CameraBrand::Sony
    }

    fn connect(&mut self) -> anyhow::Result<CameraCapabilities> {
        let info = self.session.get_device_info()?;
        if !info.model.is_empty() {
            self.capabilities.camera_model = info.model;
        }
        self.capabilities.firmware_version = info.device_version;
        self.capabilities.serial_number = info.serial_number;

        self.session.open_session(1)?;

        // Sony 3-phase SDIO PC Remote handshake.
        self.session
            .transport
            .command_nodata(sony_opcode::SDIO_CONNECT, &[1, 0, 0])
            .context("Sony SDIO_Connect phase 1 failed")?;
        self.session
            .transport
            .command_nodata(sony_opcode::SDIO_CONNECT, &[2, 0, 0])
            .context("Sony SDIO_Connect phase 2 failed")?;
        let _ = self
            .session
            .transport
            .command_data_in(sony_opcode::SDIO_GET_EXT_DEVICE_INFO, &[0x012C]);
        self.session
            .transport
            .command_nodata(sony_opcode::SDIO_CONNECT, &[3, 0, 0])
            .context("Sony SDIO_Connect phase 3 failed")?;

        let _ = self.refresh_ext_props();
        debug!(model = %self.capabilities.camera_model, "connected Sony SDIO session");
        Ok(self.capabilities.clone())
    }

    fn start_live_view(&mut self) -> anyhow::Result<()> {
        self.live_view_enabled = true;
        Ok(())
    }

    fn stop_live_view(&mut self) -> anyhow::Result<()> {
        self.live_view_enabled = false;
        Ok(())
    }

    fn next_frame(&mut self) -> anyhow::Result<LiveViewFrame> {
        let start = Instant::now();
        let transport = &mut self.session.transport;
        let (payload, _) = retry_on_device_busy(6, Duration::from_millis(12), || {
            transport.command_data_in(
                sony_opcode::GET_OBJECT,
                &[sony_prop::LIVE_VIEW_OBJECT_HANDLE],
            )
        })?;
        let jpeg = extract_jpeg_slice(&payload).ok_or_else(|| {
            anyhow::anyhow!(
                "Sony Live View payload ({} bytes) missing JPEG frame",
                payload.len()
            )
        })?;
        self.frame_seq += 1;
        let (width, height) = parse_jpeg_dimensions(jpeg);
        Ok(LiveViewFrame {
            jpeg_bytes: jpeg.to_vec(),
            sequence: self.frame_seq,
            timestamp_us: unix_timestamp_us(),
            width,
            height,
            usb_transfer_ms: start.elapsed().as_secs_f32() * 1000.0,
        })
    }

    fn set_property(&mut self, prop: CameraPropertyId, raw_value: u32) -> anyhow::Result<()> {
        let code = reverse_sony_prop(prop)
            .ok_or_else(|| anyhow::anyhow!("unsupported Sony property {prop:?}"))?;
        let data_type = self.prop_data_types.get(&prop).copied().unwrap_or(0x0006);
        let payload = PtpDevicePropDesc::encode_value(data_type, raw_value);
        let transport = &mut self.session.transport;
        retry_on_device_busy(6, Duration::from_millis(15), || {
            transport.command_data_out(
                sony_opcode::SDIO_SET_EXT_DEVICE_PROP_VALUE,
                &[u32::from(code)],
                &payload,
            )
        })?;
        if let Some(desc) = self.capabilities.properties.get_mut(&prop) {
            desc.current = PropertyValue {
                raw: raw_value,
                label: format_sony_value(prop, raw_value),
            };
        }
        Ok(())
    }

    fn poll_events(&mut self) -> anyhow::Result<Vec<(CameraPropertyId, PropertyDescriptor)>> {
        self.refresh_ext_props()
    }

    fn trigger_autofocus(
        &mut self,
        _norm_x: Option<f32>,
        _norm_y: Option<f32>,
    ) -> anyhow::Result<()> {
        self.session.transport.command_data_out(
            sony_opcode::SDIO_CONTROL_DEVICE,
            &[u32::from(sony_prop::AF_SHUTTER_HALF_PRESS)],
            &2u16.to_le_bytes(),
        )?;
        self.session.transport.command_data_out(
            sony_opcode::SDIO_CONTROL_DEVICE,
            &[u32::from(sony_prop::AF_SHUTTER_HALF_PRESS)],
            &1u16.to_le_bytes(),
        )?;
        Ok(())
    }

    fn drive_manual_focus(&mut self, step: i32) -> anyhow::Result<()> {
        if step == 0 {
            return Ok(());
        }
        let encoded = (step.clamp(-7, 7) as i16) as u16;
        self.session.transport.command_data_out(
            sony_opcode::SDIO_CONTROL_DEVICE,
            &[u32::from(sony_prop::MANUAL_FOCUS_STEP)],
            &encoded.to_le_bytes(),
        )?;
        Ok(())
    }

    fn capabilities(&self) -> &CameraCapabilities {
        &self.capabilities
    }

    fn disconnect(&mut self) -> anyhow::Result<()> {
        let _ = self.stop_live_view();
        self.session.close_session()?;
        Ok(())
    }
}

impl<T: PtpTransportBackend> Drop for SonyDriver<T> {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}

/// Parses Sony `SDIO_GetAllExtDevicePropInfo` (0x9209) array records.
pub fn parse_sony_ext_prop_table(data: &[u8]) -> Vec<(CameraPropertyId, PropertyDescriptor)> {
    parse_sony_ext_prop_table_with_types(data)
        .into_iter()
        .map(|(id, _, desc)| (id, desc))
        .collect()
}

fn parse_sony_ext_prop_table_with_types(
    data: &[u8],
) -> Vec<(CameraPropertyId, u16, PropertyDescriptor)> {
    let mut c = ByteCursor::new(data);
    let Ok(count) = c.read_u32() else {
        return Vec::new();
    };
    let _reserved = c.read_u32();
    let mut out = Vec::new();

    for _ in 0..count.min(256) {
        let Ok(prop_code) = c.read_u16() else { break };
        let Ok(data_type) = c.read_u16() else { break };
        let Ok(writable_flag) = c.read_u8() else {
            break;
        };
        let Ok(_is_enable) = c.read_u8() else { break };
        let Ok(_default_val) = c.read_typed_u32(data_type) else {
            break;
        };
        let Ok(current_val) = c.read_typed_u32(data_type) else {
            break;
        };
        let Ok(form_flag) = c.read_u8() else { break };

        let mut allowed_raw = Vec::new();
        match form_flag {
            0x01 => {
                let (Ok(min), Ok(max), Ok(step)) = (
                    c.read_typed_u32(data_type),
                    c.read_typed_u32(data_type),
                    c.read_typed_u32(data_type),
                ) else {
                    break;
                };
                let step = step.max(1);
                let mut v = min;
                let mut n = 0usize;
                while v <= max && n < 256 {
                    allowed_raw.push(v);
                    v = v.saturating_add(step);
                    n += 1;
                }
            }
            0x02 => {
                let Ok(enum_count) = c.read_u16() else { break };
                for _ in 0..usize::from(enum_count).min(512) {
                    let Ok(val) = c.read_typed_u32(data_type) else {
                        break;
                    };
                    allowed_raw.push(val);
                }
            }
            _ => {}
        }

        if let Some(prop_id) = map_sony_prop(prop_code) {
            let allowed_values = allowed_raw
                .into_iter()
                .map(|raw| PropertyValue {
                    raw,
                    label: format_sony_value(prop_id, raw),
                })
                .collect::<Vec<_>>();
            out.push((
                prop_id,
                data_type,
                PropertyDescriptor {
                    id: prop_id,
                    raw_code: u32::from(prop_code),
                    current: PropertyValue {
                        raw: current_val,
                        label: format_sony_value(prop_id, current_val),
                    },
                    writable: writable_flag != 0 && !allowed_values.is_empty(),
                    allowed_values,
                },
            ));
        }
    }

    out
}

fn map_sony_prop(code: u16) -> Option<CameraPropertyId> {
    match code {
        sony_prop::ISO => Some(CameraPropertyId::Iso),
        sony_prop::SHUTTER_SPEED => Some(CameraPropertyId::ShutterSpeed),
        sony_prop::APERTURE => Some(CameraPropertyId::Aperture),
        sony_prop::EXPOSURE_COMPENSATION => Some(CameraPropertyId::ExposureCompensation),
        sony_prop::WHITE_BALANCE => Some(CameraPropertyId::WhiteBalanceMode),
        sony_prop::COLOR_TEMPERATURE => Some(CameraPropertyId::ColorTemperatureKelvin),
        sony_prop::FOCUS_MODE => Some(CameraPropertyId::FocusMode),
        _ => None,
    }
}

fn reverse_sony_prop(id: CameraPropertyId) -> Option<u16> {
    match id {
        CameraPropertyId::Iso => Some(sony_prop::ISO),
        CameraPropertyId::ShutterSpeed => Some(sony_prop::SHUTTER_SPEED),
        CameraPropertyId::Aperture => Some(sony_prop::APERTURE),
        CameraPropertyId::ExposureCompensation => Some(sony_prop::EXPOSURE_COMPENSATION),
        CameraPropertyId::WhiteBalanceMode => Some(sony_prop::WHITE_BALANCE),
        CameraPropertyId::ColorTemperatureKelvin => Some(sony_prop::COLOR_TEMPERATURE),
        CameraPropertyId::FocusMode => Some(sony_prop::FOCUS_MODE),
        CameraPropertyId::FocusArea | CameraPropertyId::FilmSimulation => None,
    }
}

fn format_sony_value(prop: CameraPropertyId, raw: u32) -> String {
    match prop {
        CameraPropertyId::Iso => {
            if raw == 0x00FF_FFFF || raw == 0 {
                "Auto".to_string()
            } else {
                format!("ISO {}", raw & 0x00FF_FFFF)
            }
        }
        CameraPropertyId::Aperture => {
            if raw == 0 {
                "Auto".to_string()
            } else {
                format!("f/{:.1}", raw as f32 / 100.0)
            }
        }
        CameraPropertyId::ShutterSpeed => {
            let num = raw >> 16;
            let den = raw & 0xFFFF;
            if num == 1 && den > 1 {
                format!("1/{den}")
            } else if den == 10 {
                format!("{:.1}\"", num as f32 / 10.0)
            } else {
                format!("0x{raw:08X}")
            }
        }
        CameraPropertyId::ColorTemperatureKelvin => format!("{raw}K"),
        CameraPropertyId::ExposureCompensation => {
            let signed = raw as i16;
            format!("{:+.1} EV", signed as f32 / 1000.0)
        }
        CameraPropertyId::WhiteBalanceMode => match raw {
            0x0002 => "Auto",
            0x0004 => "Daylight",
            0x8011 => "Shade",
            0x8010 => "Cloudy",
            0x0006 => "Tungsten",
            0x8012 => "Kelvin",
            _ => return format!("WB 0x{raw:04X}"),
        }
        .to_string(),
        CameraPropertyId::FocusMode => match raw {
            0x0001 => "Manual Focus",
            0x0002 => "AF-S",
            0x8004 => "AF-C",
            0x8005 => "AF-A",
            _ => return format!("AF 0x{raw:04X}"),
        }
        .to_string(),
        CameraPropertyId::FocusArea | CameraPropertyId::FilmSimulation => {
            format!("0x{raw:04X}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ptp::MockPtpTransport;

    #[test]
    fn sony_handshake_prop_table_and_live_view_frame() {
        let mock = MockPtpTransport::new();
        mock.push_ok(vec![
            100, 0, 0x11, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 5, b'S', 0, b'o', 0, b'n', 0, b'y', 0, 0, 0, 10, b'S', 0, b'o', 0, b'n', 0,
            b'y', 0, b' ', 0, b'A', 0, b'7', 0, b' ', 0, b'I', 0, b'V', 0, 0, 0, 2, b'1', 0, 0, 0,
            2, b'S', 0, 0, 0,
        ]); // GetDeviceInfo
        mock.push_ok(Vec::new()); // OpenSession
        mock.push_ok(Vec::new()); // SDIO_Connect(1)
        mock.push_ok(Vec::new()); // SDIO_Connect(2)
        mock.push_ok(Vec::new()); // SDIO_GetExtDeviceInfo
        mock.push_ok(Vec::new()); // SDIO_Connect(3)

        // Build Sony GetAllExtDevicePropInfo table with 1 ISO property (0xD20D, u32)
        let mut table = Vec::new();
        table.extend_from_slice(&1u32.to_le_bytes()); // count
        table.extend_from_slice(&0u32.to_le_bytes()); // reserved
        table.extend_from_slice(&sony_prop::ISO.to_le_bytes());
        table.extend_from_slice(&0x0006u16.to_le_bytes()); // u32
        table.push(1); // writable
        table.push(1); // enabled
        table.extend_from_slice(&400u32.to_le_bytes());
        table.extend_from_slice(&800u32.to_le_bytes());
        table.push(2); // enum
        table.extend_from_slice(&2u16.to_le_bytes());
        table.extend_from_slice(&400u32.to_le_bytes());
        table.extend_from_slice(&800u32.to_le_bytes());
        mock.push_ok(table);

        // Live View object with 8-byte Sony header + JPEG
        let mut lv = vec![0xAA; 8];
        lv.extend_from_slice(&[
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x04, 0x00, 0x05, 0xA0, 0x03, 0x01, 0x22,
            0x00, 0xFF, 0xD9,
        ]);
        mock.push_ok(lv);
        mock.push_ok(Vec::new()); // SetExtDevicePropValue
        mock.push_ok(Vec::new()); // CloseSession

        let mut driver = SonyDriver::new(mock);
        let caps = driver.connect().unwrap();
        assert_eq!(caps.camera_model, "Sony A7 IV");
        assert_eq!(
            caps.properties[&CameraPropertyId::Iso].current.label,
            "ISO 800"
        );

        driver.start_live_view().unwrap();
        let frame = driver.next_frame().unwrap();
        assert_eq!((frame.width, frame.height), (1440, 1024));

        driver.set_property(CameraPropertyId::Iso, 400).unwrap();
        assert_eq!(
            driver.capabilities().properties[&CameraPropertyId::Iso]
                .current
                .label,
            "ISO 400"
        );
        driver.disconnect().unwrap();
    }
}
