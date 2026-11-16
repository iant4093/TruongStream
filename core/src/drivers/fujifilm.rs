use std::collections::HashMap;
use std::time::{Duration, Instant};

use tracing::debug;

use super::{
    extract_jpeg_slice, parse_jpeg_dimensions, unix_timestamp_us, CameraBrand, CameraCapabilities,
    CameraDriver, CameraPropertyId, LiveViewFrame, PropertyDescriptor, PropertyValue,
};
use crate::ptp::{
    opcode, retry_on_device_busy, PtpDevicePropDesc, PtpSession, PtpTransportBackend,
    UsbPtpTransport,
};

pub const FUJIFILM_VID: u16 = 0x04CB;

pub mod fuji_prop {
    pub const WHITE_BALANCE: u16 = 0x5005;
    pub const APERTURE: u16 = 0x5007;
    pub const SHUTTER_SPEED: u16 = 0xD018;
    pub const ISO: u16 = 0xD218;
    pub const EXPOSURE_COMPENSATION: u16 = 0x5010;
    pub const FILM_SIMULATION: u16 = 0xD001;
    pub const LIVE_VIEW_MODE: u16 = 0xD208;
    pub const LIVE_VIEW_OBJECT_HANDLE: u32 = 0x8000_0001;
}

/// Fujifilm X / GFX USB PTP driver with Film Simulation support.
pub struct FujifilmDriver<T: PtpTransportBackend = UsbPtpTransport> {
    session: PtpSession<T>,
    capabilities: CameraCapabilities,
    prop_data_types: HashMap<CameraPropertyId, u16>,
    live_view_enabled: bool,
    frame_seq: u64,
}

impl FujifilmDriver<UsbPtpTransport> {
    pub fn open_usb(product_id: Option<u16>, serial: Option<&str>) -> anyhow::Result<Self> {
        let transport = UsbPtpTransport::open_matching(FUJIFILM_VID, product_id, serial)?;
        Ok(Self::new(transport))
    }
}

impl<T: PtpTransportBackend> FujifilmDriver<T> {
    pub fn new(transport: T) -> Self {
        Self {
            session: PtpSession::new(transport),
            capabilities: CameraCapabilities {
                brand: CameraBrand::Fujifilm,
                camera_model: String::from("Fujifilm X"),
                lens_model: String::from("FUJINON Lens"),
                firmware_version: String::new(),
                serial_number: String::new(),
                battery_percent: None,
                exposure_mode: String::from("M (Manual)"),
                manual_mode_active: true,
                properties: HashMap::new(),
                supports_autofocus: true,
                supports_touch_autofocus: false,
                supports_manual_focus_drive: false,
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
                label: format_fuji_value(prop_id, raw),
            })
            .collect::<Vec<_>>();
        let desc = PropertyDescriptor {
            id: prop_id,
            raw_code: u32::from(prop_code),
            current: PropertyValue {
                raw: parsed.current_value,
                label: format_fuji_value(prop_id, parsed.current_value),
            },
            writable: parsed.writable && !allowed_values.is_empty(),
            allowed_values,
        };
        self.capabilities.properties.insert(prop_id, desc.clone());
        Some(desc)
    }
}

impl<T: PtpTransportBackend> CameraDriver for FujifilmDriver<T> {
    fn brand(&self) -> CameraBrand {
        CameraBrand::Fujifilm
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
            (fuji_prop::FILM_SIMULATION, CameraPropertyId::FilmSimulation),
            (fuji_prop::APERTURE, CameraPropertyId::Aperture),
            (fuji_prop::SHUTTER_SPEED, CameraPropertyId::ShutterSpeed),
            (fuji_prop::ISO, CameraPropertyId::Iso),
            (
                fuji_prop::EXPOSURE_COMPENSATION,
                CameraPropertyId::ExposureCompensation,
            ),
            (fuji_prop::WHITE_BALANCE, CameraPropertyId::WhiteBalanceMode),
        ] {
            let _ = self.query_prop(code, id);
        }

        debug!(model = %self.capabilities.camera_model, "connected Fujifilm PTP session");
        Ok(self.capabilities.clone())
    }

    fn start_live_view(&mut self) -> anyhow::Result<()> {
        if self.live_view_enabled {
            return Ok(());
        }
        let _ = self.session.transport.command_data_out(
            opcode::SET_DEVICE_PROP_VALUE,
            &[u32::from(fuji_prop::LIVE_VIEW_MODE)],
            &1u16.to_le_bytes(),
        );
        self.live_view_enabled = true;
        Ok(())
    }

    fn stop_live_view(&mut self) -> anyhow::Result<()> {
        if !self.live_view_enabled {
            return Ok(());
        }
        let _ = self.session.transport.command_data_out(
            opcode::SET_DEVICE_PROP_VALUE,
            &[u32::from(fuji_prop::LIVE_VIEW_MODE)],
            &0u16.to_le_bytes(),
        );
        self.live_view_enabled = false;
        Ok(())
    }

    fn next_frame(&mut self) -> anyhow::Result<LiveViewFrame> {
        let start = Instant::now();
        let transport = &mut self.session.transport;
        let (payload, _) = retry_on_device_busy(6, Duration::from_millis(12), || {
            transport.command_data_in(opcode::GET_OBJECT, &[fuji_prop::LIVE_VIEW_OBJECT_HANDLE])
        })?;
        let jpeg = extract_jpeg_slice(&payload).ok_or_else(|| {
            anyhow::anyhow!(
                "Fujifilm Live View payload ({} bytes) missing JPEG frame",
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
            .or_else(|| reverse_fuji_prop(prop))
            .ok_or_else(|| anyhow::anyhow!("unsupported Fujifilm property {prop:?}"))?;
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
                label: format_fuji_value(prop, raw_value),
            };
        }
        Ok(())
    }

    fn poll_events(&mut self) -> anyhow::Result<Vec<(CameraPropertyId, PropertyDescriptor)>> {
        let mut changed = Vec::new();
        for &(code, id) in &[
            (fuji_prop::FILM_SIMULATION, CameraPropertyId::FilmSimulation),
            (fuji_prop::ISO, CameraPropertyId::Iso),
        ] {
            let prev = self.capabilities.properties.get(&id).map(|d| d.current.raw);
            if let Some(desc) = self.query_prop(code, id) {
                if Some(desc.current.raw) != prev {
                    changed.push((id, desc));
                }
            }
        }
        Ok(changed)
    }

    fn trigger_autofocus(
        &mut self,
        _norm_x: Option<f32>,
        _norm_y: Option<f32>,
    ) -> anyhow::Result<()> {
        // Trigger single AF capture-focus prep via vendor prop 0xD209 if supported.
        let _ = self.session.transport.command_data_out(
            opcode::SET_DEVICE_PROP_VALUE,
            &[0xD209],
            &1u16.to_le_bytes(),
        );
        Ok(())
    }

    fn drive_manual_focus(&mut self, _step: i32) -> anyhow::Result<()> {
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

impl<T: PtpTransportBackend> Drop for FujifilmDriver<T> {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}

fn reverse_fuji_prop(id: CameraPropertyId) -> Option<u16> {
    match id {
        CameraPropertyId::FilmSimulation => Some(fuji_prop::FILM_SIMULATION),
        CameraPropertyId::Aperture => Some(fuji_prop::APERTURE),
        CameraPropertyId::ShutterSpeed => Some(fuji_prop::SHUTTER_SPEED),
        CameraPropertyId::Iso => Some(fuji_prop::ISO),
        CameraPropertyId::ExposureCompensation => Some(fuji_prop::EXPOSURE_COMPENSATION),
        CameraPropertyId::WhiteBalanceMode => Some(fuji_prop::WHITE_BALANCE),
        CameraPropertyId::ColorTemperatureKelvin
        | CameraPropertyId::FocusMode
        | CameraPropertyId::FocusArea => None,
    }
}

fn format_fuji_value(prop: CameraPropertyId, raw: u32) -> String {
    match prop {
        CameraPropertyId::FilmSimulation => match raw {
            0x0001 => "PROVIA / Standard",
            0x0002 => "Velvia / Vivid",
            0x0003 => "ASTIA / Soft",
            0x0004 => "Monochrome",
            0x0006 => "Classic Chrome",
            0x0007 => "PRO Neg. Hi",
            0x0008 => "PRO Neg. Std",
            0x0009 => "ACROS",
            0x000A => "ETERNA / Cinema",
            0x000B => "Classic Neg.",
            0x000C => "Nostalgic Neg.",
            0x000D => "REALA ACE",
            _ => return format!("FilmSim 0x{raw:04X}"),
        }
        .to_string(),
        CameraPropertyId::Aperture => format!("f/{:.1}", raw as f32 / 100.0),
        CameraPropertyId::Iso => format!("ISO {}", raw & 0xFFFF),
        CameraPropertyId::ShutterSpeed => match raw {
            30 => "1/30".to_string(),
            60 => "1/60".to_string(),
            125 => "1/125".to_string(),
            250 => "1/250".to_string(),
            500 => "1/500".to_string(),
            1000 => "1/1000".to_string(),
            _ => format!("1/{raw}"),
        },
        CameraPropertyId::ExposureCompensation => {
            format!("{:+.1} EV", (raw as i16) as f32 / 1000.0)
        }
        CameraPropertyId::WhiteBalanceMode => match raw {
            2 => "Auto".to_string(),
            4 => "Daylight".to_string(),
            6 => "Tungsten".to_string(),
            0x8001 => "Shade".to_string(),
            0x8002 => "Kelvin".to_string(),
            _ => format!("WB 0x{raw:04X}"),
        },
        CameraPropertyId::ColorTemperatureKelvin => format!("{raw}K"),
        CameraPropertyId::FocusMode | CameraPropertyId::FocusArea => format!("AF 0x{raw:04X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ptp::MockPtpTransport;

    #[test]
    fn fujifilm_film_simulation_and_live_view() {
        let mock = MockPtpTransport::new();
        mock.push_ok(vec![
            100, 0, 0x0C, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 9, b'F', 0, b'U', 0, b'J', 0, b'I', 0, b'F', 0, b'I', 0, b'L', 0, b'M', 0,
            0, 0, 5, b'X', 0, b'-', 0, b'T', 0, b'5', 0, 0, 0, 2, b'3', 0, 0, 0, 2, b'F', 0, 0, 0,
        ]); // GetDeviceInfo
        mock.push_ok(Vec::new()); // OpenSession
        let mut film_desc = Vec::new();
        film_desc.extend_from_slice(&fuji_prop::FILM_SIMULATION.to_le_bytes());
        film_desc.extend_from_slice(&0x0004u16.to_le_bytes());
        film_desc.push(1);
        film_desc.extend_from_slice(&0x0006u16.to_le_bytes());
        film_desc.extend_from_slice(&0x0006u16.to_le_bytes());
        film_desc.push(2);
        film_desc.extend_from_slice(&2u16.to_le_bytes());
        film_desc.extend_from_slice(&0x0001u16.to_le_bytes());
        film_desc.extend_from_slice(&0x0006u16.to_le_bytes());
        mock.push_ok(film_desc);
        for _ in 0..5 {
            mock.push_ok(Vec::new());
        }
        mock.push_ok(Vec::new()); // Start live view
        mock.push_ok(vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x04, 0x38, 0x07, 0x80, 0x03, 0x01, 0x22,
            0x00, 0xFF, 0xD9,
        ]);
        mock.push_ok(Vec::new()); // Set film simulation
        mock.push_ok(Vec::new()); // Stop live view
        mock.push_ok(Vec::new()); // CloseSession

        let mut driver = FujifilmDriver::new(mock);
        let caps = driver.connect().unwrap();
        assert_eq!(caps.camera_model, "X-T5");
        assert_eq!(
            caps.properties[&CameraPropertyId::FilmSimulation]
                .current
                .label,
            "Classic Chrome"
        );
        driver.start_live_view().unwrap();
        let frame = driver.next_frame().unwrap();
        assert_eq!((frame.width, frame.height), (1920, 1080));
        driver
            .set_property(CameraPropertyId::FilmSimulation, 0x0001)
            .unwrap();
        assert_eq!(
            driver.capabilities().properties[&CameraPropertyId::FilmSimulation]
                .current
                .label,
            "PROVIA / Standard"
        );
        driver.disconnect().unwrap();
    }
}
