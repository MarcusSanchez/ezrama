//! Request builders and response views for the display protocol.
//!
//! Requests are built as protobuf payloads ready for framing. Responses are
//! parsed into borrowed views; fields the views do not know about are left
//! in place and ignored.

use std::num::NonZeroU64;

use crate::pb::{DecodeError, Fields, Message, Value};

/// Header version carried by every request.
pub const PROTOCOL_VERSION: u64 = 1;
/// Placeholder string carried by the bootstrap query tokens.
pub const QUERY_DUMMY: &[u8] = b"NA";
/// Key sent with the authentication query.
pub const AUTH_KEY: u64 = 1;
/// Payload sent with every keepalive ping.
pub const PING_PAYLOAD: &[u8] = b"hello?";

/// Field numbers of the request and response envelopes.
pub mod field {
    pub const HEADER: u32 = 1;
    pub const ERROR: u32 = 2;

    pub const PING: u32 = 10;
    pub const DEVICE_INFORMATION_QUERY: u32 = 100;
    pub const DEVICE_AUTHENTICATION_QUERY: u32 = 101;
    pub const SYSTEM_CONFIGURATION_QUERY: u32 = 102;
    pub const MEDIA_CATALOG_QUERY: u32 = 103;
    pub const USER_CONFIGURATION_QUERY: u32 = 104;
    pub const USER_CONFIGURATION_UPDATE: u32 = 200;
    pub const OVERLAY_LAYOUT: u32 = 201;
    pub const METRIC_BATCH: u32 = 300;

    pub const PONG: u32 = 10;
    pub const DEVICE_INFORMATION: u32 = 500;
    pub const DEVICE_AUTHENTICATION: u32 = 501;
    pub const SYSTEM_CONFIGURATION: u32 = 502;
    pub const MEDIA_CATALOG: u32 = 503;
    pub const USER_CONFIGURATION: u32 = 504;
    pub const ACKNOWLEDGEMENT: u32 = 600;
    pub const TRANSFER_BEGIN_STATUS: u32 = 800;
    pub const TRANSFER_CHUNK_STATUS: u32 = 801;
    pub const TRANSFER_END_STATUS: u32 = 802;
    pub const MEDIA_READ_CHUNK: u32 = 805;
    pub const ASYNCHRONOUS_EVENT: u32 = 987;

    /// Every field number a response may carry as its body.
    pub const RESPONSE_BODIES: [u32; 12] = [
        PONG,
        DEVICE_INFORMATION,
        DEVICE_AUTHENTICATION,
        SYSTEM_CONFIGURATION,
        MEDIA_CATALOG,
        USER_CONFIGURATION,
        ACKNOWLEDGEMENT,
        TRANSFER_BEGIN_STATUS,
        TRANSFER_CHUNK_STATUS,
        TRANSFER_END_STATUS,
        MEDIA_READ_CHUNK,
        ASYNCHRONOUS_EVENT,
    ];
}

/// Field numbers inside `WireHeader`.
mod header_field {
    pub const VERSION: u32 = 1;
    pub const TRACK_ID: u32 = 2;
    pub const PAYLOAD_CRC32: u32 = 3;
}

/// Field numbers inside `ProtocolError`.
mod error_field {
    pub const CODE: u32 = 1;
    pub const WHY: u32 = 2;
}

fn bootstrap_header() -> Message {
    Message::new().uint(header_field::VERSION, PROTOCOL_VERSION)
}

fn tracked_header(track_id: NonZeroU64) -> Message {
    Message::new()
        .uint(header_field::VERSION, PROTOCOL_VERSION)
        .uint(header_field::TRACK_ID, track_id.get())
}

fn query_token() -> Message {
    Message::new().bytes(1, QUERY_DUMMY)
}

/// First bootstrap request. Untracked; answered by `DEVICE_INFORMATION`.
pub fn device_information_query() -> Vec<u8> {
    Message::new()
        .message(field::HEADER, &bootstrap_header())
        .message(field::DEVICE_INFORMATION_QUERY, &query_token())
        .into_bytes()
}

/// Second bootstrap request. Untracked; answered by `SYSTEM_CONFIGURATION`.
pub fn system_configuration_query() -> Vec<u8> {
    Message::new()
        .message(field::HEADER, &bootstrap_header())
        .message(field::SYSTEM_CONFIGURATION_QUERY, &query_token())
        .into_bytes()
}

/// Third bootstrap request. Untracked; answered by `DEVICE_AUTHENTICATION`.
pub fn device_authentication_query() -> Vec<u8> {
    Message::new()
        .message(field::HEADER, &bootstrap_header())
        .message(
            field::DEVICE_AUTHENTICATION_QUERY,
            &Message::new().uint(1, AUTH_KEY),
        )
        .into_bytes()
}

/// Keepalive. Untracked and write-only; the header is present but empty.
pub fn keepalive_ping() -> Vec<u8> {
    Message::new()
        .message(field::HEADER, &Message::new())
        .message(field::PING, &Message::new().bytes(1, PING_PAYLOAD))
        .into_bytes()
}

/// Empty overlay layout that activates the stored configuration. Tracked;
/// a response is optional.
pub fn activation_trigger(track_id: NonZeroU64) -> Vec<u8> {
    Message::new()
        .message(field::HEADER, &tracked_header(track_id))
        .message(field::OVERLAY_LAYOUT, &Message::new())
        .into_bytes()
}

/// Read-only configuration query. Tracked; answered by `USER_CONFIGURATION`.
pub fn user_configuration_query(track_id: NonZeroU64) -> Vec<u8> {
    Message::new()
        .message(field::HEADER, &tracked_header(track_id))
        .message(field::USER_CONFIGURATION_QUERY, &Message::new())
        .into_bytes()
}

/// `WireHeader` as sent by the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    pub version: u64,
    pub track_id: u64,
    pub payload_crc32: u64,
}

impl Header {
    pub fn parse(body: &[u8]) -> Result<Self, DecodeError> {
        let mut header = Header::default();
        for field in Fields::new(body) {
            let field = field?;
            let Some(value) = field.value.as_u64() else {
                continue;
            };
            match field.number {
                header_field::VERSION => header.version = value,
                header_field::TRACK_ID => header.track_id = value,
                header_field::PAYLOAD_CRC32 => header.payload_crc32 = value,
                _ => {}
            }
        }
        Ok(header)
    }
}

/// `ProtocolError` as sent by the device. Code zero means success.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProtocolError {
    pub code: u64,
    pub why: String,
}

impl ProtocolError {
    pub fn parse(body: &[u8]) -> Result<Self, DecodeError> {
        let mut error = ProtocolError::default();
        for field in Fields::new(body) {
            let field = field?;
            match (field.number, field.value) {
                (error_field::CODE, value) => {
                    if let Some(code) = value.as_u64() {
                        error.code = code;
                    }
                }
                (error_field::WHY, Value::Len(bytes)) => error.why = text(bytes),
                _ => {}
            }
        }
        Ok(error)
    }

    pub fn is_success(&self) -> bool {
        self.code == 0
    }
}

/// The envelope of a device response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response<'a> {
    /// `None` when the header field is absent. An empty header is `Some`
    /// with every field at its default.
    pub header: Option<Header>,
    /// `None` when the error field is absent.
    pub error: Option<ProtocolError>,
    /// The body field number and its raw bytes, if any known body is set.
    pub body: Option<(u32, &'a [u8])>,
}

impl<'a> Response<'a> {
    pub fn parse(payload: &'a [u8]) -> Result<Self, DecodeError> {
        let mut response = Response {
            header: None,
            error: None,
            body: None,
        };
        for field in Fields::new(payload) {
            let field = field?;
            let Value::Len(bytes) = field.value else {
                continue;
            };
            match field.number {
                field::HEADER => response.header = Some(Header::parse(bytes)?),
                field::ERROR => response.error = Some(ProtocolError::parse(bytes)?),
                number if field::RESPONSE_BODIES.contains(&number) => {
                    response.body = Some((number, bytes));
                }
                _ => {}
            }
        }
        Ok(response)
    }

    /// The body field number, if a body is present.
    pub fn body_number(&self) -> Option<u32> {
        self.body.map(|(number, _)| number)
    }

    /// The error when the device reported one with a non-zero code.
    pub fn rejection(&self) -> Option<&ProtocolError> {
        self.error.as_ref().filter(|error| !error.is_success())
    }
}

/// `DeviceInformation` response body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceInformation {
    pub os_name: String,
    pub os_version: String,
    pub firmware_version: String,
    pub product_name: String,
    pub app_version: String,
    pub serial_number: String,
    pub serial_number_locked: bool,
    pub chip_id: String,
}

impl DeviceInformation {
    pub fn parse(body: &[u8]) -> Result<Self, DecodeError> {
        let mut info = DeviceInformation::default();
        for field in Fields::new(body) {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Len(b)) => info.os_name = text(b),
                (2, Value::Len(b)) => info.os_version = text(b),
                (3, Value::Len(b)) => info.firmware_version = text(b),
                (4, Value::Len(b)) => info.product_name = text(b),
                (5, Value::Len(b)) => info.app_version = text(b),
                (8, Value::Len(b)) => info.serial_number = text(b),
                (9, value) => info.serial_number_locked = value.as_u64() == Some(1),
                (10, Value::Len(b)) => info.chip_id = text(b),
                _ => {}
            }
        }
        Ok(info)
    }
}

/// `DeviceAuthentication` response body: the opaque `auth` string.
pub fn parse_device_authentication(body: &[u8]) -> Result<String, DecodeError> {
    let mut auth = String::new();
    for field in Fields::new(body) {
        let field = field?;
        if let (1, Value::Len(bytes)) = (field.number, field.value) {
            auth = text(bytes);
        }
    }
    Ok(auth)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaMode {
    Single,
    Dual,
    Kaleidoscope,
    Unknown(u64),
}

impl From<u64> for MediaMode {
    fn from(value: u64) -> Self {
        match value {
            0 => MediaMode::Single,
            1 => MediaMode::Dual,
            2 => MediaMode::Kaleidoscope,
            other => MediaMode::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode {
    Single,
    All,
    Random,
    Unknown(u64),
}

impl From<u64> for LoopMode {
    fn from(value: u64) -> Self {
        match value {
            0 => LoopMode::Single,
            1 => LoopMode::All,
            2 => LoopMode::Random,
            other => LoopMode::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PowerOnConfiguration {
    pub media_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StandbyConfiguration {
    pub enable: bool,
    pub media_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkConfiguration {
    pub media_mode: MediaMode,
    pub loop_mode: LoopMode,
    pub single_mode_media_file: String,
    pub dual_mode_left_media_file: String,
    pub dual_mode_right_media_file: String,
    pub kaleidoscope_media_file: String,
    pub kaleidoscope_source: u64,
}

impl Default for WorkConfiguration {
    fn default() -> Self {
        Self {
            media_mode: MediaMode::Single,
            loop_mode: LoopMode::Single,
            single_mode_media_file: String::new(),
            dual_mode_left_media_file: String::new(),
            dual_mode_right_media_file: String::new(),
            kaleidoscope_media_file: String::new(),
            kaleidoscope_source: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DisplayConfiguration {
    pub backlight_enable: bool,
    pub backlight_brightness: u64,
    pub mirror: bool,
    pub ui_rotation: u64,
    pub media_rotation: u64,
}

/// `UserConfiguration` response body. Each section is `None` when the
/// device did not include it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserConfiguration {
    pub poweron: Option<PowerOnConfiguration>,
    pub standby: Option<StandbyConfiguration>,
    pub work: Option<WorkConfiguration>,
    pub display: Option<DisplayConfiguration>,
}

impl UserConfiguration {
    pub fn parse(body: &[u8]) -> Result<Self, DecodeError> {
        let mut config = UserConfiguration::default();
        for field in Fields::new(body) {
            let field = field?;
            let Value::Len(bytes) = field.value else {
                continue;
            };
            match field.number {
                1 => config.poweron = Some(parse_poweron(bytes)?),
                2 => config.standby = Some(parse_standby(bytes)?),
                3 => config.work = Some(parse_work(bytes)?),
                5 => config.display = Some(parse_display(bytes)?),
                _ => {}
            }
        }
        Ok(config)
    }
}

fn parse_poweron(body: &[u8]) -> Result<PowerOnConfiguration, DecodeError> {
    let mut config = PowerOnConfiguration::default();
    for field in Fields::new(body) {
        let field = field?;
        if let (1, Value::Len(bytes)) = (field.number, field.value) {
            config.media_file = text(bytes);
        }
    }
    Ok(config)
}

fn parse_standby(body: &[u8]) -> Result<StandbyConfiguration, DecodeError> {
    let mut config = StandbyConfiguration::default();
    for field in Fields::new(body) {
        let field = field?;
        match (field.number, field.value) {
            (1, value) => config.enable = value.as_u64() == Some(1),
            (2, Value::Len(bytes)) => config.media_file = text(bytes),
            _ => {}
        }
    }
    Ok(config)
}

fn parse_work(body: &[u8]) -> Result<WorkConfiguration, DecodeError> {
    let mut config = WorkConfiguration::default();
    for field in Fields::new(body) {
        let field = field?;
        match (field.number, field.value) {
            (1, value) => {
                if let Some(v) = value.as_u64() {
                    config.media_mode = MediaMode::from(v);
                }
            }
            (2, value) => {
                if let Some(v) = value.as_u64() {
                    config.loop_mode = LoopMode::from(v);
                }
            }
            (3, Value::Len(b)) => config.single_mode_media_file = text(b),
            (4, Value::Len(b)) => config.dual_mode_left_media_file = text(b),
            (5, Value::Len(b)) => config.dual_mode_right_media_file = text(b),
            (6, Value::Len(b)) => config.kaleidoscope_media_file = text(b),
            (7, value) => {
                if let Some(v) = value.as_u64() {
                    config.kaleidoscope_source = v;
                }
            }
            _ => {}
        }
    }
    Ok(config)
}

fn parse_display(body: &[u8]) -> Result<DisplayConfiguration, DecodeError> {
    let mut config = DisplayConfiguration::default();
    for field in Fields::new(body) {
        let field = field?;
        let Some(value) = field.value.as_u64() else {
            continue;
        };
        match field.number {
            1 => config.backlight_enable = value == 1,
            2 => config.backlight_brightness = value,
            3 => config.mirror = value == 1,
            4 => config.ui_rotation = value,
            5 => config.media_rotation = value,
            _ => {}
        }
    }
    Ok(config)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64) -> NonZeroU64 {
        NonZeroU64::new(id).unwrap()
    }

    #[test]
    fn device_information_query_fixture() {
        assert_eq!(
            device_information_query(),
            [0x0a, 0x02, 0x08, 0x01, 0xa2, 0x06, 0x04, 0x0a, 0x02, b'N', b'A']
        );
    }

    #[test]
    fn system_configuration_query_fixture() {
        assert_eq!(
            system_configuration_query(),
            [0x0a, 0x02, 0x08, 0x01, 0xb2, 0x06, 0x04, 0x0a, 0x02, b'N', b'A']
        );
    }

    #[test]
    fn device_authentication_query_fixture() {
        assert_eq!(
            device_authentication_query(),
            [0x0a, 0x02, 0x08, 0x01, 0xaa, 0x06, 0x02, 0x08, 0x01]
        );
    }

    #[test]
    fn keepalive_ping_fixture() {
        assert_eq!(
            keepalive_ping(),
            [0x0a, 0x00, 0x52, 0x08, 0x0a, 0x06, b'h', b'e', b'l', b'l', b'o', b'?']
        );
    }

    #[test]
    fn activation_trigger_fixture() {
        assert_eq!(
            activation_trigger(track(1)),
            [0x0a, 0x04, 0x08, 0x01, 0x10, 0x01, 0xca, 0x0c, 0x00]
        );
        assert_eq!(
            activation_trigger(track(300)),
            [0x0a, 0x05, 0x08, 0x01, 0x10, 0xac, 0x02, 0xca, 0x0c, 0x00]
        );
    }

    #[test]
    fn user_configuration_query_fixture() {
        assert_eq!(
            user_configuration_query(track(2)),
            [0x0a, 0x04, 0x08, 0x01, 0x10, 0x02, 0xc2, 0x06, 0x00]
        );
    }

    #[test]
    fn requests_round_trip_through_the_response_view() {
        let request = activation_trigger(track(9));
        let view = Response::parse(&request).unwrap();
        assert_eq!(
            view.header,
            Some(Header {
                version: 1,
                track_id: 9,
                payload_crc32: 0
            })
        );
        assert_eq!(view.error, None);
        assert_eq!(view.body, None);
    }

    #[test]
    fn response_with_every_part_and_unknown_fields() {
        let info = Message::new()
            .bytes(1, b"Linux")
            .bytes(2, b"5.10")
            .bytes(3, b"1.2.3")
            .bytes(4, b"PASE")
            .bytes(5, b"2.0")
            .bytes(6, b"gap")
            .uint(7, 42)
            .bytes(8, b"SN123")
            .uint(9, 1)
            .bytes(10, b"chip")
            .bytes(11, b"later");
        let payload = Message::new()
            .message(field::HEADER, &Message::new().uint(1, 1).uint(2, 7))
            .message(field::ERROR, &Message::new())
            .uint(3000, 5)
            .message(field::DEVICE_INFORMATION, &info)
            .into_bytes();

        let view = Response::parse(&payload).unwrap();
        assert_eq!(
            view.header,
            Some(Header {
                version: 1,
                track_id: 7,
                payload_crc32: 0
            })
        );
        assert_eq!(view.error, Some(ProtocolError::default()));
        assert_eq!(view.rejection(), None);
        assert_eq!(view.body_number(), Some(field::DEVICE_INFORMATION));

        let parsed = DeviceInformation::parse(view.body.unwrap().1).unwrap();
        assert_eq!(
            parsed,
            DeviceInformation {
                os_name: "Linux".into(),
                os_version: "5.10".into(),
                firmware_version: "1.2.3".into(),
                product_name: "PASE".into(),
                app_version: "2.0".into(),
                serial_number: "SN123".into(),
                serial_number_locked: true,
                chip_id: "chip".into(),
            }
        );
    }

    #[test]
    fn missing_header_and_present_empty_header_are_distinct() {
        let without = Message::new()
            .message(field::PONG, &Message::new().bytes(1, b"hello?"))
            .into_bytes();
        let view = Response::parse(&without).unwrap();
        assert_eq!(view.header, None);
        assert_eq!(view.body_number(), Some(field::PONG));

        let with_empty = keepalive_ping();
        let view = Response::parse(&with_empty).unwrap();
        assert_eq!(view.header, Some(Header::default()));
    }

    #[test]
    fn header_only_response_has_no_body() {
        let payload = Message::new()
            .message(field::HEADER, &Message::new().uint(1, 1).uint(2, 3))
            .into_bytes();
        let view = Response::parse(&payload).unwrap();
        assert_eq!(view.body, None);
        assert_eq!(view.body_number(), None);
    }

    #[test]
    fn rejection_carries_code_and_reason() {
        let payload = Message::new()
            .message(field::HEADER, &Message::new().uint(1, 1).uint(2, 3))
            .message(
                field::ERROR,
                &Message::new().uint(1, 2).bytes(2, b"unsupported body"),
            )
            .into_bytes();
        let view = Response::parse(&payload).unwrap();
        let rejection = view.rejection().unwrap();
        assert_eq!(rejection.code, 2);
        assert_eq!(rejection.why, "unsupported body");
        assert!(!rejection.is_success());
    }

    #[test]
    fn last_body_field_wins() {
        let payload = Message::new()
            .message(field::PONG, &Message::new())
            .message(field::ACKNOWLEDGEMENT, &Message::new())
            .into_bytes();
        let view = Response::parse(&payload).unwrap();
        assert_eq!(view.body_number(), Some(field::ACKNOWLEDGEMENT));
    }

    #[test]
    fn malformed_response_is_an_error() {
        assert_eq!(Response::parse(&[0x0a, 0x09, 0x08]), Err(DecodeError::Truncated));
        let bad_header = [0x0a, 0x01, 0x80];
        assert_eq!(Response::parse(&bad_header), Err(DecodeError::Truncated));
    }

    #[test]
    fn device_authentication_body() {
        let body = Message::new().bytes(1, b"token").uint(2, 1).into_bytes();
        assert_eq!(parse_device_authentication(&body).unwrap(), "token");
        assert_eq!(parse_device_authentication(&[]).unwrap(), "");
    }

    #[test]
    fn user_configuration_sections() {
        let work = Message::new()
            .uint(2, 1)
            .bytes(3, b"wall.mp4")
            .bytes(4, b"left.mp4")
            .bytes(5, b"right.mp4")
            .bytes(6, b"k.mp4")
            .uint(7, 1);
        let display = Message::new()
            .uint(1, 1)
            .uint(2, 75)
            .uint(4, 90)
            .uint(5, 180);
        let body = Message::new()
            .message(1, &Message::new().bytes(1, b"boot.mp4"))
            .message(2, &Message::new().uint(1, 1).bytes(2, b"idle.mp4"))
            .message(3, &work)
            .message(4, &Message::new().uint(1, 1))
            .message(5, &display)
            .bytes(9, b"future")
            .into_bytes();

        let config = UserConfiguration::parse(&body).unwrap();
        assert_eq!(
            config.poweron,
            Some(PowerOnConfiguration {
                media_file: "boot.mp4".into()
            })
        );
        assert_eq!(
            config.standby,
            Some(StandbyConfiguration {
                enable: true,
                media_file: "idle.mp4".into()
            })
        );
        assert_eq!(
            config.work,
            Some(WorkConfiguration {
                media_mode: MediaMode::Single,
                loop_mode: LoopMode::All,
                single_mode_media_file: "wall.mp4".into(),
                dual_mode_left_media_file: "left.mp4".into(),
                dual_mode_right_media_file: "right.mp4".into(),
                kaleidoscope_media_file: "k.mp4".into(),
                kaleidoscope_source: 1,
            })
        );
        assert_eq!(
            config.display,
            Some(DisplayConfiguration {
                backlight_enable: true,
                backlight_brightness: 75,
                mirror: false,
                ui_rotation: 90,
                media_rotation: 180,
            })
        );
    }

    #[test]
    fn user_configuration_absent_sections_stay_none() {
        let body = Message::new().message(5, &Message::new()).into_bytes();
        let config = UserConfiguration::parse(&body).unwrap();
        assert_eq!(config.poweron, None);
        assert_eq!(config.standby, None);
        assert_eq!(config.work, None);
        assert_eq!(config.display, Some(DisplayConfiguration::default()));
        assert_eq!(UserConfiguration::parse(&[]).unwrap(), UserConfiguration::default());
    }

    #[test]
    fn enum_mapping() {
        assert_eq!(MediaMode::from(0), MediaMode::Single);
        assert_eq!(MediaMode::from(1), MediaMode::Dual);
        assert_eq!(MediaMode::from(2), MediaMode::Kaleidoscope);
        assert_eq!(MediaMode::from(9), MediaMode::Unknown(9));
        assert_eq!(LoopMode::from(0), LoopMode::Single);
        assert_eq!(LoopMode::from(1), LoopMode::All);
        assert_eq!(LoopMode::from(2), LoopMode::Random);
        assert_eq!(LoopMode::from(9), LoopMode::Unknown(9));
    }

    #[test]
    fn text_fields_tolerate_invalid_utf8() {
        let body = Message::new().bytes(4, b"PA\xffSE").into_bytes();
        let info = DeviceInformation::parse(&body).unwrap();
        assert_eq!(info.product_name, "PA\u{fffd}SE");
    }
}
