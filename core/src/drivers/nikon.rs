use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context;
use tracing::debug;

use super::{
    extract_jpeg_slice, parse_jpeg_dimensions, unix_timestamp_us, CameraBrand, CameraCapabilities,
    CameraDriver, CameraPropertyId, LiveViewFrame, PropertyDescriptor, PropertyValue,
};
use crate::ptp::{
    opcode, retry_on_device_busy, PtpDevicePropDesc, PtpSession, PtpTransportBackend,
    UsbPtpTransport,
};

pub const NIKON_VID: u16 = 0x04B0;

pub mod nikon_opcode {
    pub const AF_DRIVE: u16 = 0x90C1;
    pub const GET_EVENT: u16 = 0x90C7;
    pub const DEVICE_READY: u16 = 0x90C8;
    pub const START_LIVE_VIEW: u16 = 0x9201;
    pub const END_LIVE_VIEW: u16 = 0x9202;
    pub const GET_LIVE_VIEW_IMAGE: u16 = 0x9203;
    pub const MF_DRIVE: u16 = 0x9204;
    pub const CHANGE_AF_AREA: u16 = 0x9205;
}

pub mod nikon_prop {
    pub const WHITE_BALANCE: u16 = 0x5005;
    pub const APERTURE: u16 = 0x5007;
    pub const SHUTTER_SPEED: u16 = 0x500D;
    pub const ISO: u16 = 0x500F;
    pub const EXPOSURE_COMPENSATION: u16 = 0x5010;
    pub const FOCUS_MODE: u16 = 0xD161;
}

/// Nikon Z/D-series USB PTP driver.
pub struct NikonDriver<T: PtpTransportBackend = UsbPtpTransport> {
    session: PtpSession<T>,
    capabilities: CameraCapabilities,
    prop_data_types: HashMap<CameraPropertyId, u16>,
    live_view_enabled: bool,
    frame_seq: u64,
}

impl NikonDriver<UsbPtpTransport> {
    pub fn open_usb(product_id: Option<u16>, serial: Option<&str>) -> anyhow::Result<Self> {
        let transport = UsbPtpTransport::open_matching(NIKON_VID, product_id, serial)?;
        Ok(Self::new(transport))
    }
}

impl<T: PtpTransportBackend> NikonDriver<T> {
    pub fn new(transport: T) -> Self {
        Self {
            session: PtpSession::new(transport),
            capabilities: CameraCapabilities {
                brand: CameraBrand::Nikon,
                camera_model: String::from("Nikon Z"),
                lens_model: String::from("NIKKOR Lens"),
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

    fn query_prop(
        &mut self,
        prop_code: u16,
        prop_id: CameraPropertyId,
    ) -> Option<PropertyDescriptor> {
        let (data, _) = self
            .session
            .transport
            .command_data_in(opcode::GET_DEVICE_PROP_DESC, &[u32::from(prop_code)])
            .ok()?;
        let parsed = PtpDevicePropDesc::parse(&data).ok()?;
        self.prop_data_types.insert(prop_id, parsed.data_type);

        let allowed_values = parsed
            .allowed_values
            .into_iter()
            .map(|raw| PropertyValue {
                raw,
                label: format_nikon_value(prop_id, raw),
            })
            .collect::<Vec<_>>();
        let desc = PropertyDescriptor {
            id: prop_id,
            raw_code: u32::from(prop_code),
            current: PropertyValue {
                raw: parsed.current_value,
                label: format_nikon_value(prop_id, parsed.current_value),
            },
            writable: parsed.writable && !allowed_values.is_empty(),
            allowed_values,
        };
        self.capabilities.properties.insert(prop_id, desc.clone());
        Some(desc)
    }
}

impl<T: PtpTransportBackend> CameraDriver for NikonDriver<T> {
    fn brand(&self) -> CameraBrand {
        CameraBrand::Nikon
    }

    fn connect(&mut self) -> anyhow::Result<CameraCapabilities> {
        let info = self.session.get_device_info()?;
        if !info.model.is_empty() {
            self.capabilities.camera_model = info.model;
        }
        self.capabilities.firmware_version = info.device_version;
        self.capabilities.serial_number = info.serial_number;

        self.session.open_session(1)?;

        for &(code, id) in &[
            (nikon_prop::SHUTTER_SPEED, CameraPropertyId::ShutterSpeed),
            (nikon_prop::APERTURE, CameraPropertyId::Aperture),
            (nikon_prop::ISO, CameraPropertyId::Iso),
            (
                nikon_prop::EXPOSURE_COMPENSATION,
                CameraPropertyId::ExposureCompensation,
            ),
            (
                nikon_prop::WHITE_BALANCE,
                CameraPropertyId::WhiteBalanceMode,
            ),
            (nikon_prop::FOCUS_MODE, CameraPropertyId::FocusMode),
        ] {
            let _ = self.query_prop(code, id);
        }

        debug!(model = %self.capabilities.camera_model, "connected Nikon PTP session");
        Ok(self.capabilities.clone())
    }

    fn start_live_view(&mut self) -> anyhow::Result<()> {
        if self.live_view_enabled {
            return Ok(());
        }
        self.session
            .transport
            .command_nodata(nikon_opcode::START_LIVE_VIEW, &[])
            .context("Nikon StartLiveView failed")?;
        self.live_view_enabled = true;
        Ok(())
    }

    fn stop_live_view(&mut self) -> anyhow::Result<()> {
        if !self.live_view_enabled {
            return Ok(());
        }
        let _ = self
            .session
            .transport
            .command_nodata(nikon_opcode::END_LIVE_VIEW, &[]);
        self.live_view_enabled = false;
        Ok(())
    }

    fn next_frame(&mut self) -> anyhow::Result<LiveViewFrame> {
        let start = Instant::now();
        let transport = &mut self.session.transport;
        let (payload, _) = retry_on_device_busy(6, Duration::from_millis(12), || {
            transport.command_data_in(nikon_opcode::GET_LIVE_VIEW_IMAGE, &[])
        })?;
        let jpeg = extract_jpeg_slice(&payload).ok_or_else(|| {
            anyhow::anyhow!(
                "Nikon GetLiveViewImage payload ({} bytes) missing JPEG data",
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
        let prop_code = self
            .capabilities
            .properties
            .get(&prop)
            .map(|d| d.raw_code as u16)
            .or_else(|| reverse_nikon_prop(prop))
            .ok_or_else(|| anyhow::anyhow!("unsupported Nikon property {prop:?}"))?;
        let data_type = self.prop_data_types.get(&prop).copied().unwrap_or(0x0004);
        let payload = PtpDevicePropDesc::encode_value(data_type, raw_value);

        let transport = &mut self.session.transport;
        retry_on_device_busy(6, Duration::from_millis(15), || {
            transport.command_data_out(
                opcode::SET_DEVICE_PROP_VALUE,
                &[u32::from(prop_code)],
                &payload,
            )
        })?;

        if let Some(desc) = self.capabilities.properties.get_mut(&prop) {
            desc.current = PropertyValue {
                raw: raw_value,
                label: format_nikon_value(prop, raw_value),
            };
        }
        Ok(())
    }

    fn poll_events(&mut self) -> anyhow::Result<Vec<(CameraPropertyId, PropertyDescriptor)>> {
        let (data, _) = self
            .session
            .transport
            .command_data_in(nikon_opcode::GET_EVENT, &[])?;
        let mut updated = Vec::new();
        if data.len() < 2 {
            return Ok(updated);
        }
        let count = u16::from_le_bytes([data[0], data[1]]) as usize;
        let mut offset = 2usize;
        for _ in 0..count.min(64) {
            if offset + 6 > data.len() {
                break;
            }
            let ev_code = u16::from_le_bytes([data[offset], data[offset + 1]]);
            let param = u32::from_le_bytes([
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
            ]);
            offset += 6;
            if ev_code == 0x4006 {
                // DevicePropChanged: param holds the PTP property code.
                if let Some(prop_id) = map_nikon_prop(param as u16) {
                    if let Some(desc) = self.query_prop(param as u16, prop_id) {
                        updated.push((prop_id, desc));
                    }
                }
            }
        }
        Ok(updated)
    }

    fn trigger_autofocus(
        &mut self,
        _norm_x: Option<f32>,
        _norm_y: Option<f32>,
    ) -> anyhow::Result<()> {
        self.session
            .transport
            .command_nodata(nikon_opcode::AF_DRIVE, &[])?;
        Ok(())
    }

    fn drive_manual_focus(&mut self, step: i32) -> anyhow::Result<()> {
        if step == 0 {
            return Ok(());
        }
        let direction = if step > 0 { 1u32 } else { 2u32 };
        let amount = (step.unsigned_abs() * 50).min(1500);
        self.session
            .transport
            .command_nodata(nikon_opcode::MF_DRIVE, &[direction, amount])?;
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

impl<T: PtpTransportBackend> Drop for NikonDriver<T> {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}

fn map_nikon_prop(code: u16) -> Option<CameraPropertyId> {
    match code {
        nikon_prop::WHITE_BALANCE => Some(CameraPropertyId::WhiteBalanceMode),
        nikon_prop::APERTURE => Some(CameraPropertyId::Aperture),
        nikon_prop::SHUTTER_SPEED => Some(CameraPropertyId::ShutterSpeed),
        nikon_prop::ISO => Some(CameraPropertyId::Iso),
        nikon_prop::EXPOSURE_COMPENSATION => Some(CameraPropertyId::ExposureCompensation),
        nikon_prop::FOCUS_MODE => Some(CameraPropertyId::FocusMode),
        _ => None,
    }
}

fn reverse_nikon_prop(id: CameraPropertyId) -> Option<u16> {
    match id {
        CameraPropertyId::WhiteBalanceMode => Some(nikon_prop::WHITE_BALANCE),
        CameraPropertyId::Aperture => Some(nikon_prop::APERTURE),
        CameraPropertyId::ShutterSpeed => Some(nikon_prop::SHUTTER_SPEED),
        CameraPropertyId::Iso => Some(nikon_prop::ISO),
        CameraPropertyId::ExposureCompensation => Some(nikon_prop::EXPOSURE_COMPENSATION),
        CameraPropertyId::FocusMode => Some(nikon_prop::FOCUS_MODE),
        CameraPropertyId::ColorTemperatureKelvin
        | CameraPropertyId::FocusArea
        | CameraPropertyId::FilmSimulation => None,
    }
}

fn format_nikon_value(prop: CameraPropertyId, raw: u32) -> String {
    match prop {
        CameraPropertyId::Aperture => format!("f/{:.1}", raw as f32 / 100.0),
        CameraPropertyId::ShutterSpeed => {
            if raw == 0xFFFF_FFFF {
                "Bulb".to_string()
            } else if raw >= 10_000 {
                format!("{:.1}\"", raw as f32 / 10_000.0)
            } else if raw > 0 {
                format!("1/{}", (10_000 / raw).max(1))
            } else {
                "Auto".to_string()
            }
        }
        CameraPropertyId::Iso => format!("ISO {raw}"),
        CameraPropertyId::ExposureCompensation => {
            format!("{:+.1} EV", (raw as i16) as f32 / 1000.0)
        }
        CameraPropertyId::WhiteBalanceMode => match raw {
            2 => "Auto",
            4 => "Daylight",
            5 => "Fluorescent",
            6 => "Tungsten",
            7 => "Flash",
            0x8010 => "Cloudy",
            0x8011 => "Shade",
            0x8012 => "Kelvin",
            _ => return format!("WB {raw}"),
        }
        .to_string(),
        CameraPropertyId::FocusMode => match raw {
            0 => "MF",
            1 => "AF-S",
            2 => "AF-C",
            3 => "AF-A",
            _ => return format!("AF {raw}"),
        }
        .to_string(),
        CameraPropertyId::ColorTemperatureKelvin => format!("{raw}K"),
        CameraPropertyId::FocusArea | CameraPropertyId::FilmSimulation => format!("0x{raw:04X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ptp::MockPtpTransport;

    fn prop_desc_u16(code: u16, cur: u16, vals: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&code.to_le_bytes());
        out.extend_from_slice(&0x0004u16.to_le_bytes());
        out.push(1);
        out.extend_from_slice(&cur.to_le_bytes());
        out.extend_from_slice(&cur.to_le_bytes());
        out.push(2);
        out.extend_from_slice(&(vals.len() as u16).to_le_bytes());
        for &v in vals {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    #[test]
    fn nikon_connect_live_view_and_property_control() {
        let mock = MockPtpTransport::new();
        mock.push_ok(vec![
            100, 0, 0x0A, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 6, b'N', 0, b'i', 0, b'k', 0, b'o', 0, b'n', 0, 0, 0, 9, b'N', 0, b'i', 0,
            b'k', 0, b'o', 0, b'n', 0, b' ', 0, b'Z', 0, b'8', 0, 0, 0, 2, b'2', 0, 0, 0, 2, b'N',
            0, 0, 0,
        ]); // GetDeviceInfo
        mock.push_ok(Vec::new()); // OpenSession
        mock.push_ok(Vec::new()); // Shutter (skip)
        mock.push_ok(prop_desc_u16(nikon_prop::APERTURE, 280, &[280, 400, 560]));
        mock.push_ok(prop_desc_u16(nikon_prop::ISO, 400, &[100, 400, 800]));
        mock.push_ok(Vec::new()); // EV
        mock.push_ok(Vec::new()); // WB
        mock.push_ok(Vec::new()); // Focus
        mock.push_ok(Vec::new()); // StartLiveView
        let mut lv = vec![0x00; 128];
        lv.extend_from_slice(&[
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x04, 0x38, 0x07, 0x80, 0x03, 0x01, 0x22,
            0x00, 0xFF, 0xD9,
        ]);
        mock.push_ok(lv); // GetLiveViewImage
        mock.push_ok(Vec::new()); // SetDevicePropValue
        mock.push_ok(Vec::new()); // EndLiveView
        mock.push_ok(Vec::new()); // CloseSession

        let mut driver = NikonDriver::new(mock);
        let caps = driver.connect().unwrap();
        assert_eq!(caps.camera_model, "Nikon Z8");
        assert_eq!(
            caps.properties[&CameraPropertyId::Aperture].current.label,
            "f/2.8"
        );

        driver.start_live_view().unwrap();
        let frame = driver.next_frame().unwrap();
        assert_eq!((frame.width, frame.height), (1920, 1080));

        driver
            .set_property(CameraPropertyId::Aperture, 400)
            .unwrap();
        assert_eq!(
            driver.capabilities().properties[&CameraPropertyId::Aperture]
                .current
                .label,
            "f/4.0"
        );
        driver.disconnect().unwrap();
    }
}
