pub mod macos_daemon;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use rusb::{Direction, GlobalContext, TransferType, UsbContext};
use thiserror::Error;
use tracing::{debug, warn};

/// Standard USB Image/PTP interface class code.
pub const USB_CLASS_PTP: u8 = 0x06;
pub const USB_SUBCLASS_PTP: u8 = 0x01;
pub const USB_PROTOCOL_PTP: u8 = 0x01;

/// PTP USB container types (ISO 15740 / PIMA 15740).
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerType {
    Command = 1,
    Data = 2,
    Response = 3,
    Event = 4,
}

impl ContainerType {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::Command),
            2 => Some(Self::Data),
            3 => Some(Self::Response),
            4 => Some(Self::Event),
            _ => None,
        }
    }
}

/// Standard PTP operation codes shared across vendors.
pub mod opcode {
    pub const GET_DEVICE_INFO: u16 = 0x1001;
    pub const OPEN_SESSION: u16 = 0x1002;
    pub const CLOSE_SESSION: u16 = 0x1003;
    pub const GET_OBJECT: u16 = 0x1009;
    pub const GET_DEVICE_PROP_DESC: u16 = 0x1014;
    pub const GET_DEVICE_PROP_VALUE: u16 = 0x1015;
    pub const SET_DEVICE_PROP_VALUE: u16 = 0x1016;
}

/// Standard PTP response codes.
pub mod response {
    pub const OK: u16 = 0x2001;
    pub const GENERAL_ERROR: u16 = 0x2002;
    pub const SESSION_NOT_OPEN: u16 = 0x2003;
    pub const INVALID_TRANSACTION_ID: u16 = 0x2004;
    pub const OPERATION_NOT_SUPPORTED: u16 = 0x2005;
    pub const PARAMETER_NOT_SUPPORTED: u16 = 0x2006;
    pub const DEVICE_BUSY: u16 = 0x2019;
    pub const INVALID_DEVICE_PROP_VALUE: u16 = 0x201C;
    pub const SESSION_ALREADY_OPEN: u16 = 0x201E;
}

/// Errors produced by PTP packet framing or USB bulk transport.
#[derive(Debug, Error)]
pub enum PtpError {
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),
    #[error("PTP still-image interface not found on USB device")]
    InterfaceNotFound,
    #[error("camera device not found or disconnected")]
    DeviceNotFound,
    #[error("malformed PTP container: {0}")]
    MalformedContainer(String),
    #[error("PTP response 0x{code:04X} for opcode 0x{opcode:04X}")]
    ProtocolError { opcode: u16, code: u16 },
    #[error("camera busy (0x2019)")]
    DeviceBusy,
    #[error("unexpected PTP container type {found} (expected {expected})")]
    UnexpectedContainer { expected: u16, found: u16 },
}

/// Decoded PTP response container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtpResponse {
    pub code: u16,
    pub transaction_id: u32,
    pub params: Vec<u32>,
}

/// Decoded raw PTP container header + payload slice helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtpContainer {
    pub container_type: ContainerType,
    pub code: u16,
    pub transaction_id: u32,
    pub payload: Vec<u8>,
}

impl PtpContainer {
    pub fn command(code: u16, transaction_id: u32, params: &[u32]) -> Self {
        let mut payload = Vec::with_capacity(params.len() * 4);
        for &p in params {
            payload.extend_from_slice(&p.to_le_bytes());
        }
        Self {
            container_type: ContainerType::Command,
            code,
            transaction_id,
            payload,
        }
    }

    pub fn data(code: u16, transaction_id: u32, payload: Vec<u8>) -> Self {
        Self {
            container_type: ContainerType::Data,
            code,
            transaction_id,
            payload,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let total_len = (12 + self.payload.len()) as u32;
        let mut out = Vec::with_capacity(total_len as usize);
        out.extend_from_slice(&total_len.to_le_bytes());
        out.extend_from_slice(&(self.container_type as u16).to_le_bytes());
        out.extend_from_slice(&self.code.to_le_bytes());
        out.extend_from_slice(&self.transaction_id.to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    pub fn parse_header(buf: &[u8]) -> Result<(u32, ContainerType, u16, u32), PtpError> {
        if buf.len() < 12 {
            return Err(PtpError::MalformedContainer(format!(
                "container header shorter than 12 bytes ({})",
                buf.len()
            )));
        }
        let length = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if length < 12 {
            return Err(PtpError::MalformedContainer(format!(
                "invalid container length {length} < 12"
            )));
        }
        let type_raw = u16::from_le_bytes([buf[4], buf[5]]);
        let container_type = ContainerType::from_u16(type_raw).ok_or_else(|| {
            PtpError::MalformedContainer(format!("unknown container type {type_raw}"))
        })?;
        let code = u16::from_le_bytes([buf[6], buf[7]]);
        let transaction_id = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        Ok((length, container_type, code, transaction_id))
    }

    pub fn parse_response(buf: &[u8]) -> Result<PtpResponse, PtpError> {
        let (length, ctype, code, tx_id) = Self::parse_header(buf)?;
        if ctype != ContainerType::Response {
            return Err(PtpError::UnexpectedContainer {
                expected: ContainerType::Response as u16,
                found: ctype as u16,
            });
        }
        let len = length as usize;
        if buf.len() < len {
            return Err(PtpError::MalformedContainer(format!(
                "truncated response container: expected {len} bytes, got {}",
                buf.len()
            )));
        }
        let mut params = Vec::new();
        let mut offset = 12;
        while offset + 4 <= len {
            params.push(u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]));
            offset += 4;
        }
        Ok(PtpResponse {
            code,
            transaction_id: tx_id,
            params,
        })
    }
}

/// Parsed subset of PTP `GetDeviceInfo` (0x1001).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PtpDeviceInfo {
    pub standard_version: u16,
    pub vendor_extension_id: u32,
    pub manufacturer: String,
    pub model: String,
    pub device_version: String,
    pub serial_number: String,
    pub operations_supported: Vec<u16>,
    pub device_properties_supported: Vec<u16>,
}

impl PtpDeviceInfo {
    /// Decodes a standard PTP `DeviceInfo` dataset payload.
    pub fn parse(data: &[u8]) -> Result<Self, PtpError> {
        let mut c = ByteCursor::new(data);
        let standard_version = c.read_u16()?;
        let vendor_extension_id = c.read_u32()?;
        let _vendor_ext_ver = c.read_u16()?;
        let _vendor_ext_desc = c.read_ptp_string()?;
        let _functional_mode = c.read_u16()?;
        let operations_supported = c.read_u16_array()?;
        let _events_supported = c.read_u16_array()?;
        let device_properties_supported = c.read_u16_array()?;
        let _capture_formats = c.read_u16_array()?;
        let _image_formats = c.read_u16_array()?;
        let manufacturer = c.read_ptp_string()?;
        let model = c.read_ptp_string()?;
        let device_version = c.read_ptp_string()?;
        let serial_number = c.read_ptp_string()?;

        Ok(Self {
            standard_version,
            vendor_extension_id,
            manufacturer,
            model,
            device_version,
            serial_number,
            operations_supported,
            device_properties_supported,
        })
    }
}

/// Little-endian cursor for decoding PTP binary datasets and UTF-16LE PTP strings.
pub struct ByteCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn read_u8(&mut self) -> Result<u8, PtpError> {
        if self.pos >= self.data.len() {
            return Err(PtpError::MalformedContainer(
                "unexpected EOF reading u8".into(),
            ));
        }
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }

    pub fn read_u16(&mut self) -> Result<u16, PtpError> {
        if self.pos + 2 > self.data.len() {
            return Err(PtpError::MalformedContainer(
                "unexpected EOF reading u16".into(),
            ));
        }
        let v = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    pub fn read_u32(&mut self) -> Result<u32, PtpError> {
        if self.pos + 4 > self.data.len() {
            return Err(PtpError::MalformedContainer(
                "unexpected EOF reading u32".into(),
            ));
        }
        let v = u32::from_le_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }

    pub fn read_u16_array(&mut self) -> Result<Vec<u16>, PtpError> {
        let count = self.read_u32()? as usize;
        if count > 4096 || self.pos + count * 2 > self.data.len() {
            return Err(PtpError::MalformedContainer(format!(
                "invalid u16 array count {count}"
            )));
        }
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.read_u16()?);
        }
        Ok(out)
    }

    pub fn read_ptp_string(&mut self) -> Result<String, PtpError> {
        let num_chars = self.read_u8()? as usize;
        if num_chars == 0 {
            return Ok(String::new());
        }
        if self.pos + num_chars * 2 > self.data.len() {
            return Err(PtpError::MalformedContainer(format!(
                "truncated PTP string of {num_chars} UTF-16 code units"
            )));
        }
        let mut units = Vec::with_capacity(num_chars);
        for _ in 0..num_chars {
            units.push(self.read_u16()?);
        }
        while units.last() == Some(&0) {
            units.pop();
        }
        Ok(String::from_utf16_lossy(&units))
    }

    pub fn read_typed_u32(&mut self, data_type: u16) -> Result<u32, PtpError> {
        match data_type {
            0x0001 | 0x0002 => Ok(u32::from(self.read_u8()?)),
            0x0003 | 0x0004 => Ok(u32::from(self.read_u16()?)),
            0x0005 | 0x0006 => self.read_u32(),
            other => Err(PtpError::MalformedContainer(format!(
                "unsupported PTP scalar data type 0x{other:04X}"
            ))),
        }
    }
}

/// Parsed standard PTP `DevicePropDesc` (ISO 15740, returned by `0x1014`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtpDevicePropDesc {
    pub prop_code: u16,
    pub data_type: u16,
    pub writable: bool,
    pub default_value: u32,
    pub current_value: u32,
    pub allowed_values: Vec<u32>,
}

impl PtpDevicePropDesc {
    pub fn parse(data: &[u8]) -> Result<Self, PtpError> {
        let mut c = ByteCursor::new(data);
        let prop_code = c.read_u16()?;
        let data_type = c.read_u16()?;
        let writable = c.read_u8()? != 0;
        let default_value = c.read_typed_u32(data_type)?;
        let current_value = c.read_typed_u32(data_type)?;
        let form_flag = c.read_u8()?;

        let mut allowed_values = Vec::new();
        match form_flag {
            0x01 => {
                // Range form: min, max, step
                let min = c.read_typed_u32(data_type)?;
                let max = c.read_typed_u32(data_type)?;
                let step = c.read_typed_u32(data_type)?.max(1);
                let mut cur = min;
                let mut count = 0usize;
                while cur <= max && count < 256 {
                    allowed_values.push(cur);
                    cur = cur.saturating_add(step);
                    count += 1;
                }
            }
            0x02 => {
                // Enum form: u16 count + values
                let count = usize::from(c.read_u16()?).min(512);
                allowed_values.reserve(count);
                for _ in 0..count {
                    allowed_values.push(c.read_typed_u32(data_type)?);
                }
            }
            _ => {}
        }

        Ok(Self {
            prop_code,
            data_type,
            writable,
            default_value,
            current_value,
            allowed_values,
        })
    }

    pub fn encode_value(data_type: u16, value: u32) -> Vec<u8> {
        match data_type {
            0x0001 | 0x0002 => vec![value as u8],
            0x0003 | 0x0004 => (value as u16).to_le_bytes().to_vec(),
            _ => value.to_le_bytes().to_vec(),
        }
    }
}

/// Abstract half-duplex PTP transport backend so drivers can be unit-tested deterministically
/// without hardware attached while driving live USB cameras via [`UsbPtpTransport`].
pub trait PtpTransportBackend: Send {
    fn command_nodata(&mut self, opcode: u16, params: &[u32]) -> Result<PtpResponse, PtpError>;
    fn command_data_in(
        &mut self,
        opcode: u16,
        params: &[u32],
    ) -> Result<(Vec<u8>, PtpResponse), PtpError>;
    fn command_data_out(
        &mut self,
        opcode: u16,
        params: &[u32],
        data: &[u8],
    ) -> Result<PtpResponse, PtpError>;
    fn poll_interrupt(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, PtpError> {
        Ok(None)
    }
    fn reset_transaction_ids(&mut self);
    fn reset_pipe(&mut self) {}
}

/// Live USB PTP transport backed by `rusb` / `libusb-1.0`.
pub struct UsbPtpTransport {
    handle: rusb::DeviceHandle<GlobalContext>,
    interface: u8,
    ep_in: u8,
    ep_out: u8,
    ep_int: Option<u8>,
    max_packet_out: usize,
    tx_id: AtomicU32,
    timeout: Duration,
    reattached_kernel_driver: bool,
    had_transfer_error: bool,
}

impl UsbPtpTransport {
    /// Opens the PTP interface on a discovered `rusb::Device`, automatically guarding
    /// against competing macOS daemons (`ptpcamerad` / `mscamerad-xpc`) if claiming fails.
    pub fn open(device: &rusb::Device<GlobalContext>) -> Result<Self, PtpError> {
        let config = device
            .active_config_descriptor()
            .or_else(|_| device.config_descriptor(0))?;
        let mut found: Option<(u8, u8, u8, Option<u8>, usize)> = None;

        for iface in config.interfaces() {
            for desc in iface.descriptors() {
                if desc.class_code() == USB_CLASS_PTP {
                    let mut ep_in = None;
                    let mut ep_out = None;
                    let mut ep_int = None;
                    let mut max_packet_out = 512usize;

                    for ep in desc.endpoint_descriptors() {
                        match (ep.direction(), ep.transfer_type()) {
                            (Direction::In, TransferType::Bulk) => {
                                ep_in = Some(ep.address());
                            }
                            (Direction::Out, TransferType::Bulk) => {
                                ep_out = Some(ep.address());
                                max_packet_out = usize::from(ep.max_packet_size()).max(64);
                            }
                            (Direction::In, TransferType::Interrupt) => {
                                ep_int = Some(ep.address());
                            }
                            _ => {}
                        }
                    }

                    if let (Some(i), Some(o)) = (ep_in, ep_out) {
                        found = Some((desc.interface_number(), i, o, ep_int, max_packet_out));
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }

        let (interface, ep_in, ep_out, ep_int, max_packet_out) =
            found.ok_or(PtpError::InterfaceNotFound)?;

        let handle = device.open()?;
        let mut reattached_kernel_driver = false;

        if matches!(handle.kernel_driver_active(interface), Ok(true)) {
            if let Err(err) = handle.detach_kernel_driver(interface) {
                debug!(?err, interface, "detach_kernel_driver skipped");
            } else {
                reattached_kernel_driver = true;
            }
        }

        if let Err(err) = handle.claim_interface(interface) {
            if matches!(err, rusb::Error::Busy | rusb::Error::Access) {
                warn!(
                    ?err,
                    "USB PTP interface locked; attempting macOS daemon release"
                );
                if macos_daemon::release_competing_daemons() {
                    handle.claim_interface(interface)?;
                } else {
                    return Err(PtpError::Usb(err));
                }
            } else {
                return Err(PtpError::Usb(err));
            }
        }

        debug!(
            interface,
            ep_in = format_args!("0x{ep_in:02X}"),
            ep_out = format_args!("0x{ep_out:02X}"),
            ?ep_int,
            "claimed USB PTP interface"
        );

        let transport = Self {
            handle,
            interface,
            ep_in,
            ep_out,
            ep_int,
            max_packet_out,
            tx_id: AtomicU32::new(0),
            timeout: Duration::from_millis(2500),
            reattached_kernel_driver,
            had_transfer_error: false,
        };
        transport.clear_stall();
        Ok(transport)
    }

    /// Issues the USB Still Image Class-Specific Reset (`0x66`) and clears bulk endpoint halts.
    pub fn clear_stall(&self) {
        let _ = self.handle.write_control(
            0x21,
            0x66,
            0,
            u16::from(self.interface),
            &[],
            Duration::from_millis(1000),
        );
        let _ = self.handle.clear_halt(self.ep_in);
        let _ = self.handle.clear_halt(self.ep_out);
    }

    /// Finds and opens a USB camera matching `vendor_id` and optional `product_id` / `serial`.
    pub fn open_matching(
        vendor_id: u16,
        product_id: Option<u16>,
        serial: Option<&str>,
    ) -> Result<Self, PtpError> {
        let devices = GlobalContext::default().devices()?;
        for device in devices.iter() {
            let Ok(desc) = device.device_descriptor() else {
                continue;
            };
            if desc.vendor_id() != vendor_id {
                continue;
            }
            if let Some(pid) = product_id {
                if desc.product_id() != pid {
                    continue;
                }
            }
            if let Some(target_serial) = serial {
                if let Ok(h) = device.open() {
                    if let Ok(s) = h.read_serial_number_string_ascii(&desc) {
                        if s != target_serial {
                            continue;
                        }
                    }
                }
            }
            return Self::open(&device);
        }
        Err(PtpError::DeviceNotFound)
    }

    fn next_tx_id(&self) -> u32 {
        self.tx_id.fetch_add(1, Ordering::Relaxed)
    }

    fn write_all_bulk(&self, bytes: &[u8]) -> Result<(), PtpError> {
        let mut written = 0usize;
        while written < bytes.len() {
            let n = self
                .handle
                .write_bulk(self.ep_out, &bytes[written..], self.timeout)?;
            if n == 0 {
                return Err(PtpError::MalformedContainer(
                    "zero-byte bulk write before packet completion".into(),
                ));
            }
            written += n;
        }
        // PTP over USB requires a ZLP when the container ends on an exact max-packet boundary.
        if !bytes.is_empty() && bytes.len() % self.max_packet_out == 0 {
            let _ = self.handle.write_bulk(self.ep_out, &[], self.timeout)?;
        }
        Ok(())
    }

    fn read_response_only(&mut self) -> Result<PtpResponse, PtpError> {
        let mut buf = [0u8; 512];
        let mut n = self
            .handle
            .read_bulk(self.ep_in, &mut buf, self.timeout)
            .inspect_err(|_| self.had_transfer_error = true)?;
        if n == 0 {
            // Exact max-packet data transfer ended with a ZLP; read the Response container next.
            n = self
                .handle
                .read_bulk(self.ep_in, &mut buf, self.timeout)
                .inspect_err(|_| self.had_transfer_error = true)?;
        }
        PtpContainer::parse_response(&buf[..n])
    }
}

impl PtpTransportBackend for UsbPtpTransport {
    fn reset_transaction_ids(&mut self) {
        self.tx_id.store(1, Ordering::Relaxed);
    }

    fn reset_pipe(&mut self) {
        self.clear_stall();
        self.had_transfer_error = false;
    }

    fn command_nodata(&mut self, opcode: u16, params: &[u32]) -> Result<PtpResponse, PtpError> {
        let tx = self.next_tx_id();
        let cmd = PtpContainer::command(opcode, tx, params).encode();
        self.write_all_bulk(&cmd)
            .inspect_err(|_| self.had_transfer_error = true)?;
        let resp = self.read_response_only()?;
        check_ptp_response(opcode, &resp)?;
        Ok(resp)
    }

    fn command_data_in(
        &mut self,
        opcode: u16,
        params: &[u32],
    ) -> Result<(Vec<u8>, PtpResponse), PtpError> {
        let tx = self.next_tx_id();
        let cmd = PtpContainer::command(opcode, tx, params).encode();
        self.write_all_bulk(&cmd)
            .inspect_err(|_| self.had_transfer_error = true)?;

        let mut chunk = vec![0u8; 512 * 1024];
        let first_n = self
            .handle
            .read_bulk(self.ep_in, &mut chunk, self.timeout)
            .inspect_err(|_| self.had_transfer_error = true)?;
        if first_n < 12 {
            return Err(PtpError::MalformedContainer(format!(
                "short initial bulk IN read ({first_n} bytes)"
            )));
        }

        let (length, ctype, _code, _rx_tx) = PtpContainer::parse_header(&chunk[..first_n])?;
        if ctype == ContainerType::Response {
            let resp = PtpContainer::parse_response(&chunk[..first_n])?;
            check_ptp_response(opcode, &resp)?;
            return Ok((Vec::new(), resp));
        }
        if ctype != ContainerType::Data {
            return Err(PtpError::UnexpectedContainer {
                expected: ContainerType::Data as u16,
                found: ctype as u16,
            });
        }

        let total_len = length as usize;
        let mut buf = Vec::with_capacity(total_len.max(first_n));
        buf.extend_from_slice(&chunk[..first_n]);

        while buf.len() < total_len {
            let remaining = total_len - buf.len();
            let read_size = (remaining.div_ceil(512).max(8) * 512).min(512 * 1024);
            let old_len = buf.len();
            buf.resize(old_len + read_size, 0);
            let n = self
                .handle
                .read_bulk(self.ep_in, &mut buf[old_len..], self.timeout)
                .inspect_err(|_| self.had_transfer_error = true)?;
            buf.truncate(old_len + n);
            if n == 0 {
                break;
            }
        }

        if buf.len() < total_len {
            return Err(PtpError::MalformedContainer(format!(
                "incomplete PTP data phase: expected {total_len}, got {}",
                buf.len()
            )));
        }

        let data = buf[12..total_len].to_vec();
        let resp = if buf.len() > total_len {
            let mut tail = buf[total_len..].to_vec();
            while tail.len() < 12 {
                let mut extra = [0u8; 512];
                let n = self
                    .handle
                    .read_bulk(self.ep_in, &mut extra, self.timeout)
                    .inspect_err(|_| self.had_transfer_error = true)?;
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&extra[..n]);
            }
            PtpContainer::parse_response(&tail)?
        } else {
            self.read_response_only()?
        };

        check_ptp_response(opcode, &resp)?;
        Ok((data, resp))
    }

    fn command_data_out(
        &mut self,
        opcode: u16,
        params: &[u32],
        data: &[u8],
    ) -> Result<PtpResponse, PtpError> {
        let tx = self.next_tx_id();
        let cmd = PtpContainer::command(opcode, tx, params).encode();
        self.write_all_bulk(&cmd)
            .inspect_err(|_| self.had_transfer_error = true)?;

        let data_packet = PtpContainer::data(opcode, tx, data.to_vec()).encode();
        self.write_all_bulk(&data_packet)
            .inspect_err(|_| self.had_transfer_error = true)?;

        let resp = self.read_response_only()?;
        check_ptp_response(opcode, &resp)?;
        Ok(resp)
    }

    fn poll_interrupt(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, PtpError> {
        let Some(ep_int) = self.ep_int else {
            return Ok(None);
        };
        let mut buf = [0u8; 64];
        match self.handle.read_interrupt(ep_int, &mut buf, timeout) {
            Ok(n) if n > 0 => Ok(Some(buf[..n].to_vec())),
            Ok(_) | Err(rusb::Error::Timeout) => Ok(None),
            Err(e) => Err(PtpError::Usb(e)),
        }
    }
}

impl Drop for UsbPtpTransport {
    fn drop(&mut self) {
        if self.had_transfer_error {
            self.clear_stall();
        }
        let _ = self.handle.release_interface(self.interface);
        if self.reattached_kernel_driver {
            let _ = self.handle.attach_kernel_driver(self.interface);
        }
    }
}

fn check_ptp_response(opcode: u16, resp: &PtpResponse) -> Result<(), PtpError> {
    match resp.code {
        response::OK | response::SESSION_ALREADY_OPEN => Ok(()),
        response::DEVICE_BUSY => Err(PtpError::DeviceBusy),
        other => Err(PtpError::ProtocolError {
            opcode,
            code: other,
        }),
    }
}

/// Retries a PTP operation when the camera responds with transient `DeviceBusy` (`0x2019`).
pub fn retry_on_device_busy<R>(
    attempts: usize,
    delay: Duration,
    mut op: impl FnMut() -> Result<R, PtpError>,
) -> Result<R, PtpError> {
    let total = attempts.max(1);
    for attempt in 0..total {
        match op() {
            Ok(val) => return Ok(val),
            Err(PtpError::DeviceBusy)
            | Err(PtpError::ProtocolError {
                code: response::DEVICE_BUSY,
                ..
            }) if attempt + 1 < total => {
                std::thread::sleep(delay);
            }
            Err(err) => return Err(err),
        }
    }
    Err(PtpError::DeviceBusy)
}

/// Session state machine wrapping a [`PtpTransportBackend`] with clean session lifecycle.
pub struct PtpSession<T: PtpTransportBackend> {
    pub transport: T,
    session_open: bool,
}

impl<T: PtpTransportBackend> PtpSession<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            session_open: false,
        }
    }

    pub fn open_session(&mut self, session_id: u32) -> Result<(), PtpError> {
        if self.session_open {
            return Ok(());
        }
        self.transport
            .command_nodata(opcode::OPEN_SESSION, &[session_id])?;
        self.transport.reset_transaction_ids();
        self.session_open = true;
        Ok(())
    }

    pub fn close_session(&mut self) -> Result<(), PtpError> {
        if !self.session_open {
            return Ok(());
        }
        let res = self.transport.command_nodata(opcode::CLOSE_SESSION, &[]);
        self.session_open = false;
        res.map(|_| ())
    }

    pub fn is_open(&self) -> bool {
        self.session_open
    }

    pub fn get_device_info(&mut self) -> Result<PtpDeviceInfo, PtpError> {
        let (data, _) = self
            .transport
            .command_data_in(opcode::GET_DEVICE_INFO, &[])?;
        PtpDeviceInfo::parse(&data)
    }
}

impl<T: PtpTransportBackend> Drop for PtpSession<T> {
    fn drop(&mut self) {
        if self.session_open {
            let _ = self.close_session();
        }
    }
}

/// Recorded command entry `(opcode, params, data_out)` stored by [`MockPtpTransport`].
pub type RecordedCommand = (u16, Vec<u32>, Vec<u8>);
type MockReply = Result<(Vec<u8>, u16), PtpError>;

/// Recorded command + mock reply queue used for deterministic driver unit tests.
#[derive(Default)]
pub struct MockPtpTransport {
    pub recorded_commands: Mutex<Vec<RecordedCommand>>,
    replies: Mutex<VecDeque<MockReply>>,
    tx_id: u32,
}

impl MockPtpTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_ok(&self, data: Vec<u8>) {
        self.replies.lock().push_back(Ok((data, response::OK)));
    }

    pub fn push_response(&self, code: u16, data: Vec<u8>) {
        self.replies.lock().push_back(Ok((data, code)));
    }

    pub fn push_err(&self, err: PtpError) {
        self.replies.lock().push_back(Err(err));
    }

    fn next_reply(&mut self, opcode: u16) -> Result<(Vec<u8>, PtpResponse), PtpError> {
        self.tx_id += 1;
        let reply = self
            .replies
            .lock()
            .pop_front()
            .unwrap_or_else(|| Ok((Vec::new(), response::OK)))?;
        let resp = PtpResponse {
            code: reply.1,
            transaction_id: self.tx_id,
            params: Vec::new(),
        };
        check_ptp_response(opcode, &resp)?;
        Ok((reply.0, resp))
    }
}

impl PtpTransportBackend for MockPtpTransport {
    fn command_nodata(&mut self, opcode: u16, params: &[u32]) -> Result<PtpResponse, PtpError> {
        self.recorded_commands
            .lock()
            .push((opcode, params.to_vec(), Vec::new()));
        let (_, resp) = self.next_reply(opcode)?;
        Ok(resp)
    }

    fn command_data_in(
        &mut self,
        opcode: u16,
        params: &[u32],
    ) -> Result<(Vec<u8>, PtpResponse), PtpError> {
        self.recorded_commands
            .lock()
            .push((opcode, params.to_vec(), Vec::new()));
        self.next_reply(opcode)
    }

    fn command_data_out(
        &mut self,
        opcode: u16,
        params: &[u32],
        data: &[u8],
    ) -> Result<PtpResponse, PtpError> {
        self.recorded_commands
            .lock()
            .push((opcode, params.to_vec(), data.to_vec()));
        let (_, resp) = self.next_reply(opcode)?;
        Ok(resp)
    }

    fn reset_transaction_ids(&mut self) {
        self.tx_id = 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_parses_ptp_command_and_response() {
        let cmd = PtpContainer::command(opcode::OPEN_SESSION, 7, &[1]);
        let bytes = cmd.encode();
        assert_eq!(bytes.len(), 16);
        let (len, ctype, code, tx) = PtpContainer::parse_header(&bytes).unwrap();
        assert_eq!(len, 16);
        assert_eq!(ctype, ContainerType::Command);
        assert_eq!(code, opcode::OPEN_SESSION);
        assert_eq!(tx, 7);

        let mut resp_bytes = Vec::new();
        resp_bytes.extend_from_slice(&12u32.to_le_bytes());
        resp_bytes.extend_from_slice(&(ContainerType::Response as u16).to_le_bytes());
        resp_bytes.extend_from_slice(&response::OK.to_le_bytes());
        resp_bytes.extend_from_slice(&7u32.to_le_bytes());
        let resp = PtpContainer::parse_response(&resp_bytes).unwrap();
        assert_eq!(resp.code, response::OK);
        assert_eq!(resp.transaction_id, 7);
    }

    #[test]
    fn session_closes_cleanly_on_drop() {
        let mock = MockPtpTransport::new();
        mock.push_ok(Vec::new()); // OpenSession
        mock.push_ok(Vec::new()); // CloseSession

        let mut session = PtpSession::new(mock);
        session.open_session(1).unwrap();
        assert!(session.is_open());
        session.close_session().unwrap();
        assert!(!session.is_open());

        let cmds = session.transport.recorded_commands.lock();
        assert_eq!(cmds[0].0, opcode::OPEN_SESSION);
        assert_eq!(cmds[1].0, opcode::CLOSE_SESSION);
    }

    #[test]
    fn parses_ptp_device_info_dataset() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&100u16.to_le_bytes()); // standard version
        buf.extend_from_slice(&0x0000000Bu32.to_le_bytes()); // Canon vendor ext
        buf.extend_from_slice(&100u16.to_le_bytes());
        buf.push(0); // vendor ext desc empty
        buf.extend_from_slice(&0u16.to_le_bytes()); // functional mode
        for _ in 0..5 {
            buf.extend_from_slice(&0u32.to_le_bytes()); // empty u16 arrays
        }
        for s in ["Canon Inc.", "Canon EOS R7", "1.4.0", "5abc0545"] {
            let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
            buf.push(units.len() as u8);
            for u in units {
                buf.extend_from_slice(&u.to_le_bytes());
            }
        }

        let info = PtpDeviceInfo::parse(&buf).unwrap();
        assert_eq!(info.manufacturer, "Canon Inc.");
        assert_eq!(info.model, "Canon EOS R7");
        assert_eq!(info.serial_number, "5abc0545");
    }
}
