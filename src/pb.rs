//! Minimal protobuf wire encoding and decoding.
//!
//! Encoding follows proto3 canonical serialization: scalars equal to zero and
//! empty byte strings are omitted, while a sub-message that is present is
//! written even when it has no fields.
//!
//! Decoding walks a message field by field without a schema. Unknown fields
//! are yielded like any other so callers can ignore or preserve them.

use std::fmt;

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

/// Largest field number protobuf allows.
pub const MAX_FIELD: u64 = (1 << 29) - 1;
/// Longest varint that fits in 64 bits.
const MAX_VARINT_BYTES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended in the middle of a tag, value, or length-prefixed
    /// field.
    Truncated,
    /// A varint ran past ten bytes or did not fit in 64 bits.
    InvalidVarint,
    /// A tag had field number zero or a number above [`MAX_FIELD`].
    InvalidTag,
    /// A tag used a wire type this decoder does not accept.
    UnknownWireType(u32),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "protobuf message is truncated"),
            DecodeError::InvalidVarint => write!(f, "protobuf varint is invalid"),
            DecodeError::InvalidTag => write!(f, "protobuf field tag is invalid"),
            DecodeError::UnknownWireType(t) => write!(f, "protobuf wire type {t} is not supported"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A decoded field value. Length-delimited values borrow from the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Len(&'a [u8]),
    Fixed32(u32),
}

impl<'a> Value<'a> {
    /// The value as an unsigned integer, for varint and fixed-width fields.
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::Varint(v) | Value::Fixed64(v) => Some(v),
            Value::Fixed32(v) => Some(u64::from(v)),
            Value::Len(_) => None,
        }
    }

    /// The raw bytes of a length-delimited field.
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match *self {
            Value::Len(bytes) => Some(bytes),
            _ => None,
        }
    }
}

/// One field read from a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field<'a> {
    pub number: u32,
    pub value: Value<'a>,
}

/// Reads a varint starting at `*pos`, advancing `*pos` past it.
pub fn get_varint(data: &[u8], pos: &mut usize) -> Result<u64, DecodeError> {
    let mut value = 0u64;
    for index in 0..MAX_VARINT_BYTES {
        let byte = *data.get(*pos).ok_or(DecodeError::Truncated)?;
        *pos += 1;
        let bits = u64::from(byte & 0x7f);
        if index == MAX_VARINT_BYTES - 1 && bits > 1 {
            return Err(DecodeError::InvalidVarint);
        }
        value |= bits << (7 * index);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(DecodeError::InvalidVarint)
}

/// Iterates over the fields of a message in wire order.
///
/// The first error ends the iteration.
#[derive(Debug, Clone)]
pub struct Fields<'a> {
    data: &'a [u8],
    pos: usize,
    failed: bool,
}

impl<'a> Fields<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            failed: false,
        }
    }

    fn read_field(&mut self) -> Result<Field<'a>, DecodeError> {
        let tag = get_varint(self.data, &mut self.pos)?;
        let number = tag >> 3;
        if number == 0 || number > MAX_FIELD {
            return Err(DecodeError::InvalidTag);
        }
        let wire_type = (tag & 0x7) as u32;
        let value = match wire_type {
            WIRE_VARINT => Value::Varint(get_varint(self.data, &mut self.pos)?),
            WIRE_FIXED64 => Value::Fixed64(u64::from_le_bytes(self.take_array()?)),
            WIRE_LEN => {
                let len = get_varint(self.data, &mut self.pos)?;
                let remaining = (self.data.len() - self.pos) as u64;
                if len > remaining {
                    return Err(DecodeError::Truncated);
                }
                let start = self.pos;
                self.pos += len as usize;
                Value::Len(&self.data[start..self.pos])
            }
            WIRE_FIXED32 => Value::Fixed32(u32::from_le_bytes(self.take_array()?)),
            other => return Err(DecodeError::UnknownWireType(other)),
        };
        Ok(Field {
            number: number as u32,
            value,
        })
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let end = self.pos.checked_add(N).ok_or(DecodeError::Truncated)?;
        let slice = self.data.get(self.pos..end).ok_or(DecodeError::Truncated)?;
        self.pos = end;
        let mut array = [0u8; N];
        array.copy_from_slice(slice);
        Ok(array)
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<Field<'a>, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.data.len() {
            return None;
        }
        match self.read_field() {
            Ok(field) => Some(Ok(field)),
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

/// Returns the last occurrence of `number` in `data`, which is the value a
/// schema-aware parser would keep for a non-repeated field. The whole
/// message is walked, so any malformed field is reported even after a match.
pub fn last_field(data: &[u8], number: u32) -> Result<Option<Value<'_>>, DecodeError> {
    let mut found = None;
    for field in Fields::new(data) {
        let field = field?;
        if field.number == number {
            found = Some(field.value);
        }
    }
    Ok(found)
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

    fn walk(data: &[u8]) -> Result<Vec<Field<'_>>, DecodeError> {
        Fields::new(data).collect()
    }

    #[test]
    fn get_varint_reads_and_advances() {
        let data = [0xac, 0x02, 0x7f];
        let mut pos = 0;
        assert_eq!(get_varint(&data, &mut pos), Ok(300));
        assert_eq!(pos, 2);
        assert_eq!(get_varint(&data, &mut pos), Ok(127));
        assert_eq!(pos, 3);
        assert_eq!(get_varint(&data, &mut pos), Err(DecodeError::Truncated));
    }

    #[test]
    fn get_varint_rejects_overlong_and_overflowing_input() {
        let eleven = [0x80u8; 11];
        let mut pos = 0;
        assert_eq!(get_varint(&eleven, &mut pos), Err(DecodeError::InvalidVarint));
        let overflow = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02];
        pos = 0;
        assert_eq!(get_varint(&overflow, &mut pos), Err(DecodeError::InvalidVarint));
        let max = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
        pos = 0;
        assert_eq!(get_varint(&max, &mut pos), Ok(u64::MAX));
    }

    #[test]
    fn get_varint_reports_truncation_mid_value() {
        let mut pos = 0;
        assert_eq!(get_varint(&[0x80], &mut pos), Err(DecodeError::Truncated));
        pos = 0;
        assert_eq!(get_varint(&[], &mut pos), Err(DecodeError::Truncated));
    }

    #[test]
    fn walks_every_wire_type_in_order() {
        let data = [
            0x08, 0x05, // field 1 varint 5
            0x11, 1, 0, 0, 0, 0, 0, 0, 0, // field 2 fixed64 1
            0x1a, 0x02, b'N', b'A', // field 3 len "NA"
            0x25, 7, 0, 0, 0, // field 4 fixed32 7
        ];
        assert_eq!(
            walk(&data).unwrap(),
            vec![
                Field { number: 1, value: Value::Varint(5) },
                Field { number: 2, value: Value::Fixed64(1) },
                Field { number: 3, value: Value::Len(b"NA") },
                Field { number: 4, value: Value::Fixed32(7) },
            ]
        );
    }

    #[test]
    fn empty_message_has_no_fields() {
        assert_eq!(walk(&[]).unwrap(), Vec::new());
    }

    #[test]
    fn nested_messages_walk_by_descending() {
        let inner = Message::new().uint(1, 1).uint(2, 5);
        let outer = Message::new().message(1, &inner).message(201, &Message::new());
        let fields = walk(outer.as_bytes()).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].number, 1);
        let header = fields[0].value.as_bytes().unwrap();
        assert_eq!(
            walk(header).unwrap(),
            vec![
                Field { number: 1, value: Value::Varint(1) },
                Field { number: 2, value: Value::Varint(5) },
            ]
        );
        assert_eq!(fields[1], Field { number: 201, value: Value::Len(b"") });
    }

    #[test]
    fn unknown_fields_are_yielded_in_position() {
        let known_then_unknown_then_known = Message::new()
            .uint(1, 1)
            .bytes(99, b"future")
            .uint(1000, 3)
            .uint(2, 2);
        let fields = walk(known_then_unknown_then_known.as_bytes()).unwrap();
        let numbers: Vec<u32> = fields.iter().map(|f| f.number).collect();
        assert_eq!(numbers, [1, 99, 1000, 2]);
        assert_eq!(fields[1].value, Value::Len(b"future"));
        assert_eq!(fields[2].value, Value::Varint(3));
    }

    #[test]
    fn truncated_length_delimited_field_is_an_error() {
        assert_eq!(walk(&[0x0a, 0x05, b'a', b'b']), Err(DecodeError::Truncated));
    }

    #[test]
    fn truncated_scalars_are_errors() {
        assert_eq!(walk(&[0x08]), Err(DecodeError::Truncated));
        assert_eq!(walk(&[0x08, 0x80]), Err(DecodeError::Truncated));
        assert_eq!(walk(&[0x11, 1, 2, 3]), Err(DecodeError::Truncated));
        assert_eq!(walk(&[0x25, 1, 2]), Err(DecodeError::Truncated));
    }

    #[test]
    fn invalid_tags_are_errors() {
        assert_eq!(walk(&[0x00]), Err(DecodeError::InvalidTag));
        let too_large = [0x80, 0x80, 0x80, 0x80, 0x10];
        assert_eq!(walk(&too_large), Err(DecodeError::InvalidTag));
    }

    #[test]
    fn deprecated_and_reserved_wire_types_are_errors() {
        for wire_type in [3u32, 4, 6, 7] {
            let tag = (1 << 3) | wire_type as u8;
            assert_eq!(walk(&[tag, 0x00]), Err(DecodeError::UnknownWireType(wire_type)));
        }
    }

    #[test]
    fn iteration_stops_after_the_first_error() {
        let mut fields = Fields::new(&[0x0a, 0x05, b'x', 0x08, 0x01]);
        assert_eq!(fields.next(), Some(Err(DecodeError::Truncated)));
        assert_eq!(fields.next(), None);
    }

    #[test]
    fn last_field_keeps_the_final_occurrence() {
        let data = Message::new().uint(1, 1).uint(1, 2).bytes(2, b"x");
        assert_eq!(last_field(data.as_bytes(), 1), Ok(Some(Value::Varint(2))));
        assert_eq!(last_field(data.as_bytes(), 2), Ok(Some(Value::Len(b"x"))));
        assert_eq!(last_field(data.as_bytes(), 3), Ok(None));
    }

    #[test]
    fn last_field_reports_errors_after_a_match() {
        let data = [0x08, 0x01, 0x0a, 0x09, b'x'];
        assert_eq!(last_field(&data, 1), Err(DecodeError::Truncated));
    }

    #[test]
    fn value_accessors() {
        assert_eq!(Value::Varint(9).as_u64(), Some(9));
        assert_eq!(Value::Fixed64(9).as_u64(), Some(9));
        assert_eq!(Value::Fixed32(9).as_u64(), Some(9));
        assert_eq!(Value::Len(b"9").as_u64(), None);
        assert_eq!(Value::Len(b"9").as_bytes(), Some(&b"9"[..]));
        assert_eq!(Value::Varint(9).as_bytes(), None);
    }

    #[test]
    fn writer_and_reader_round_trip() {
        let request = Message::new()
            .message(1, &Message::new().uint(1, 1).uint(2, 7))
            .message(104, &Message::new());
        let fields = walk(request.as_bytes()).unwrap();
        assert_eq!(fields[0].number, 1);
        assert_eq!(
            last_field(fields[0].value.as_bytes().unwrap(), 2),
            Ok(Some(Value::Varint(7)))
        );
        assert_eq!(fields[1], Field { number: 104, value: Value::Len(b"") });
    }
}
