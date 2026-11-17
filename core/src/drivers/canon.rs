use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context;
use tracing::{debug, trace, warn};

use super::{
    extract_jpeg_slice, parse_jpeg_dimensions, unix_timestamp_us, CameraBrand, CameraCapabilities,
    CameraDriver, CameraPropertyId, LiveViewFrame, PropertyDescriptor, PropertyValue,
};
use crate::ptp::{
    response, ByteCursor, PtpError, PtpSession, PtpTransportBackend, UsbPtpTransport,
};

pub const CANON_VID: u16 = 0x04A9;

pub mod canon_opcode {
    pub const SET_DEVICE_PROP_VALUE_EX: u16 = 0x9110;
    pub const SET_REMOTE_MODE: u16 = 0x9114;
    pub const SET_EVENT_MODE: u16 = 0x9115;
    pub const GET_EVENT: u16 = 0x9116;
    pub const GET_VIEW_FINDER_DATA: u16 = 0x9153;
    pub const DO_AF: u16 = 0x9154;
    pub const DRIVE_LENS: u16 = 0x9155;
    pub const ZOOM_POSITION: u16 = 0x9159;
    pub const TOUCH_AF_POSITION: u16 = 0x915B;
    pub const AF_CANCEL: u16 = 0x9160;
}

pub mod canon_event {
    pub const PROP_VALUE_CHANGED: u32 = 0xC189;
    pub const AVAIL_LIST_CHANGED: u32 = 0xC18A;
}

pub mod canon_response {
    pub const NOT_READY: u16 = 0xA102;
}

pub mod canon_prop {
    pub const APERTURE: u32 = 0xD101;
    pub const SHUTTER_SPEED: u32 = 0xD102;
    pub const ISO: u32 = 0xD103;
    pub const EXPOSURE_COMPENSATION: u32 = 0xD104;
    pub const AUTO_EXPOSURE_MODE: u32 = 0xD105;
    pub const AF_MODE: u32 = 0xD108;
    pub const WHITE_BALANCE: u32 = 0xD109;
    pub const COLOR_TEMPERATURE: u32 = 0xD10A;
    pub const BATTERY_LEVEL: u32 = 0xD111;
    pub const LENS_NAME: u32 = 0xD11E;
    pub const AE_MODE_DIAL: u32 = 0xD138;
    pub const MOVIE_SERVO_AF: u32 = 0xD179;
    pub const LV_AF_SYSTEM: u32 = 0xD1BA;
    pub const CONTINUOUS_AF_MODE: u32 = 0xD1C9;
    pub const LENS_NAME_ALT: u32 = 0xD1D3;
    pub const LENS_NAME_EOS_R: u32 = 0xD1D8;
    pub const EVF_OUTPUT_DEVICE: u32 = 0xD1B0;
    pub const EVF_MODE: u32 = 0xD1B1;
}

/// Canon EOS USB PTP driver verified against the Canon EOS R7 (`0x04A9:0x32F7`).
pub struct CanonDriver<T: PtpTransportBackend = UsbPtpTransport> {
    session: PtpSession<T>,
    capabilities: CameraCapabilities,
    operations_supported: Vec<u16>,
    evf_mode_current: u32,
    evf_output_current: u32,
    evf_output_allowed: Vec<u32>,
    last_evf_out_target: u32,
    evf_coord_size: (u32, u32),
    exposure_mode_raw: Option<u32>,
    live_view_enabled: bool,
    af_drive_active: bool,
    frame_seq: u64,
}

impl CanonDriver<UsbPtpTransport> {
    pub fn open_usb(product_id: Option<u16>, serial: Option<&str>) -> anyhow::Result<Self> {
        let transport = UsbPtpTransport::open_matching(CANON_VID, product_id, serial)?;
        Ok(Self::new(transport))
    }
}

impl<T: PtpTransportBackend> CanonDriver<T> {
    pub fn new(transport: T) -> Self {
        Self {
            session: PtpSession::new(transport),
            capabilities: CameraCapabilities {
                brand: CameraBrand::Canon,
                camera_model: String::from("Canon EOS"),
                lens_model: String::from("Attached Lens"),
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
            operations_supported: Vec::new(),
            evf_mode_current: 0,
            evf_output_current: 0,
            evf_output_allowed: Vec::new(),
            last_evf_out_target: 2,
            evf_coord_size: (6960, 4640),
            exposure_mode_raw: None,
            live_view_enabled: false,
            af_drive_active: false,
            frame_seq: 0,
        }
    }

    fn set_canon_prop(&mut self, prop_code: u32, raw_value: u32) -> Result<(), PtpError> {
        self.cancel_active_af();
        let mut payload = Vec::with_capacity(12);
        payload.extend_from_slice(&12u32.to_le_bytes());
        payload.extend_from_slice(&prop_code.to_le_bytes());
        payload.extend_from_slice(&raw_value.to_le_bytes());

        for attempt in 0..8 {
            match self.session.transport.command_data_out(
                canon_opcode::SET_DEVICE_PROP_VALUE_EX,
                &[],
                &payload,
            ) {
                Ok(_) => return Ok(()),
                Err(PtpError::DeviceBusy)
                | Err(PtpError::ProtocolError {
                    code: response::DEVICE_BUSY,
                    ..
                }) if attempt < 7 => {
                    let _ = self.drain_events(2);
                    std::thread::sleep(Duration::from_millis(15));
                }
                Err(err) => return Err(err),
            }
        }
        Err(PtpError::DeviceBusy)
    }

    fn drain_events(
        &mut self,
        max_passes: usize,
    ) -> anyhow::Result<Vec<(CameraPropertyId, PropertyDescriptor)>> {
        let mut updated_props = Vec::new();

        for _pass in 0..max_passes {
            let (data, _) = self
                .session
                .transport
                .command_data_in(canon_opcode::GET_EVENT, &[])?;

            if data.len() < 8 {
                break;
            }

            if !self.parse_event_dataset(&data, &mut updated_props) {
                break;
            }
        }

        Ok(updated_props)
    }

    fn parse_event_dataset(
        &mut self,
        data: &[u8],
        updated_props: &mut Vec<(CameraPropertyId, PropertyDescriptor)>,
    ) -> bool {
        let mut offset = 0usize;
        let mut parsed_records = 0usize;

        while offset + 8 <= data.len() {
            let size = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            let event_type = u32::from_le_bytes([
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);

            if size < 8 || offset + size > data.len() {
                break;
            }
            if event_type == 0 {
                break;
            }

            parsed_records += 1;
            let record = &data[offset..offset + size];

            match event_type {
                canon_event::PROP_VALUE_CHANGED if record.len() >= 16 => {
                    let prop_code =
                        u32::from_le_bytes([record[8], record[9], record[10], record[11]]);
                    let raw_val =
                        u32::from_le_bytes([record[12], record[13], record[14], record[15]]);
                    self.apply_prop_value(prop_code, raw_val, &record[12..], updated_props);
                }
                canon_event::AVAIL_LIST_CHANGED if record.len() >= 20 => {
                    let prop_code =
                        u32::from_le_bytes([record[8], record[9], record[10], record[11]]);
                    let _data_type =
                        u32::from_le_bytes([record[12], record[13], record[14], record[15]]);
                    let count = u32::from_le_bytes([record[16], record[17], record[18], record[19]])
                        as usize;
                    let max_count = (record.len() - 20) / 4;
                    let safe_count = count.min(max_count);
                    let mut values = Vec::with_capacity(safe_count);
                    for i in 0..safe_count {
                        let base = 20 + i * 4;
                        values.push(u32::from_le_bytes([
                            record[base],
                            record[base + 1],
                            record[base + 2],
                            record[base + 3],
                        ]));
                    }
                    self.apply_avail_list(prop_code, &values, updated_props);
                }
                _ => {
                    trace!(
                        event_type = format_args!("0x{event_type:04X}"),
                        size,
                        "unhandled Canon EOS event record"
                    );
                }
            }

            offset += size;
        }

        parsed_records > 0
    }

    fn apply_prop_value(
        &mut self,
        prop_code: u32,
        raw_val: u32,
        value_bytes: &[u8],
        updated_props: &mut Vec<(CameraPropertyId, PropertyDescriptor)>,
    ) {
        if prop_code == canon_prop::BATTERY_LEVEL {
            let pct = match raw_val {
                0 => Some(0),
                1 | 4 => Some(25),
                2 => Some(50),
                5 => Some(75),
                3 => Some(100),
                6..=100 => Some(raw_val as u8),
                _ => None,
            };
            if let Some(p) = pct {
                self.capabilities.battery_percent = Some(p);
            }
            return;
        }

        if prop_code == canon_prop::EVF_MODE {
            self.evf_mode_current = raw_val;
            return;
        }

        if prop_code == canon_prop::EVF_OUTPUT_DEVICE {
            self.evf_output_current = raw_val;
            return;
        }

        if prop_code == canon_prop::AUTO_EXPOSURE_MODE || prop_code == canon_prop::AE_MODE_DIAL {
            self.exposure_mode_raw = Some(raw_val);
            self.capabilities.exposure_mode = format_canon_ae_mode(raw_val);
            self.capabilities.manual_mode_active = matches!(raw_val, 3 | 0x14);
            self.update_exposure_comp_writability();
            return;
        }

        if prop_code == canon_prop::LENS_NAME
            || prop_code == canon_prop::LENS_NAME_ALT
            || prop_code == canon_prop::LENS_NAME_EOS_R
        {
            if let Some(name) = parse_canon_ascii_string(value_bytes) {
                if !name.is_empty() {
                    self.capabilities.lens_model = name;
                }
            }
            return;
        }

        let Some(prop_id) = map_canon_prop_id(prop_code) else {
            return;
        };

        let labeled = PropertyValue {
            raw: raw_val,
            label: format_canon_value(prop_id, raw_val),
        };

        let desc = self
            .capabilities
            .properties
            .entry(prop_id)
            .or_insert_with(|| PropertyDescriptor {
                id: prop_id,
                raw_code: prop_code,
                current: labeled.clone(),
                writable: false,
                allowed_values: Vec::new(),
            });
        desc.raw_code = prop_code;
        desc.current = labeled;

        // If ColorTemperature isn't returned in an AvailListChanged record, allow standard Kelvin range.
        if prop_id == CameraPropertyId::ColorTemperatureKelvin && desc.allowed_values.is_empty() {
            desc.writable = true;
            desc.allowed_values = (2500..=10000)
                .step_by(100)
                .map(|k| PropertyValue {
                    raw: k,
                    label: format!("{k}K"),
                })
                .collect();
        }

        self.update_exposure_comp_writability();
        if let Some(updated_desc) = self.capabilities.properties.get(&prop_id) {
            updated_props.push((prop_id, updated_desc.clone()));
        }
    }

    fn apply_avail_list(
        &mut self,
        prop_code: u32,
        values: &[u32],
        updated_props: &mut Vec<(CameraPropertyId, PropertyDescriptor)>,
    ) {
        if prop_code == canon_prop::EVF_OUTPUT_DEVICE {
            self.evf_output_allowed = values.to_vec();
            return;
        }

        let Some(prop_id) = map_canon_prop_id(prop_code) else {
            return;
        };

        let filtered_raws: Vec<u32> = if prop_id == CameraPropertyId::ShutterSpeed {
            let fast: Vec<u32> = values.iter().copied().filter(|&r| r >= 0x58).collect();
            if fast.is_empty() {
                values.to_vec()
            } else {
                fast
            }
        } else {
            values.to_vec()
        };

        let allowed_values: Vec<PropertyValue> = filtered_raws
            .iter()
            .map(|&raw| PropertyValue {
                raw,
                label: format_canon_value(prop_id, raw),
            })
            .collect();

        let desc = self
            .capabilities
            .properties
            .entry(prop_id)
            .or_insert_with(|| PropertyDescriptor {
                id: prop_id,
                raw_code: prop_code,
                current: allowed_values.first().cloned().unwrap_or(PropertyValue {
                    raw: 0,
                    label: "—".into(),
                }),
                writable: !allowed_values.is_empty(),
                allowed_values: Vec::new(),
            });

        desc.raw_code = prop_code;
        desc.writable = !allowed_values.is_empty();
        desc.allowed_values = allowed_values;

        // Keep current label synced with any updated formatting.
        desc.current.label = format_canon_value(prop_id, desc.current.raw);
        self.update_exposure_comp_writability();
        if let Some(updated_desc) = self.capabilities.properties.get(&prop_id) {
            updated_props.push((prop_id, updated_desc.clone()));
        }
    }

    fn update_exposure_comp_writability(&mut self) {
        let iso_is_fixed = self
            .capabilities
            .properties
            .get(&CameraPropertyId::Iso)
            .is_some_and(|d| d.current.raw != 0 && d.current.raw != 1);
        let in_manual = self.capabilities.manual_mode_active;
        let allow_ev = !in_manual || !iso_is_fixed;
        if let Some(ev) = self
            .capabilities
            .properties
            .get_mut(&CameraPropertyId::ExposureCompensation)
        {
            ev.writable = !ev.allowed_values.is_empty() && allow_ev;
        }
    }

    fn is_op_supported(&self, opcode: u16) -> bool {
        self.operations_supported.contains(&opcode)
    }

    fn cancel_active_af(&mut self) {
        if self.af_drive_active {
            let _ = self
                .session
                .transport
                .command_nodata(canon_opcode::AF_CANCEL, &[]);
            self.af_drive_active = false;
        }
    }

    fn ensure_focus_area(&mut self, from_raws: &[u32], target_raw: u32, drain_passes: usize) {
        let should_switch = self
            .capabilities
            .properties
            .get(&CameraPropertyId::FocusArea)
            .is_some_and(|area| {
                area.writable
                    && from_raws.contains(&area.current.raw)
                    && area.allowed_values.iter().any(|v| v.raw == target_raw)
            });

        if should_switch
            && self
                .set_canon_prop(canon_prop::LV_AF_SYSTEM, target_raw)
                .is_ok()
        {
            if let Some(desc) = self
                .capabilities
                .properties
                .get_mut(&CameraPropertyId::FocusArea)
            {
                desc.current = PropertyValue {
                    raw: target_raw,
                    label: format_canon_value(CameraPropertyId::FocusArea, target_raw),
                };
            }
            let _ = self.drain_events(drain_passes);
        }
    }
}

impl<T: PtpTransportBackend> CameraDriver for CanonDriver<T> {
    fn brand(&self) -> CameraBrand {
        CameraBrand::Canon
    }

    fn connect(&mut self) -> anyhow::Result<CameraCapabilities> {
        let info = match self.session.get_device_info() {
            Ok(info) => info,
            Err(first_err) => {
                warn!(
                    ?first_err,
                    "GetDeviceInfo failed; resetting USB PTP pipe and retrying"
                );
                self.session.transport.reset_pipe();
                std::thread::sleep(Duration::from_millis(80));
                self.session.get_device_info()?
            }
        };
        if !info.model.is_empty() {
            self.capabilities.camera_model = info.model.clone();
        }
        self.capabilities.firmware_version = info.device_version.clone();
        self.capabilities.serial_number = info.serial_number.clone();
        self.operations_supported = info.operations_supported.clone();
        self.capabilities.supports_touch_autofocus =
            self.is_op_supported(canon_opcode::TOUCH_AF_POSITION);
        self.capabilities.supports_autofocus =
            self.operations_supported.is_empty() || self.is_op_supported(canon_opcode::DO_AF);
        self.capabilities.supports_manual_focus_drive =
            self.operations_supported.is_empty() || self.is_op_supported(canon_opcode::DRIVE_LENS);

        self.session.open_session(1)?;

        // Enter EOS PC Remote Mode + Event Mode so property tables and EVF become accessible.
        self.session
            .transport
            .command_nodata(canon_opcode::SET_REMOTE_MODE, &[1])
            .context("Canon SetRemoteMode(1) failed")?;
        self.session
            .transport
            .command_nodata(canon_opcode::SET_EVENT_MODE, &[1])
            .context("Canon SetEventMode(1) failed")?;

        let _ = self.drain_events(6)?;
        self.update_exposure_comp_writability();

        debug!(
            model = %self.capabilities.camera_model,
            lens = %self.capabilities.lens_model,
            mode = %self.capabilities.exposure_mode,
            props = self.capabilities.properties.len(),
            "connected Canon EOS session"
        );
        Ok(self.capabilities.clone())
    }

    fn start_live_view(&mut self) -> anyhow::Result<()> {
        if self.live_view_enabled {
            return Ok(());
        }
        // Enable EVF subsystem first and drain events so the camera populates 0xD1B0 options.
        if self.evf_mode_current != 1 {
            self.set_canon_prop(canon_prop::EVF_MODE, 1)
                .context("failed to enable Canon EvfMode")?;
            std::thread::sleep(Duration::from_millis(30));
            let _ = self.drain_events(4);
        }

        let evf_out = if self.evf_output_allowed.is_empty() || self.evf_output_allowed.contains(&2)
        {
            2
        } else if self.evf_output_allowed.contains(&3) {
            3
        } else {
            let combined = self.evf_output_current | 2;
            if self.evf_output_allowed.contains(&combined) {
                combined
            } else {
                2
            }
        };
        self.last_evf_out_target = evf_out;

        self.set_canon_prop(canon_prop::EVF_OUTPUT_DEVICE, evf_out)
            .context("failed to route Canon EvfOutputDevice to PC")?;

        // Drain events across warm-up passes so the EVF scaler finishes switching to USB.
        std::thread::sleep(Duration::from_millis(30));
        let _ = self.drain_events(6);

        // Upgrade tiny 1-Point / Spot AF to Whole Area AF (0x0E) for compatibility when available.
        if let Some(area) = self
            .capabilities
            .properties
            .get(&CameraPropertyId::FocusArea)
            .cloned()
        {
            if area.writable && matches!(area.current.raw, 1 | 10) {
                let target_area = if area.allowed_values.iter().any(|v| v.raw == 14) {
                    Some(14)
                } else if area.allowed_values.iter().any(|v| v.raw == 2) {
                    Some(2)
                } else {
                    None
                };
                if let Some(target) = target_area {
                    let _ = self.set_property(CameraPropertyId::FocusArea, target);
                }
            }
        }

        // Bump slow multi-second/sub-1/30 shutter speeds in Manual mode to 1/60 for smooth streaming.
        if self.capabilities.manual_mode_active {
            if let Some(ss) = self
                .capabilities
                .properties
                .get(&CameraPropertyId::ShutterSpeed)
                .cloned()
            {
                if ss.writable && ss.current.raw > 0 && ss.current.raw < 0x60 {
                    let target_ss = ss
                        .allowed_values
                        .iter()
                        .find(|v| v.raw == 0x68)
                        .or_else(|| ss.allowed_values.iter().find(|v| v.raw >= 0x60))
                        .map(|v| v.raw);
                    if let Some(target) = target_ss {
                        let _ = self.set_property(CameraPropertyId::ShutterSpeed, target);
                    }
                }
            }
        }

        self.live_view_enabled = true;
        Ok(())
    }

    fn stop_live_view(&mut self) -> anyhow::Result<()> {
        self.cancel_active_af();
        if !self.live_view_enabled {
            return Ok(());
        }
        let _ = self.set_canon_prop(canon_prop::EVF_OUTPUT_DEVICE, 0);
        let _ = self.set_canon_prop(canon_prop::EVF_MODE, 0);
        self.live_view_enabled = false;
        Ok(())
    }

    fn next_frame(&mut self) -> anyhow::Result<LiveViewFrame> {
        // Canon EOS R bodies return 0xA102 (NotReady) or 0x2019 (DeviceBusy) while EVF warms up
        // or between sensor frame ticks.
        let mut last_err = None;
        let mut not_ready_count = 0u32;
        let mut busy_count = 0u32;

        for attempt in 0..120 {
            let xfer_start = Instant::now();
            match self
                .session
                .transport
                .command_data_in(canon_opcode::GET_VIEW_FINDER_DATA, &[0x0020_0000])
            {
                Ok((payload, _)) => {
                    let usb_transfer_ms = xfer_start.elapsed().as_secs_f32() * 1000.0;
                    if let Some((cw, ch)) = extract_canon_evf_coord_size(&payload) {
                        self.evf_coord_size = (cw, ch);
                    }
                    if let Some(jpeg) = extract_canon_evf_jpeg(&payload) {
                        self.frame_seq += 1;
                        let (width, height) = parse_jpeg_dimensions(jpeg);
                        return Ok(LiveViewFrame {
                            jpeg_bytes: jpeg.to_vec(),
                            raw_pixels: Vec::new(),
                            pixel_format: super::FramePixelFormat::Jpeg,
                            sequence: self.frame_seq,
                            timestamp_us: unix_timestamp_us(),
                            width,
                            height,
                            usb_transfer_ms,
                        });
                    }
                    last_err = Some(anyhow::anyhow!(
                        "Canon GetViewFinderData returned {} bytes without JPEG payload",
                        payload.len()
                    ));
                    std::thread::sleep(Duration::from_millis(4));
                }
                Err(PtpError::DeviceBusy)
                | Err(PtpError::ProtocolError {
                    code: response::DEVICE_BUSY,
                    ..
                }) => {
                    busy_count += 1;
                    if self.af_drive_active && busy_count > 6 {
                        self.cancel_active_af();
                    }
                    let wait_ms = if self.frame_seq > 0 {
                        2
                    } else if attempt < 8 {
                        10
                    } else {
                        15
                    };
                    std::thread::sleep(Duration::from_millis(wait_ms));
                }
                Err(PtpError::ProtocolError {
                    code: canon_response::NOT_READY,
                    ..
                }) => {
                    not_ready_count += 1;
                    if self.af_drive_active && not_ready_count > 6 {
                        self.cancel_active_af();
                    }
                    if self.frame_seq > 0 {
                        if not_ready_count % 20 == 0 {
                            // Self-recover EVF routing if a focus operation paused EVF output.
                            let _ = self.set_canon_prop(canon_prop::EVF_MODE, 1);
                            let _ = self.set_canon_prop(
                                canon_prop::EVF_OUTPUT_DEVICE,
                                self.last_evf_out_target,
                            );
                            let _ = self.drain_events(2);
                        } else if not_ready_count % 5 == 0 {
                            let _ = self.drain_events(1);
                        }
                        std::thread::sleep(Duration::from_millis(4));
                        continue;
                    }
                    debug!(
                        attempt,
                        not_ready_count,
                        evf_mode = self.evf_mode_current,
                        evf_out = self.evf_output_current,
                        "Canon GetViewFinderData NotReady (0xA102) during initial warm-up"
                    );
                    let _ = self.drain_events(1);
                    if not_ready_count == 15 {
                        let _ = self.set_canon_prop(canon_prop::EVF_MODE, 1);
                        let alt_out = if self.last_evf_out_target == 2 { 3 } else { 2 };
                        self.last_evf_out_target = alt_out;
                        let _ = self.set_canon_prop(canon_prop::EVF_OUTPUT_DEVICE, alt_out);
                        let _ = self.drain_events(2);
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
                Err(e) => return Err(e.into()),
            }
        }

        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!("Canon GetViewFinderData timed out waiting past DeviceBusy / NotReady")
        }))
    }

    fn set_property(&mut self, prop: CameraPropertyId, raw_value: u32) -> anyhow::Result<()> {
        let prop_code = self
            .capabilities
            .properties
            .get(&prop)
            .map(|d| d.raw_code)
            .or_else(|| reverse_canon_prop_code(prop))
            .ok_or_else(|| anyhow::anyhow!("unsupported Canon property {prop:?}"))?;

        self.cancel_active_af();
        self.set_canon_prop(prop_code, raw_value)?;

        if prop == CameraPropertyId::FocusMode {
            if raw_value == 1 {
                let _ = self.set_canon_prop(canon_prop::CONTINUOUS_AF_MODE, 1);
                let _ = self.set_canon_prop(canon_prop::MOVIE_SERVO_AF, 1);
                if let Some(area) = self
                    .capabilities
                    .properties
                    .get(&CameraPropertyId::FocusArea)
                    .cloned()
                {
                    if area.writable && matches!(area.current.raw, 1 | 10) {
                        if let Some(target) = area
                            .allowed_values
                            .iter()
                            .find(|v| v.raw == 14)
                            .map(|v| v.raw)
                        {
                            let _ = self.set_canon_prop(canon_prop::LV_AF_SYSTEM, target);
                        }
                    }
                }
                if self.live_view_enabled && self.capabilities.supports_autofocus {
                    // Pulse DoAf briefly and cancel so exposure dials never remain locked (0x2019).
                    if self
                        .session
                        .transport
                        .command_nodata(canon_opcode::DO_AF, &[])
                        .is_ok()
                    {
                        self.af_drive_active = true;
                        for _ in 0..3 {
                            std::thread::sleep(Duration::from_millis(50));
                            let _ = self.drain_events(1);
                        }
                        self.cancel_active_af();
                    }
                }
            } else {
                let _ = self.set_canon_prop(canon_prop::CONTINUOUS_AF_MODE, 0);
                let _ = self.set_canon_prop(canon_prop::MOVIE_SERVO_AF, 0);
            }
        }

        if prop == CameraPropertyId::FocusArea
            && raw_value == 14
            && self.capabilities.supports_touch_autofocus
        {
            let _ = self
                .session
                .transport
                .command_nodata(canon_opcode::TOUCH_AF_POSITION, &[0, 0, 0]);
        }

        if let Some(desc) = self.capabilities.properties.get_mut(&prop) {
            desc.current = PropertyValue {
                raw: raw_value,
                label: format_canon_value(prop, raw_value),
            };
        }
        self.update_exposure_comp_writability();
        let _ = self.drain_events(3);
        Ok(())
    }

    fn poll_events(&mut self) -> anyhow::Result<Vec<(CameraPropertyId, PropertyDescriptor)>> {
        self.drain_events(2)
    }

    fn trigger_autofocus(
        &mut self,
        norm_x: Option<f32>,
        norm_y: Option<f32>,
    ) -> anyhow::Result<()> {
        self.cancel_active_af();

        if let (Some(nx), Some(ny)) = (norm_x, norm_y) {
            self.ensure_focus_area(&[14, 2], 1, 2);

            let px = (nx.clamp(0.05, 0.95) * self.evf_coord_size.0.max(1) as f32).round() as u32;
            let py = (ny.clamp(0.05, 0.95) * self.evf_coord_size.1.max(1) as f32).round() as u32;
            if self.capabilities.supports_touch_autofocus {
                if let Err(err) = self
                    .session
                    .transport
                    .command_nodata(canon_opcode::TOUCH_AF_POSITION, &[3, px, py])
                {
                    debug!(?err, px, py, "Canon TouchAfPosition returned non-ok");
                }
            }
            if self.is_op_supported(canon_opcode::ZOOM_POSITION) {
                let _ = self
                    .session
                    .transport
                    .command_nodata(canon_opcode::ZOOM_POSITION, &[px, py]);
            }
            #[cfg(test)]
            std::thread::sleep(Duration::from_millis(1));
            #[cfg(not(test))]
            std::thread::sleep(Duration::from_millis(25));
            let _ = self.drain_events(1);
        } else {
            if self.capabilities.supports_touch_autofocus {
                let _ = self
                    .session
                    .transport
                    .command_nodata(canon_opcode::TOUCH_AF_POSITION, &[0, 0, 0]);
            }
            self.ensure_focus_area(&[1, 5, 6, 10, 11, 12, 13], 14, 1);
        }

        match self
            .session
            .transport
            .command_nodata(canon_opcode::DO_AF, &[])
        {
            Ok(_) => {
                self.af_drive_active = true;
            }
            Err(PtpError::ProtocolError {
                code: response::PARAMETER_NOT_SUPPORTED,
                ..
            }) => {
                self.session
                    .transport
                    .command_nodata(canon_opcode::DO_AF, &[1])?;
                self.af_drive_active = true;
            }
            Err(PtpError::DeviceBusy) => {
                debug!("Canon DoAf skipped because AF subsystem is busy");
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        }

        // Allow Dual Pixel CMOS AF II sufficient travel (~360 ms) before releasing half-press.
        for _ in 0..6 {
            #[cfg(test)]
            std::thread::sleep(Duration::from_millis(1));
            #[cfg(not(test))]
            std::thread::sleep(Duration::from_millis(60));
            let _ = self.drain_events(1);
        }

        self.cancel_active_af();
        let _ = self.drain_events(1);
        Ok(())
    }

    fn drive_manual_focus(&mut self, step: i32) -> anyhow::Result<()> {
        if step == 0 {
            return Ok(());
        }
        self.cancel_active_af();
        let mag = step.unsigned_abs().clamp(1, 3);
        let code = if step > 0 { 0x8000 | mag } else { mag };
        self.session
            .transport
            .command_nodata(canon_opcode::DRIVE_LENS, &[code])?;
        Ok(())
    }

    fn capabilities(&self) -> &CameraCapabilities {
        &self.capabilities
    }

    fn disconnect(&mut self) -> anyhow::Result<()> {
        self.cancel_active_af();
        if self.live_view_enabled {
            let _ = self.stop_live_view();
        }
        if self.session.is_open() {
            // Never send SetRemoteMode(0) on EOS R bodies, which wedges their USB PTP responder.
            if let Err(err) = self
                .session
                .transport
                .command_nodata(canon_opcode::SET_EVENT_MODE, &[0])
            {
                warn!(?err, "Canon SetEventMode(0) failed during disconnect");
                self.session.transport.reset_pipe();
            }
            if let Err(err) = self.session.close_session() {
                warn!(?err, "Canon CloseSession failed during disconnect");
                self.session.transport.reset_pipe();
            }
        }
        Ok(())
    }
}

impl<T: PtpTransportBackend> Drop for CanonDriver<T> {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}

/// Extracts the EOS Live View coordinate space `(width, height)` from Canon EVF record `0x0E`.
fn extract_canon_evf_coord_size(payload: &[u8]) -> Option<(u32, u32)> {
    if payload.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut cursor = ByteCursor::new(payload);
    while cursor.remaining() >= 8 {
        let start_pos = payload.len() - cursor.remaining();
        let Ok(rec_size) = cursor.read_u32() else {
            break;
        };
        let Ok(rec_type) = cursor.read_u32() else {
            break;
        };
        let rec_len = rec_size as usize;
        if rec_len < 8 || start_pos + rec_len > payload.len() {
            break;
        }
        if rec_type == 0x0000_000E && rec_len >= 16 {
            let mut inner = ByteCursor::new(&payload[start_pos + 8..start_pos + rec_len]);
            if let (Ok(cw), Ok(ch)) = (inner.read_u32(), inner.read_u32()) {
                if (640..=20_000).contains(&cw) && (480..=20_000).contains(&ch) {
                    return Some((cw, ch));
                }
            }
        }
        cursor = ByteCursor::new(&payload[start_pos + rec_len..]);
    }
    None
}

/// Extracts JPEG bytes from either raw Canon EVF data or multi-record Canon EVF datasets.
pub fn extract_canon_evf_jpeg(payload: &[u8]) -> Option<&[u8]> {
    if payload.starts_with(&[0xFF, 0xD8]) {
        return extract_jpeg_slice(payload);
    }
    // Canon EOS R bodies often package EVF frames as [size: u32, type: u32, ...] blocks where
    // record type `0x00000001` carries the JPEG bitstream right after the 8-byte record header.
    let mut cursor = ByteCursor::new(payload);
    while cursor.remaining() >= 8 {
        let start_pos = payload.len() - cursor.remaining();
        let Ok(rec_size) = cursor.read_u32() else {
            break;
        };
        let Ok(rec_type) = cursor.read_u32() else {
            break;
        };
        let rec_len = rec_size as usize;
        if rec_len < 8 || start_pos + rec_len > payload.len() {
            break;
        }
        if rec_type == 1 {
            if let Some(jpeg) = extract_jpeg_slice(&payload[start_pos + 8..start_pos + rec_len]) {
                return Some(jpeg);
            }
        }
        let remaining_in_rec = rec_len - 8;
        let next_start = start_pos + rec_len;
        let _ = remaining_in_rec;
        cursor = ByteCursor::new(&payload[next_start..]);
        if let Some(jpeg) = extract_jpeg_slice(&payload[start_pos..start_pos + rec_len]) {
            return Some(jpeg);
        }
    }
    extract_jpeg_slice(payload)
}

fn parse_canon_ascii_string(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let s = std::str::from_utf8(&bytes[..end]).ok()?.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_graphic() || b == b' ') {
        return None;
    }
    Some(s.to_string())
}

fn format_canon_ae_mode(raw: u32) -> String {
    match raw {
        0 => "P (Program)",
        1 => "Tv (Shutter Priority)",
        2 => "Av (Aperture Priority)",
        3 => "M (Manual)",
        4 => "Bulb",
        5 => "A-DEP",
        6 => "DEP",
        7 => "C1 (Custom)",
        8 => "Lock",
        9 => "Auto",
        0x0A => "Night Portrait",
        0x0B => "Sports",
        0x0C => "Portrait",
        0x0D => "Landscape",
        0x0E => "Close-Up",
        0x0F => "Flash Off",
        0x10 => "C2 (Custom)",
        0x11 => "C3 (Custom)",
        0x12 => "Creative Auto",
        0x13 => "Movie",
        0x14 => "Manual Movie",
        0x15 => "Scene Intelligent Auto",
        0x16 => "Fv (Flexible Priority)",
        _ => return format!("Mode 0x{raw:02X}"),
    }
    .to_string()
}

fn map_canon_prop_id(prop_code: u32) -> Option<CameraPropertyId> {
    match prop_code {
        canon_prop::APERTURE => Some(CameraPropertyId::Aperture),
        canon_prop::SHUTTER_SPEED => Some(CameraPropertyId::ShutterSpeed),
        canon_prop::ISO => Some(CameraPropertyId::Iso),
        canon_prop::EXPOSURE_COMPENSATION => Some(CameraPropertyId::ExposureCompensation),
        canon_prop::WHITE_BALANCE => Some(CameraPropertyId::WhiteBalanceMode),
        canon_prop::COLOR_TEMPERATURE => Some(CameraPropertyId::ColorTemperatureKelvin),
        canon_prop::AF_MODE => Some(CameraPropertyId::FocusMode),
        canon_prop::LV_AF_SYSTEM => Some(CameraPropertyId::FocusArea),
        _ => None,
    }
}

fn reverse_canon_prop_code(id: CameraPropertyId) -> Option<u32> {
    match id {
        CameraPropertyId::Aperture => Some(canon_prop::APERTURE),
        CameraPropertyId::ShutterSpeed => Some(canon_prop::SHUTTER_SPEED),
        CameraPropertyId::Iso => Some(canon_prop::ISO),
        CameraPropertyId::ExposureCompensation => Some(canon_prop::EXPOSURE_COMPENSATION),
        CameraPropertyId::WhiteBalanceMode => Some(canon_prop::WHITE_BALANCE),
        CameraPropertyId::ColorTemperatureKelvin => Some(canon_prop::COLOR_TEMPERATURE),
        CameraPropertyId::FocusMode => Some(canon_prop::AF_MODE),
        CameraPropertyId::FocusArea => Some(canon_prop::LV_AF_SYSTEM),
        CameraPropertyId::FilmSimulation => None,
    }
}

/// Translates Canon EOS APEX hex property codes into human-readable labels.
pub fn format_canon_value(prop: CameraPropertyId, raw: u32) -> String {
    match prop {
        CameraPropertyId::Aperture => match raw {
            0x08 => "f/1.0",
            0x0B => "f/1.1",
            0x0C | 0x0D => "f/1.2",
            0x10 => "f/1.4",
            0x13 => "f/1.6",
            0x14 | 0x15 => "f/1.8",
            0x18 => "f/2.0",
            0x1B => "f/2.2",
            0x1C | 0x1D => "f/2.5",
            0x20 => "f/2.8",
            0x23 => "f/3.2",
            0x24 | 0x25 => "f/3.5",
            0x28 => "f/4.0",
            0x2B => "f/4.5",
            0x2C | 0x2D => "f/5.0",
            0x30 => "f/5.6",
            0x33 => "f/6.3",
            0x34 | 0x35 => "f/7.1",
            0x38 => "f/8.0",
            0x3B => "f/9.0",
            0x3C | 0x3D => "f/10",
            0x40 => "f/11",
            0x43 => "f/13",
            0x44 | 0x45 => "f/14",
            0x48 => "f/16",
            0x4B => "f/18",
            0x4C | 0x4D => "f/20",
            0x50 => "f/22",
            0x53 => "f/25",
            0x54 | 0x55 => "f/29",
            0x58 => "f/32",
            0x00 | 0xFFFFFFFF => "Auto",
            _ => return format!("0x{raw:02X}"),
        }
        .to_string(),
        CameraPropertyId::ShutterSpeed => match raw {
            0x00 | 0xFFFFFFFF => "Auto",
            0x0C => "Bulb",
            0x10 => "30\"",
            0x13 => "25\"",
            0x14 | 0x15 => "20\"",
            0x18 => "15\"",
            0x1B => "13\"",
            0x1C | 0x1D => "10\"",
            0x20 => "8\"",
            0x23 => "6\"",
            0x24 | 0x25 => "5\"",
            0x28 => "4\"",
            0x2B => "3\"2",
            0x2C | 0x2D => "2\"5",
            0x30 => "2\"",
            0x33 => "1\"6",
            0x34 | 0x35 => "1\"3",
            0x38 => "1\"",
            0x3B => "0\"8",
            0x3C | 0x3D => "0\"6",
            0x40 => "1/2",
            0x43 => "0\"4",
            0x44 | 0x45 => "0\"3",
            0x48 => "1/4",
            0x4B => "1/5",
            0x4C | 0x4D => "1/6",
            0x50 => "1/8",
            0x53 | 0x54 => "1/10",
            0x55 => "1/13",
            0x58 => "1/15",
            0x5B => "1/20",
            0x5C | 0x5D => "1/25",
            0x60 => "1/30",
            0x63 => "1/40",
            0x64 => "1/45",
            0x65 => "1/50",
            0x68 => "1/60",
            0x6B => "1/80",
            0x6C => "1/90",
            0x6D => "1/100",
            0x70 => "1/125",
            0x73 => "1/160",
            0x74 | 0x75 => "1/200",
            0x78 => "1/250",
            0x7B => "1/320",
            0x7C | 0x7D => "1/400",
            0x80 => "1/500",
            0x83 => "1/640",
            0x84 | 0x85 => "1/800",
            0x88 => "1/1000",
            0x8B => "1/1250",
            0x8C | 0x8D => "1/1600",
            0x90 => "1/2000",
            0x93 => "1/2500",
            0x94 | 0x95 => "1/3200",
            0x98 => "1/4000",
            0x9B => "1/5000",
            0x9C | 0x9D => "1/6400",
            0xA0 => "1/8000",
            0xA3 => "1/10000",
            0xA4 | 0xA5 => "1/12800",
            0xA8 => "1/16000",
            _ => return format!("0x{raw:02X}"),
        }
        .to_string(),
        CameraPropertyId::Iso => match raw {
            0x00 | 0xFFFFFFFF => "Auto",
            0x40 => "ISO 50",
            0x48 => "ISO 100",
            0x4B => "ISO 125",
            0x4D => "ISO 160",
            0x50 => "ISO 200",
            0x53 => "ISO 250",
            0x55 => "ISO 320",
            0x58 => "ISO 400",
            0x5B => "ISO 500",
            0x5D => "ISO 640",
            0x60 => "ISO 800",
            0x63 => "ISO 1000",
            0x65 => "ISO 1250",
            0x68 => "ISO 1600",
            0x6B => "ISO 2000",
            0x6D => "ISO 2500",
            0x70 => "ISO 3200",
            0x73 => "ISO 4000",
            0x75 => "ISO 5000",
            0x78 => "ISO 6400",
            0x7B => "ISO 8000",
            0x7D => "ISO 10000",
            0x80 => "ISO 12800",
            0x83 => "ISO 16000",
            0x85 => "ISO 20000",
            0x88 => "ISO 25600",
            0x8B => "ISO 32000",
            0x8D => "ISO 40000",
            0x90 => "ISO 51200",
            0x98 => "ISO 102400",
            _ => return format!("0x{raw:02X}"),
        }
        .to_string(),
        CameraPropertyId::ExposureCompensation => match raw {
            0x00 => "0.0 EV",
            0x03 => "+0.3 EV",
            0x05 => "+0.7 EV",
            0x08 => "+1.0 EV",
            0x0B => "+1.3 EV",
            0x0D => "+1.7 EV",
            0x10 => "+2.0 EV",
            0x13 => "+2.3 EV",
            0x15 => "+2.7 EV",
            0x18 => "+3.0 EV",
            0xFD => "-0.3 EV",
            0xFB => "-0.7 EV",
            0xF8 => "-1.0 EV",
            0xF5 => "-1.3 EV",
            0xF3 => "-1.7 EV",
            0xF0 => "-2.0 EV",
            0xED => "-2.3 EV",
            0xEB => "-2.7 EV",
            0xE8 => "-3.0 EV",
            _ => return format!("0x{raw:02X}"),
        }
        .to_string(),
        CameraPropertyId::WhiteBalanceMode => match raw {
            0 => "Auto (Ambience)",
            17 | 23 => "Auto (White)",
            1 => "Daylight",
            8 => "Shade",
            2 => "Cloudy",
            3 => "Tungsten",
            4 => "Fluorescent",
            5 => "Flash",
            6 => "Custom",
            9 => "Kelvin",
            _ => return format!("WB {raw}"),
        }
        .to_string(),
        CameraPropertyId::ColorTemperatureKelvin => format!("{raw}K"),
        CameraPropertyId::FocusMode => match raw {
            0 => "One-Shot AF",
            1 => "AF-C (Servo AF)",
            2 => "AI Focus AF",
            3 => "Manual Focus",
            _ => return format!("AF {raw}"),
        }
        .to_string(),
        CameraPropertyId::FocusArea => match raw {
            0 => "FlexiZone Multi",
            1 => "1-Point AF",
            2 => "Face + Tracking",
            3 => "FlexiZone Single",
            4 => "Zone AF",
            5 => "Expand AF Area",
            6 => "Expand AF Area: Around",
            10 => "Spot AF",
            11 => "Flexible Zone 1",
            12 => "Flexible Zone 2",
            13 => "Flexible Zone 3",
            14 => "Whole Area AF",
            _ => return format!("AF Area {raw}"),
        }
        .to_string(),
        CameraPropertyId::FilmSimulation => format!("0x{raw:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ptp::MockPtpTransport;

    fn build_device_info_payload(model: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&100u16.to_le_bytes());
        buf.extend_from_slice(&0x0000_000Bu32.to_le_bytes());
        buf.extend_from_slice(&100u16.to_le_bytes());
        buf.push(0);
        buf.extend_from_slice(&0u16.to_le_bytes());
        // operations_supported: DoAf + DriveLens + TouchAfPosition + AfCancel
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(&canon_opcode::DO_AF.to_le_bytes());
        buf.extend_from_slice(&canon_opcode::DRIVE_LENS.to_le_bytes());
        buf.extend_from_slice(&canon_opcode::TOUCH_AF_POSITION.to_le_bytes());
        buf.extend_from_slice(&canon_opcode::AF_CANCEL.to_le_bytes());
        for _ in 0..4 {
            buf.extend_from_slice(&0u32.to_le_bytes());
        }
        for s in ["Canon Inc.", model, "1.4.0", "5abc0545"] {
            let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
            buf.push(units.len() as u8);
            for u in units {
                buf.extend_from_slice(&u.to_le_bytes());
            }
        }
        buf
    }

    fn build_canon_event_payload() -> Vec<u8> {
        let mut out = Vec::new();
        // PropValueChanged: ISO = 0x60 (800)
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&canon_event::PROP_VALUE_CHANGED.to_le_bytes());
        out.extend_from_slice(&canon_prop::ISO.to_le_bytes());
        out.extend_from_slice(&0x60u32.to_le_bytes());

        // AvailListChanged: ISO = [0x48 (100), 0x58 (400), 0x60 (800)]
        out.extend_from_slice(&32u32.to_le_bytes());
        out.extend_from_slice(&canon_event::AVAIL_LIST_CHANGED.to_le_bytes());
        out.extend_from_slice(&canon_prop::ISO.to_le_bytes());
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&0x48u32.to_le_bytes());
        out.extend_from_slice(&0x58u32.to_le_bytes());
        out.extend_from_slice(&0x60u32.to_le_bytes());

        // Sentinel record
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    #[test]
    fn canon_connect_live_view_property_and_clean_disconnect() {
        let mock = MockPtpTransport::new();
        mock.push_ok(build_device_info_payload("Canon EOS R7")); // GetDeviceInfo
        mock.push_ok(Vec::new()); // OpenSession
        mock.push_ok(Vec::new()); // SetRemoteMode(1)
        mock.push_ok(Vec::new()); // SetEventMode(1)
        mock.push_ok(build_canon_event_payload()); // GetEvent pass 0
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]); // GetEvent sentinel
        mock.push_ok(Vec::new()); // SetDevicePropValueEx EvfMode=1
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]); // GetEvent after EvfMode
        mock.push_ok(Vec::new()); // SetDevicePropValueEx EvfOutputDevice=2
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]); // GetEvent settle
        mock.push_response(canon_response::NOT_READY, Vec::new()); // Transient 0xA102 NotReady
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]); // Drain event during NotReady recovery
        mock.push_response(response::DEVICE_BUSY, Vec::new()); // Transient busy
        mock.push_ok(vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x04, 0x38, 0x07, 0x80, 0x03, 0x01, 0x22,
            0x00, 0xFF, 0xD9,
        ]); // GetViewFinderData JPEG
        mock.push_ok(Vec::new()); // SetDevicePropValueEx ISO=0x58
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]); // GetEvent sentinel after set_property
        mock.push_ok(Vec::new()); // EvfOutputDevice=0
        mock.push_ok(Vec::new()); // EvfMode=0
        mock.push_ok(Vec::new()); // SetEventMode(0)
        mock.push_ok(Vec::new()); // CloseSession

        let mut driver = CanonDriver::new(mock);
        let caps = driver.connect().unwrap();
        assert_eq!(caps.camera_model, "Canon EOS R7");
        let iso = caps.properties.get(&CameraPropertyId::Iso).unwrap();
        assert_eq!(iso.current.label, "ISO 800");
        assert!(iso.writable);
        assert_eq!(iso.allowed_values.len(), 3);

        driver.start_live_view().unwrap();
        let frame = driver.next_frame().unwrap();
        assert_eq!((frame.width, frame.height), (1920, 1080));

        driver.set_property(CameraPropertyId::Iso, 0x58).unwrap();
        assert_eq!(
            driver.capabilities().properties[&CameraPropertyId::Iso]
                .current
                .label,
            "ISO 400"
        );

        driver.disconnect().unwrap();
    }

    #[test]
    fn canon_af_uses_safe_do_af_cancel_and_afc_mode() {
        let mock = MockPtpTransport::new();
        mock.push_ok(build_device_info_payload("Canon EOS R7")); // GetDeviceInfo
        mock.push_ok(Vec::new()); // OpenSession
        mock.push_ok(Vec::new()); // SetRemoteMode(1)
        mock.push_ok(Vec::new()); // SetEventMode(1)
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]); // GetEvent
        mock.push_ok(Vec::new()); // EvfMode=1
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]); // GetEvent
        mock.push_ok(Vec::new()); // EvfOutputDevice=2
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]); // GetEvent
                                                    // trigger_autofocus in One-Shot mode: TouchAfPosition + 1 event drain + DoAf + 6 event drains + AfCancel + 1 event drain
        mock.push_ok(Vec::new()); // TouchAfPosition (0x915B)
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]);
        mock.push_ok(Vec::new()); // DoAf (0x9154)
        for _ in 0..6 {
            mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]);
        }
        mock.push_ok(Vec::new()); // AfCancel (0x9160)
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]);
        // switch FocusMode -> 1 (AF-C): AF_MODE=1, CONTINUOUS_AF_MODE=1, MOVIE_SERVO_AF=1, DoAf + 3 event drains + AfCancel + GetEvent
        mock.push_ok(Vec::new());
        mock.push_ok(Vec::new());
        mock.push_ok(Vec::new());
        mock.push_ok(Vec::new());
        for _ in 0..3 {
            mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]);
        }
        mock.push_ok(Vec::new()); // AfCancel (0x9160)
        mock.push_ok(vec![8, 0, 0, 0, 0, 0, 0, 0]); // GetEvent sentinel
                                                    // drive_manual_focus calls DriveLens (0x9155)
        mock.push_ok(Vec::new()); // DriveLens (0x9155)
                                  // disconnect: EvfOutputDevice=0, EvfMode=0, SetEventMode(0), CloseSession
        mock.push_ok(Vec::new());
        mock.push_ok(Vec::new());
        mock.push_ok(Vec::new());
        mock.push_ok(Vec::new());

        let mut driver = CanonDriver::new(mock);
        driver.connect().unwrap();
        driver.start_live_view().unwrap();

        driver.trigger_autofocus(Some(0.5), Some(0.5)).unwrap();
        driver.set_property(CameraPropertyId::FocusMode, 1).unwrap();
        assert_eq!(
            format_canon_value(CameraPropertyId::FocusMode, 1),
            "AF-C (Servo AF)"
        );
        driver.drive_manual_focus(2).unwrap();

        let recorded = driver.session.transport.recorded_commands.lock().clone();
        assert!(!recorded.iter().any(|(op, _, _)| *op == 0x9158));
        assert!(recorded.iter().any(|(op, params, _)| {
            *op == canon_opcode::TOUCH_AF_POSITION && params == &[3, 3480, 2320]
        }));
        assert!(recorded
            .iter()
            .any(|(op, params, _)| *op == canon_opcode::DO_AF && params.is_empty()));
        assert!(recorded
            .iter()
            .any(|(op, params, _)| *op == canon_opcode::AF_CANCEL && params.is_empty()));
        assert!(recorded
            .iter()
            .any(|(op, params, _)| *op == canon_opcode::DRIVE_LENS && params == &[0x8002]));
        driver.disconnect().unwrap();
    }

    #[test]
    fn canon_eos_r7_tables_have_zero_hex_fallbacks() {
        let r7_shutters = [
            0x10, 0x13, 0x15, 0x18, 0x1B, 0x1D, 0x20, 0x23, 0x25, 0x28, 0x2B, 0x2D, 0x30, 0x33,
            0x35, 0x38, 0x3B, 0x3D, 0x40, 0x43, 0x45, 0x48, 0x4B, 0x4D, 0x50, 0x53, 0x55, 0x58,
            0x5B, 0x5D, 0x60, 0x63, 0x65, 0x68, 0x6B, 0x6D, 0x70, 0x73, 0x75, 0x78, 0x7B, 0x7D,
            0x80, 0x83, 0x85, 0x88, 0x8B, 0x8D, 0x90, 0x93, 0x95, 0x98, 0x9B, 0x9D, 0xA0,
        ];
        for raw in r7_shutters {
            let label = format_canon_value(CameraPropertyId::ShutterSpeed, raw);
            assert!(
                !label.starts_with("0x"),
                "unexpected hex shutter fallback for 0x{raw:02X}: {label}"
            );
        }

        for raw in [0x00, 0x48, 0x58, 0x88, 0x8B, 0x8D, 0x90] {
            let label = format_canon_value(CameraPropertyId::Iso, raw);
            assert!(
                !label.starts_with("0x"),
                "unexpected hex iso fallback for 0x{raw:02X}"
            );
        }

        let r7_ev = [
            0xE8, 0xEB, 0xED, 0xF0, 0xF3, 0xF5, 0xF8, 0xFB, 0xFD, 0x00, 0x03, 0x05, 0x08, 0x0B,
            0x0D, 0x10, 0x13, 0x15, 0x18,
        ];
        for raw in r7_ev {
            let label = format_canon_value(CameraPropertyId::ExposureCompensation, raw);
            assert!(
                !label.starts_with("0x"),
                "unexpected hex ev fallback for 0x{raw:02X}"
            );
        }

        for raw in [10, 1, 5, 6, 11, 12, 13, 14] {
            let label = format_canon_value(CameraPropertyId::FocusArea, raw);
            assert!(
                !label.starts_with("AF Area"),
                "unexpected focus area fallback for {raw}"
            );
        }
    }
}
