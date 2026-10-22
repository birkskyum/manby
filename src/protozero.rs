//! The parts of protozero (varints, zigzag, pbf reader and writer) that mvt.cpp
//! and geobuf.cpp use.
//!
//! protozero reports malformed input by throwing an exception. Here every
//! reader call that can throw returns a `Result` whose error is the
//! `Exception` the C++ would have thrown, so callers that catch
//! `std::exception` can match on `Err` instead.
//!
//! tippecanoe builds with `NDEBUG`, so the `protozero_assert` checks (wire
//! types, calling `next()` first) are not compiled in, and they are left out
//! here too.

use std::fmt;
use std::marker::PhantomData;

/// The protozero exceptions, all derived from `protozero::exception`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exception {
    /// `varint_too_long_exception`
    VarintTooLong,
    /// `unknown_pbf_wire_type_exception`
    UnknownPbfWireType,
    /// `end_of_buffer_exception`
    EndOfBuffer,
    /// `invalid_tag_exception`
    InvalidTag,
    /// `invalid_length_exception`
    InvalidLength,
}

impl Exception {
    /// `what()`
    pub fn what(&self) -> &'static str {
        match self {
            Exception::VarintTooLong => "varint too long exception",
            Exception::UnknownPbfWireType => "unknown pbf field type exception",
            Exception::EndOfBuffer => "end of buffer exception",
            Exception::InvalidTag => "invalid tag exception",
            Exception::InvalidLength => "invalid length exception",
        }
    }
}

impl fmt::Display for Exception {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.what())
    }
}

impl std::error::Error for Exception {}

pub type Result<T> = std::result::Result<T, Exception>;

// types.hpp

/// The type used for field tags (field numbers).
pub type PbfTagType = u32;

/// The type used to encode type information.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PbfWireType {
    /// int32/64, uint32/64, sint32/64, bool, enum
    Varint = 0,
    /// fixed64, sfixed64, double
    Fixed64 = 1,
    /// string, bytes, nested messages, packed repeated fields
    LengthDelimited = 2,
    /// fixed32, sfixed32, float
    Fixed32 = 5,
    /// used for default setting in this library
    Unknown = 99,
}

/// Get the tag and wire type of the current field in one integer suitable
/// for comparison with a switch statement.
pub const fn tag_and_type(tag: PbfTagType, wire_type: PbfWireType) -> u32 {
    (tag << 3) | wire_type as u32
}

/// The type used for length values, such as the length of a field.
pub type PbfLengthType = u32;

// varint.hpp

/// The maximum length of a 64 bit varint.
pub const MAX_VARINT_LENGTH: i8 = (u64::BITS / 7 + 1) as i8;

// from https://github.com/facebook/folly/blob/master/folly/Varint.h
fn decode_varint_impl(data: &mut &[u8]) -> Result<u64> {
    let d = *data;
    let mut p = 0;
    let mut val: u64 = 0;

    if d.len() >= MAX_VARINT_LENGTH as usize {
        // fast path
        let mut shift = 0;
        loop {
            let b = d[p] as i8 as i64;
            p += 1;
            if shift == 63 {
                val |= ((b as u64) & 0x01) << 63;
            } else {
                val |= ((b as u64) & 0x7f) << shift;
            }
            if b >= 0 {
                break;
            }
            if shift == 63 {
                return Err(Exception::VarintTooLong);
            }
            shift += 7;
        }
    } else {
        let mut shift = 0;
        while p != d.len() && (d[p] as i8) < 0 {
            val |= ((d[p] as u64) & 0x7f) << shift;
            p += 1;
            shift += 7;
        }
        if p == d.len() {
            return Err(Exception::EndOfBuffer);
        }
        val |= (d[p] as u64) << shift;
        p += 1;
    }

    *data = &d[p..];
    Ok(val)
}

/// Decode a 64 bit varint. Advances `data` past the varint on success and
/// leaves it alone on error.
pub fn decode_varint(data: &mut &[u8]) -> Result<u64> {
    // If this is a one-byte varint, decode it here.
    if let Some(&first) = data.first()
        && (first as u64 & 0x80) == 0
    {
        *data = &data[1..];
        return Ok(first as u64);
    }
    // If this varint is more than one byte, defer to complete implementation.
    decode_varint_impl(data)
}

/// Skip over a varint.
pub fn skip_varint(data: &mut &[u8]) -> Result<()> {
    let d = *data;
    let mut p = 0;

    while p != d.len() && (d[p] as i8) < 0 {
        p += 1;
    }

    if p >= MAX_VARINT_LENGTH as usize {
        return Err(Exception::VarintTooLong);
    }

    if p == d.len() {
        return Err(Exception::EndOfBuffer);
    }

    p += 1;

    *data = &d[p..];
    Ok(())
}

/// Varint encode a 64 bit integer and append it to the buffer. Returns the
/// number of bytes written, like `write_varint`.
pub fn add_varint_to_buffer(buffer: &mut Vec<u8>, mut value: u64) -> i32 {
    let mut n = 1;

    while value >= 0x80 {
        buffer.push(((value & 0x7f) | 0x80) as u8);
        value >>= 7;
        n += 1;
    }
    buffer.push(value as u8);

    n
}

/// Get the length of the varint the specified value would produce.
pub fn length_of_varint(mut value: u64) -> i32 {
    let mut n = 1;

    while value >= 0x80 {
        value >>= 7;
        n += 1;
    }

    n
}

/// ZigZag encodes a 32 bit integer.
pub const fn encode_zigzag32(value: i32) -> u32 {
    ((value as u32) << 1) ^ ((-(((value as u32) >> 31) as i32)) as u32)
}

/// ZigZag encodes a 64 bit integer.
pub const fn encode_zigzag64(value: i64) -> u64 {
    ((value as u64) << 1) ^ ((-(((value as u64) >> 63) as i64)) as u64)
}

/// Decodes a 32 bit ZigZag-encoded integer.
pub const fn decode_zigzag32(value: u32) -> i32 {
    ((value >> 1) ^ ((-((value & 1) as i32)) as u32)) as i32
}

/// Decodes a 64 bit ZigZag-encoded integer.
pub const fn decode_zigzag64(value: u64) -> i64 {
    ((value >> 1) ^ ((-((value & 1) as i64)) as u64)) as i64
}

// iterators.hpp

/// A value type that a packed varint field can hold, converted from the raw
/// varint the way `const_varint_iterator` and `const_svarint_iterator` do.
pub trait PackedVarintValue: Copy {
    fn from_varint(v: u64) -> Self;
}

impl PackedVarintValue for i32 {
    fn from_varint(v: u64) -> i32 {
        v as i32
    }
}

impl PackedVarintValue for u32 {
    fn from_varint(v: u64) -> u32 {
        v as u32
    }
}

impl PackedVarintValue for i64 {
    fn from_varint(v: u64) -> i64 {
        v as i64
    }
}

impl PackedVarintValue for u64 {
    fn from_varint(v: u64) -> u64 {
        v
    }
}

/// A packed field of varints, the `iterator_range` that `get_packed_*`
/// returns. Iterating decodes one varint at a time, so a malformed varint
/// shows up as an `Err` item, after which the iteration stops.
#[derive(Clone, Copy, Debug)]
pub struct PackedVarint<'a, T> {
    data: &'a [u8],
    zigzag: bool,
    _marker: PhantomData<T>,
}

impl<'a, T: PackedVarintValue> PackedVarint<'a, T> {
    fn new(data: &'a [u8], zigzag: bool) -> PackedVarint<'a, T> {
        PackedVarint {
            data,
            zigzag,
            _marker: PhantomData,
        }
    }

    /// `std::distance(first, last)`: the number of bytes that end a varint.
    pub fn size(&self) -> usize {
        self.data.iter().filter(|&&c| (c & 0x80) == 0).count()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl<T: PackedVarintValue> Iterator for PackedVarint<'_, T> {
    type Item = Result<T>;

    fn next(&mut self) -> Option<Result<T>> {
        if self.data.is_empty() {
            return None;
        }
        match decode_varint(&mut self.data) {
            Ok(v) => {
                if self.zigzag {
                    Some(Ok(T::from_varint(decode_zigzag64(v) as u64)))
                } else {
                    Some(Ok(T::from_varint(v)))
                }
            }
            Err(e) => {
                self.data = &[];
                Some(Err(e))
            }
        }
    }
}

/// A value type that a packed fixed-size field can hold.
pub trait PackedFixedValue: Copy {
    const SIZE: usize;
    fn from_le_slice(b: &[u8]) -> Self;
}

macro_rules! packed_fixed_value {
    ($t:ty) => {
        impl PackedFixedValue for $t {
            const SIZE: usize = std::mem::size_of::<$t>();
            fn from_le_slice(b: &[u8]) -> $t {
                <$t>::from_le_bytes(b.try_into().unwrap())
            }
        }
    };
}

packed_fixed_value!(u32);
packed_fixed_value!(i32);
packed_fixed_value!(u64);
packed_fixed_value!(i64);
packed_fixed_value!(f32);
packed_fixed_value!(f64);

/// A packed field of fixed-size values, iterated like `const_fixed_iterator`.
#[derive(Clone, Copy, Debug)]
pub struct PackedFixed<'a, T> {
    data: &'a [u8],
    _marker: PhantomData<T>,
}

impl<T: PackedFixedValue> PackedFixed<'_, T> {
    pub fn size(&self) -> usize {
        self.data.len() / T::SIZE
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl<T: PackedFixedValue> Iterator for PackedFixed<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.data.is_empty() {
            return None;
        }
        let v = T::from_le_slice(&self.data[..T::SIZE]);
        self.data = &self.data[T::SIZE..];
        Some(v)
    }
}

// pbf_reader.hpp

/// This class represents a protobuf message. Either a top-level message or
/// a nested sub-message. Top-level messages can be created from any buffer
/// with a pointer and length. A reader for a nested message comes from
/// `get_message()`.
///
/// The reader borrows the buffer, which has to outlive it, and is cheap to
/// copy, like the C++ class.
#[derive(Clone, Copy, Debug)]
pub struct PbfReader<'a> {
    data: &'a [u8],
    wire_type: PbfWireType,
    tag: PbfTagType,
}

impl Default for PbfReader<'_> {
    fn default() -> Self {
        PbfReader {
            data: &[],
            wire_type: PbfWireType::Unknown,
            tag: 0,
        }
    }
}

impl<'a> PbfReader<'a> {
    fn get_fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let data = self.data;
        self.skip_bytes(N as PbfLengthType)?;
        Ok(data[..N].try_into().unwrap())
    }

    fn packed_fixed<T: PackedFixedValue>(&mut self) -> Result<PackedFixed<'a, T>> {
        let before = self.data;
        let len = self.get_len_and_skip()?;
        if len as usize % T::SIZE != 0 {
            return Err(Exception::InvalidLength);
        }
        Ok(PackedFixed {
            data: self.skipped(before, len),
            _marker: PhantomData,
        })
    }

    fn get_varint(&mut self) -> Result<u64> {
        decode_varint(&mut self.data)
    }

    fn get_svarint(&mut self) -> Result<i64> {
        Ok(decode_zigzag64(decode_varint(&mut self.data)?))
    }

    fn get_length(&mut self) -> Result<PbfLengthType> {
        Ok(self.get_varint()? as PbfLengthType)
    }

    fn skip_bytes(&mut self, len: PbfLengthType) -> Result<()> {
        if self.data.len() < len as usize {
            return Err(Exception::EndOfBuffer);
        }
        self.data = &self.data[len as usize..];
        Ok(())
    }

    fn get_len_and_skip(&mut self) -> Result<PbfLengthType> {
        let len = self.get_length()?;
        self.skip_bytes(len)?;
        Ok(len)
    }

    /// The bytes of the length-delimited field just skipped by
    /// `get_len_and_skip()`, given the buffer as it was before.
    fn skipped(&self, before: &'a [u8], len: PbfLengthType) -> &'a [u8] {
        let end = before.len() - self.data.len();
        &before[end - len as usize..end]
    }

    fn get_packed<T: PackedVarintValue>(&mut self, zigzag: bool) -> Result<PackedVarint<'a, T>> {
        let before = self.data;
        let len = self.get_len_and_skip()?;
        Ok(PackedVarint::new(self.skipped(before, len), zigzag))
    }

    /// Construct a pbf_reader message from a data pointer and a length. The
    /// pointer will be stored inside the pbf_reader object, no data is
    /// copied. So you must make sure the buffer stays valid as long as the
    /// pbf_reader object is used.
    pub fn new(data: &'a [u8]) -> PbfReader<'a> {
        PbfReader {
            data,
            wire_type: PbfWireType::Unknown,
            tag: 0,
        }
    }

    /// In a boolean context the pbf_reader class evaluates to `true` if
    /// there are still fields available and to `false` if the last field
    /// has been read.
    pub fn as_bool(&self) -> bool {
        !self.data.is_empty()
    }

    /// Get a view of the not yet read data.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Return the length in bytes of the current message. If you have
    /// already called next() and/or any of the get_*() functions, this will
    /// return the remaining length.
    pub fn length(&self) -> usize {
        self.data.len()
    }

    /// Set next field in the message as the current field. This is usually
    /// called in a while loop:
    ///
    /// ```ignore
    /// while message.next()? {
    ///     // handle field
    /// }
    /// ```
    ///
    /// Returns `true` if there is a next field, `false` if not.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<bool> {
        if self.data.is_empty() {
            return Ok(false);
        }

        let value = self.get_varint()? as u32;
        self.tag = value >> 3;

        // tags 0 and 19000 to 19999 are not allowed as per
        // https://developers.google.com/protocol-buffers/docs/proto#assigning-tags
        if self.tag == 0 || (19000..=19999).contains(&self.tag) {
            return Err(Exception::InvalidTag);
        }

        self.wire_type = match value & 0x07 {
            0 => PbfWireType::Varint,
            1 => PbfWireType::Fixed64,
            2 => PbfWireType::LengthDelimited,
            5 => PbfWireType::Fixed32,
            _ => return Err(Exception::UnknownPbfWireType),
        };

        Ok(true)
    }

    /// Set next field with given tag in the message as the current field.
    /// Fields with other tags are skipped.
    pub fn next_tag(&mut self, next_tag: PbfTagType) -> Result<bool> {
        while self.next()? {
            if self.tag == next_tag {
                return Ok(true);
            }
            self.skip()?;
        }
        Ok(false)
    }

    /// Set next field with given tag and wire type in the message as the
    /// current field. Fields with other tags are skipped.
    pub fn next_tag_type(&mut self, next_tag: PbfTagType, type_: PbfWireType) -> Result<bool> {
        while self.next()? {
            if self.tag == next_tag && self.wire_type == type_ {
                return Ok(true);
            }
            self.skip()?;
        }
        Ok(false)
    }

    /// The tag of the current field. The tag is the field number from the
    /// description in the .proto file.
    pub fn tag(&self) -> PbfTagType {
        self.tag
    }

    /// Get the wire type of the current field.
    pub fn wire_type(&self) -> PbfWireType {
        self.wire_type
    }

    /// Get the tag and wire type of the current field in one integer
    /// suitable for comparison with a switch statement.
    pub fn tag_and_type(&self) -> u32 {
        tag_and_type(self.tag, self.wire_type)
    }

    /// Check the wire type of the current field.
    pub fn has_wire_type(&self, type_: PbfWireType) -> bool {
        self.wire_type == type_
    }

    /// Consume the current field.
    pub fn skip(&mut self) -> Result<()> {
        match self.wire_type {
            PbfWireType::Varint => skip_varint(&mut self.data),
            PbfWireType::Fixed64 => self.skip_bytes(8),
            PbfWireType::LengthDelimited => {
                let len = self.get_length()?;
                self.skip_bytes(len)
            }
            PbfWireType::Fixed32 => self.skip_bytes(4),
            PbfWireType::Unknown => Ok(()),
        }
    }

    // [[scalar field access functions]]

    /// Consume and return value of current "bool" field.
    pub fn get_bool(&mut self) -> Result<bool> {
        // The C++ reads the byte before checking that it is there.
        let result = self.data.first().is_some_and(|&b| b != 0);
        skip_varint(&mut self.data)?;
        Ok(result)
    }

    /// Consume and return value of current "enum" field.
    pub fn get_enum(&mut self) -> Result<i32> {
        Ok(self.get_varint()? as i32)
    }

    /// Consume and return value of current "int32" varint field.
    pub fn get_int32(&mut self) -> Result<i32> {
        Ok(self.get_varint()? as i32)
    }

    /// Consume and return value of current "sint32" varint field.
    pub fn get_sint32(&mut self) -> Result<i32> {
        Ok(self.get_svarint()? as i32)
    }

    /// Consume and return value of current "uint32" varint field.
    pub fn get_uint32(&mut self) -> Result<u32> {
        Ok(self.get_varint()? as u32)
    }

    /// Consume and return value of current "int64" varint field.
    pub fn get_int64(&mut self) -> Result<i64> {
        Ok(self.get_varint()? as i64)
    }

    /// Consume and return value of current "sint64" varint field.
    pub fn get_sint64(&mut self) -> Result<i64> {
        self.get_svarint()
    }

    /// Consume and return value of current "uint64" varint field.
    pub fn get_uint64(&mut self) -> Result<u64> {
        self.get_varint()
    }

    /// Consume and return value of current "fixed32" field.
    pub fn get_fixed32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.get_fixed::<4>()?))
    }

    /// Consume and return value of current "sfixed32" field.
    pub fn get_sfixed32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.get_fixed::<4>()?))
    }

    /// Consume and return value of current "fixed64" field.
    pub fn get_fixed64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.get_fixed::<8>()?))
    }

    /// Consume and return value of current "sfixed64" field.
    pub fn get_sfixed64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.get_fixed::<8>()?))
    }

    /// Consume and return value of current "float" field.
    pub fn get_float(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.get_fixed::<4>()?))
    }

    /// Consume and return value of current "double" field.
    pub fn get_double(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.get_fixed::<8>()?))
    }

    /// Consume and return value of current "bytes", "string", or "message"
    /// field.
    pub fn get_view(&mut self) -> Result<&'a [u8]> {
        let before = self.data;
        let len = self.get_len_and_skip()?;
        Ok(self.skipped(before, len))
    }

    /// Consume and return value of current "bytes" field.
    pub fn get_bytes(&mut self) -> Result<Vec<u8>> {
        Ok(self.get_view()?.to_vec())
    }

    /// Consume and return value of current "string" field. A Rust `String`
    /// has to be UTF-8, so invalid sequences become U+FFFD here, where the
    /// C++ `std::string` keeps the bytes. Use `get_bytes` to keep them.
    pub fn get_string(&mut self) -> Result<String> {
        Ok(String::from_utf8_lossy(self.get_view()?).into_owned())
    }

    /// Consume the current "message" field and return a reader for it.
    pub fn get_message(&mut self) -> Result<PbfReader<'a>> {
        Ok(PbfReader::new(self.get_view()?))
    }

    // [[repeated packed field access functions]]

    /// Consume current "repeated packed bool" field.
    pub fn get_packed_bool(&mut self) -> Result<PackedVarint<'a, i32>> {
        self.get_packed(false)
    }

    /// Consume current "repeated packed enum" field.
    pub fn get_packed_enum(&mut self) -> Result<PackedVarint<'a, i32>> {
        self.get_packed(false)
    }

    /// Consume current "repeated packed int32" field.
    pub fn get_packed_int32(&mut self) -> Result<PackedVarint<'a, i32>> {
        self.get_packed(false)
    }

    /// Consume current "repeated packed sint32" field.
    pub fn get_packed_sint32(&mut self) -> Result<PackedVarint<'a, i32>> {
        self.get_packed(true)
    }

    /// Consume current "repeated packed uint32" field.
    pub fn get_packed_uint32(&mut self) -> Result<PackedVarint<'a, u32>> {
        self.get_packed(false)
    }

    /// Consume current "repeated packed int64" field.
    pub fn get_packed_int64(&mut self) -> Result<PackedVarint<'a, i64>> {
        self.get_packed(false)
    }

    /// Consume current "repeated packed sint64" field.
    pub fn get_packed_sint64(&mut self) -> Result<PackedVarint<'a, i64>> {
        self.get_packed(true)
    }

    /// Consume current "repeated packed uint64" field.
    pub fn get_packed_uint64(&mut self) -> Result<PackedVarint<'a, u64>> {
        self.get_packed(false)
    }

    /// Consume current "repeated packed fixed32" field.
    pub fn get_packed_fixed32(&mut self) -> Result<PackedFixed<'a, u32>> {
        self.packed_fixed()
    }

    /// Consume current "repeated packed sfixed32" field.
    pub fn get_packed_sfixed32(&mut self) -> Result<PackedFixed<'a, i32>> {
        self.packed_fixed()
    }

    /// Consume current "repeated packed fixed64" field.
    pub fn get_packed_fixed64(&mut self) -> Result<PackedFixed<'a, u64>> {
        self.packed_fixed()
    }

    /// Consume current "repeated packed sfixed64" field.
    pub fn get_packed_sfixed64(&mut self) -> Result<PackedFixed<'a, i64>> {
        self.packed_fixed()
    }

    /// Consume current "repeated packed float" field.
    pub fn get_packed_float(&mut self) -> Result<PackedFixed<'a, f32>> {
        self.packed_fixed()
    }

    /// Consume current "repeated packed double" field.
    pub fn get_packed_double(&mut self) -> Result<PackedFixed<'a, f64>> {
        self.packed_fixed()
    }
}

// basic_pbf_writer.hpp

/// The pbf_writer is used to write PBF formatted messages into a buffer.
///
/// protozero writes packed fields through a nested writer that reserves room
/// for the length and closes the gap afterwards. The bytes that come out are
/// the field key, the varint length and the contents, which is what this
/// writer produces directly.
pub struct PbfWriter<'a> {
    data: &'a mut Vec<u8>,
}

impl<'a> PbfWriter<'a> {
    fn add_varint(&mut self, value: u64) {
        add_varint_to_buffer(self.data, value);
    }

    fn add_field(&mut self, tag: PbfTagType, type_: PbfWireType) {
        let b: u32 = (tag << 3) | type_ as u32;
        self.add_varint(b as u64);
    }

    fn add_tagged_varint(&mut self, tag: PbfTagType, value: u64) {
        self.add_field(tag, PbfWireType::Varint);
        self.add_varint(value);
    }

    fn add_packed_varint(&mut self, tag: PbfTagType, values: impl IntoIterator<Item = u64>) {
        let mut values = values.into_iter().peekable();
        if values.peek().is_none() {
            return;
        }

        let mut sw = Vec::new();
        for v in values {
            add_varint_to_buffer(&mut sw, v);
        }
        self.add_length_varint(tag, sw.len() as PbfLengthType);
        self.data.extend_from_slice(&sw);
    }

    fn add_length_varint(&mut self, tag: PbfTagType, length: PbfLengthType) {
        self.add_field(tag, PbfWireType::LengthDelimited);
        self.add_varint(length as u64);
    }

    /// Create a writer using the given buffer. Data is appended to it.
    pub fn new(buffer: &'a mut Vec<u8>) -> PbfWriter<'a> {
        PbfWriter { data: buffer }
    }

    // [[scalar field writer functions]]

    /// Add "bool" field to data.
    pub fn add_bool(&mut self, tag: PbfTagType, value: bool) {
        self.add_field(tag, PbfWireType::Varint);
        self.data.push(value as u8);
    }

    /// Add "enum" field to data.
    pub fn add_enum(&mut self, tag: PbfTagType, value: i32) {
        self.add_tagged_varint(tag, value as i64 as u64);
    }

    /// Add "int32" field to data.
    pub fn add_int32(&mut self, tag: PbfTagType, value: i32) {
        self.add_tagged_varint(tag, value as i64 as u64);
    }

    /// Add "sint32" field to data.
    pub fn add_sint32(&mut self, tag: PbfTagType, value: i32) {
        self.add_tagged_varint(tag, encode_zigzag32(value) as u64);
    }

    /// Add "uint32" field to data.
    pub fn add_uint32(&mut self, tag: PbfTagType, value: u32) {
        self.add_tagged_varint(tag, value as u64);
    }

    /// Add "int64" field to data.
    pub fn add_int64(&mut self, tag: PbfTagType, value: i64) {
        self.add_tagged_varint(tag, value as u64);
    }

    /// Add "sint64" field to data.
    pub fn add_sint64(&mut self, tag: PbfTagType, value: i64) {
        self.add_tagged_varint(tag, encode_zigzag64(value));
    }

    /// Add "uint64" field to data.
    pub fn add_uint64(&mut self, tag: PbfTagType, value: u64) {
        self.add_tagged_varint(tag, value);
    }

    /// Add "fixed32" field to data.
    pub fn add_fixed32(&mut self, tag: PbfTagType, value: u32) {
        self.add_field(tag, PbfWireType::Fixed32);
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Add "sfixed32" field to data.
    pub fn add_sfixed32(&mut self, tag: PbfTagType, value: i32) {
        self.add_field(tag, PbfWireType::Fixed32);
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Add "fixed64" field to data.
    pub fn add_fixed64(&mut self, tag: PbfTagType, value: u64) {
        self.add_field(tag, PbfWireType::Fixed64);
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Add "sfixed64" field to data.
    pub fn add_sfixed64(&mut self, tag: PbfTagType, value: i64) {
        self.add_field(tag, PbfWireType::Fixed64);
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Add "float" field to data.
    pub fn add_float(&mut self, tag: PbfTagType, value: f32) {
        self.add_field(tag, PbfWireType::Fixed32);
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Add "double" field to data.
    pub fn add_double(&mut self, tag: PbfTagType, value: f64) {
        self.add_field(tag, PbfWireType::Fixed64);
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Add "bytes" field to data.
    pub fn add_bytes(&mut self, tag: PbfTagType, value: &[u8]) {
        self.add_length_varint(tag, value.len() as PbfLengthType);
        self.data.extend_from_slice(value);
    }

    /// Add "string" field to data.
    pub fn add_string(&mut self, tag: PbfTagType, value: &[u8]) {
        self.add_bytes(tag, value);
    }

    /// Add "message" field to data.
    pub fn add_message(&mut self, tag: PbfTagType, value: &[u8]) {
        self.add_bytes(tag, value);
    }

    // [[packed field writer functions]]

    /// Add "repeated packed bool" field to data.
    pub fn add_packed_bool(&mut self, tag: PbfTagType, values: impl IntoIterator<Item = bool>) {
        self.add_packed_varint(tag, values.into_iter().map(|v| v as u64));
    }

    /// Add "repeated packed enum" field to data.
    pub fn add_packed_enum(&mut self, tag: PbfTagType, values: impl IntoIterator<Item = i32>) {
        self.add_packed_varint(tag, values.into_iter().map(|v| v as i64 as u64));
    }

    /// Add "repeated packed int32" field to data.
    pub fn add_packed_int32(&mut self, tag: PbfTagType, values: impl IntoIterator<Item = i32>) {
        self.add_packed_varint(tag, values.into_iter().map(|v| v as i64 as u64));
    }

    /// Add "repeated packed sint32" field to data.
    pub fn add_packed_sint32(&mut self, tag: PbfTagType, values: impl IntoIterator<Item = i32>) {
        self.add_packed_varint(tag, values.into_iter().map(|v| encode_zigzag64(v as i64)));
    }

    /// Add "repeated packed uint32" field to data.
    pub fn add_packed_uint32(&mut self, tag: PbfTagType, values: impl IntoIterator<Item = u32>) {
        self.add_packed_varint(tag, values.into_iter().map(|v| v as u64));
    }

    /// Add "repeated packed int64" field to data.
    pub fn add_packed_int64(&mut self, tag: PbfTagType, values: impl IntoIterator<Item = i64>) {
        self.add_packed_varint(tag, values.into_iter().map(|v| v as u64));
    }

    /// Add "repeated packed sint64" field to data.
    pub fn add_packed_sint64(&mut self, tag: PbfTagType, values: impl IntoIterator<Item = i64>) {
        self.add_packed_varint(tag, values.into_iter().map(encode_zigzag64));
    }

    /// Add "repeated packed uint64" field to data.
    pub fn add_packed_uint64(&mut self, tag: PbfTagType, values: impl IntoIterator<Item = u64>) {
        self.add_packed_varint(tag, values);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip() {
        for v in [
            0u64,
            1,
            127,
            128,
            300,
            16383,
            16384,
            u32::MAX as u64,
            u64::MAX,
        ] {
            let mut buf = Vec::new();
            let n = add_varint_to_buffer(&mut buf, v);
            assert_eq!(n as usize, buf.len());
            assert_eq!(length_of_varint(v), n);
            let mut d = &buf[..];
            assert_eq!(decode_varint(&mut d).unwrap(), v);
            assert!(d.is_empty());
            let mut d = &buf[..];
            skip_varint(&mut d).unwrap();
            assert!(d.is_empty());
        }
    }

    #[test]
    fn varint_errors() {
        let mut d: &[u8] = &[0x80, 0x80];
        assert_eq!(decode_varint(&mut d), Err(Exception::EndOfBuffer));
        assert_eq!(d.len(), 2);
        let mut d: &[u8] = &[0xff; 11];
        assert_eq!(decode_varint(&mut d), Err(Exception::VarintTooLong));
        let mut d: &[u8] = &[0xff; 11];
        assert_eq!(skip_varint(&mut d), Err(Exception::VarintTooLong));
        let mut d: &[u8] = &[0x80];
        assert_eq!(skip_varint(&mut d), Err(Exception::EndOfBuffer));
        // The tenth byte only contributes its lowest bit, as in the C++.
        let mut d: &[u8] = &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
        assert_eq!(decode_varint(&mut d), Ok(u64::MAX));
    }

    #[test]
    fn zigzag() {
        for v in [0i32, 1, -1, 2, -2, i32::MAX, i32::MIN] {
            assert_eq!(decode_zigzag32(encode_zigzag32(v)), v);
        }
        for v in [0i64, 1, -1, i64::MAX, i64::MIN] {
            assert_eq!(decode_zigzag64(encode_zigzag64(v)), v);
        }
        assert_eq!(encode_zigzag32(-1), 1);
        assert_eq!(encode_zigzag32(1), 2);
        assert_eq!(encode_zigzag64(-2), 3);
    }

    #[test]
    fn reader_and_writer() {
        let mut buf = Vec::new();
        {
            let mut w = PbfWriter::new(&mut buf);
            w.add_uint32(1, 150);
            w.add_string(2, b"testing");
            w.add_double(3, 1.5);
            w.add_float(4, -2.25);
            w.add_bool(5, true);
            w.add_sint64(6, -12345);
            w.add_enum(7, -1);
            w.add_packed_uint32(8, [1u32, 300, 5]);
            w.add_packed_uint32(9, Vec::<u32>::new());
            w.add_packed_sint64(10, [-1i64, 1, -300]);
        }
        assert_eq!(&buf[..3], &[0x08, 0x96, 0x01]);

        let mut r = PbfReader::new(&buf);
        assert!(r.next().unwrap());
        assert_eq!(r.tag(), 1);
        assert_eq!(r.get_uint32().unwrap(), 150);
        assert!(r.next().unwrap());
        assert_eq!(r.wire_type(), PbfWireType::LengthDelimited);
        assert_eq!(r.get_string().unwrap(), "testing");
        assert!(r.next().unwrap());
        assert_eq!(r.get_double().unwrap(), 1.5);
        assert!(r.next().unwrap());
        assert_eq!(r.get_float().unwrap(), -2.25);
        assert!(r.next().unwrap());
        assert!(r.get_bool().unwrap());
        assert!(r.next().unwrap());
        assert_eq!(r.get_sint64().unwrap(), -12345);
        assert!(r.next().unwrap());
        assert_eq!(r.get_enum().unwrap(), -1);
        assert!(r.next().unwrap());
        assert_eq!(r.tag(), 8);
        let p = r.get_packed_uint32().unwrap();
        assert_eq!(p.size(), 3);
        assert_eq!(p.map(|v| v.unwrap()).collect::<Vec<_>>(), vec![1, 300, 5]);
        // The empty packed field 9 was not written at all.
        assert!(r.next().unwrap());
        assert_eq!(r.tag(), 10);
        let p = r.get_packed_sint64().unwrap();
        assert_eq!(p.map(|v| v.unwrap()).collect::<Vec<_>>(), vec![-1, 1, -300]);
        assert!(!r.next().unwrap());
    }

    #[test]
    fn reader_errors() {
        // tag 0
        let mut r = PbfReader::new(&[0x00, 0x01]);
        assert_eq!(r.next(), Err(Exception::InvalidTag));
        // wire type 3
        let mut r = PbfReader::new(&[0x0b, 0x01]);
        assert_eq!(r.next(), Err(Exception::UnknownPbfWireType));
        // a string longer than the buffer
        let mut r = PbfReader::new(&[0x0a, 0x05, b'a']);
        assert!(r.next().unwrap());
        assert_eq!(r.get_view(), Err(Exception::EndOfBuffer));
        // skipping
        let mut r = PbfReader::new(&[0x0a, 0x01, b'a', 0x10, 0x01]);
        assert!(r.next_tag(2).unwrap());
        assert_eq!(r.get_uint32().unwrap(), 1);
    }
}
