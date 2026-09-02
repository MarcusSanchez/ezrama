//! Minimal protobuf wire encoding.
//!
//! Follows proto3 canonical serialization: scalars equal to zero and empty
//! byte strings are omitted, while a sub-message that is present is written
//! even when it has no fields.

/// Wire type for varint-encoded scalars.
pub const WIRE_VARINT: u32 = 0;
/// Wire type for 64-bit fixed-width values.
pub const WIRE_FIXED64: u32 = 1;
/// Wire type for length-delimited values: bytes, strings, sub-messages.
pub const WIRE_LEN: u32 = 2;
/// Wire type for 32-bit fixed-width values.
pub const WIRE_FIXED32: u32 = 5;

/// Appends `value` as a base-128 varint.
pub fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Appends the tag for `field` with the given wire type.
pub fn put_tag(out: &mut Vec<u8>, field: u32, wire_type: u32) {
    put_varint(out, (u64::from(field) << 3) | u64::from(wire_type & 0x7));
}

/// Appends a varint field. Zero is the proto3 default and is not written.
pub fn put_uint(out: &mut Vec<u8>, field: u32, value: u64) {
    if value == 0 {
        return;
    }
    put_tag(out, field, WIRE_VARINT);
    put_varint(out, value);
}

/// Appends a length-delimited field. An empty value is the proto3 default
/// and is not written.
pub fn put_bytes(out: &mut Vec<u8>, field: u32, value: &[u8]) {
    if value.is_empty() {
        return;
    }
    put_len(out, field, value);
}

/// Appends a sub-message field. Written even when `body` is empty, which is
/// how a present but default-valued message appears on the wire.
pub fn put_message(out: &mut Vec<u8>, field: u32, body: &[u8]) {
    put_len(out, field, body);
}

fn put_len(out: &mut Vec<u8>, field: u32, value: &[u8]) {
    put_tag(out, field, WIRE_LEN);
    put_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

/// Builds a message body field by field.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Message {
    buf: Vec<u8>,
}

impl Message {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a varint field; zero is omitted.
    pub fn uint(mut self, field: u32, value: u64) -> Self {
        put_uint(&mut self.buf, field, value);
        self
    }

    /// Adds a bytes or string field; empty is omitted.
    pub fn bytes(mut self, field: u32, value: &[u8]) -> Self {
        put_bytes(&mut self.buf, field, value);
        self
    }

    /// Adds a sub-message field; written even when empty.
    pub fn message(mut self, field: u32, inner: &Message) -> Self {
        put_message(&mut self.buf, field, &inner.buf);
        self
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, value);
        out
    }

    fn tag(field: u32, wire_type: u32) -> Vec<u8> {
        let mut out = Vec::new();
        put_tag(&mut out, field, wire_type);
        out
    }

    #[test]
    fn varint_edge_values() {
        assert_eq!(varint(0), [0x00]);
        assert_eq!(varint(1), [0x01]);
        assert_eq!(varint(127), [0x7f]);
        assert_eq!(varint(128), [0x80, 0x01]);
        assert_eq!(varint(300), [0xac, 0x02]);
        assert_eq!(varint(16_383), [0xff, 0x7f]);
        assert_eq!(varint(16_384), [0x80, 0x80, 0x01]);
        assert_eq!(
            varint(u64::from(u32::MAX)),
            [0xff, 0xff, 0xff, 0xff, 0x0f]
        );
        assert_eq!(
            varint(u64::MAX),
            [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]
        );
    }

    #[test]
    fn tags_for_the_fields_in_use() {
        assert_eq!(tag(1, WIRE_VARINT), [0x08]);
        assert_eq!(tag(1, WIRE_LEN), [0x0a]);
        assert_eq!(tag(2, WIRE_VARINT), [0x10]);
        assert_eq!(tag(10, WIRE_LEN), [0x52]);
        assert_eq!(tag(100, WIRE_LEN), [0xa2, 0x06]);
        assert_eq!(tag(101, WIRE_LEN), [0xaa, 0x06]);
        assert_eq!(tag(102, WIRE_LEN), [0xb2, 0x06]);
        assert_eq!(tag(104, WIRE_LEN), [0xc2, 0x06]);
        assert_eq!(tag(201, WIRE_LEN), [0xca, 0x0c]);
    }

    #[test]
    fn tag_masks_wire_type_to_three_bits() {
        assert_eq!(tag(1, 0xff), [0x0f]);
    }

    #[test]
    fn uint_field_omits_zero() {
        let mut out = Vec::new();
        put_uint(&mut out, 3, 0);
        assert!(out.is_empty());
        put_uint(&mut out, 3, 1);
        assert_eq!(out, [0x18, 0x01]);
    }

    #[test]
    fn bytes_field_omits_empty() {
        let mut out = Vec::new();
        put_bytes(&mut out, 1, b"");
        assert!(out.is_empty());
        put_bytes(&mut out, 1, b"NA");
        assert_eq!(out, [0x0a, 0x02, b'N', b'A']);
    }

    #[test]
    fn message_field_writes_empty_body() {
        let mut out = Vec::new();
        put_message(&mut out, 1, &[]);
        assert_eq!(out, [0x0a, 0x00]);
        put_message(&mut out, 201, &[]);
        assert_eq!(out, [0x0a, 0x00, 0xca, 0x0c, 0x00]);
    }

    #[test]
    fn builder_nests_messages() {
        let header = Message::new().uint(1, 1).uint(2, 5).uint(3, 0);
        assert_eq!(header.as_bytes(), [0x08, 0x01, 0x10, 0x05]);
        let request = Message::new().message(1, &header);
        assert_eq!(request.into_bytes(), [0x0a, 0x04, 0x08, 0x01, 0x10, 0x05]);
    }

    #[test]
    fn builder_writes_present_empty_submessage() {
        let request = Message::new()
            .message(1, &Message::new())
            .message(10, &Message::new().bytes(1, b"hello?"));
        assert_eq!(
            request.as_bytes(),
            [0x0a, 0x00, 0x52, 0x08, 0x0a, 0x06, b'h', b'e', b'l', b'l', b'o', b'?']
        );
    }

    #[test]
    fn builder_length_prefix_uses_varint() {
        let big = vec![0x55; 300];
        let message = Message::new().bytes(7, &big);
        let bytes = message.as_bytes();
        assert_eq!(&bytes[..3], [0x3a, 0xac, 0x02]);
        assert_eq!(bytes.len(), 3 + 300);
    }
}
