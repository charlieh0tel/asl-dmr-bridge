//! DV3000 packet-protocol byte constants.
//!
//! Wire framing (DVSI AMBE-3000R):
//!   start_byte(1) = 0x61
//!   payload_length(2, big-endian)
//!   packet_type(1)
//!   payload(variable)
//!
//! Held in a small public module so the internal parser
//! (`crate::dv3000`) and external relays / proxies can share one
//! definition.  Field-id constants used only by the parser stay
//! private to `dv3000`.

/// Frame start sentinel (every packet begins with this byte).
pub const START_BYTE: u8 = 0x61;

/// Length of the fixed-size header: start + len(2) + type.
pub const HEADER_SIZE: usize = 4;

/// `packet_type` values.
pub const TYPE_CONTROL: u8 = 0x00;
pub const TYPE_AMBE: u8 = 0x01;
pub const TYPE_AUDIO: u8 = 0x02;

/// First byte of a `PKT_CONTROL` payload: control field id.
pub const CONTROL_RESET: u8 = 0x33;
pub const CONTROL_READY: u8 = 0x39;
pub const CONTROL_RATEP: u8 = 0x0A;
pub const CONTROL_GAIN: u8 = 0x4B;
pub const CONTROL_PRODID: u8 = 0x30;
