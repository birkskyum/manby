//! The writing half of pbf 3.3.0 (`index.js`), as far as encode.js uses it.
//!
//! Values are JavaScript numbers, so the varint writers take `f64` and
//! convert them the way pbf does with `+val || 0`, `|`, `&` and `>>>`.

use crate::js::{throw, to_int32, to_uint32};

pub const BYTES: u32 = 2; // length-delimited: string, bytes, embedded messages, packed repeated fields
pub const FIXED64: u32 = 1; // 64-bit: double, fixed64, sfixed64
pub const VARINT: u32 = 0; // varint: int32, int64, uint32, uint64, sint32, sint64, bool, enum

pub struct Pbf {
    /// The Uint8Array. Its length is `this.length`.
    buf: Vec<u8>,
    pos: usize,
}

impl Pbf {
    pub fn new() -> Pbf {
        Pbf {
            buf: Vec::new(),
            pos: 0,
        }
    }

    // === WRITING =================================================================

    pub fn write_tag(&mut self, tag: u32, ty: u32) {
        self.write_varint(((tag << 3) | ty) as f64);
    }

    pub fn realloc(&mut self, min: usize) {
        let mut length = if self.buf.is_empty() {
            16
        } else {
            self.buf.len()
        };

        while length < self.pos + min {
            length *= 2;
        }

        if length != self.buf.len() {
            self.buf.resize(length, 0);
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.buf.truncate(self.pos);
        self.buf
    }

    pub fn write_varint(&mut self, val: f64) {
        // val = +val || 0;
        let val = if val.is_nan() || val == 0.0 { 0.0 } else { val };

        if val > 0xfffffff as f64 || val < 0.0 {
            write_big_varint(val, self);
            return;
        }

        self.realloc(4);

        // The first byte tests the original number, which need not be an integer.
        self.buf[self.pos] =
            (to_int32(val) & 0x7f) as u8 | if val > 0x7f as f64 { 0x80 } else { 0 };
        self.pos += 1;
        if val <= 0x7f as f64 {
            return;
        }
        let mut val = to_uint32(val) >> 7;
        self.buf[self.pos] = (val & 0x7f) as u8 | if val > 0x7f { 0x80 } else { 0 };
        self.pos += 1;
        if val <= 0x7f {
            return;
        }
        val >>= 7;
        self.buf[self.pos] = (val & 0x7f) as u8 | if val > 0x7f { 0x80 } else { 0 };
        self.pos += 1;
        if val <= 0x7f {
            return;
        }
        self.buf[self.pos] = ((val >> 7) & 0x7f) as u8;
        self.pos += 1;
    }

    pub fn write_svarint(&mut self, val: f64) {
        self.write_varint(if val < 0.0 {
            -val * 2.0 - 1.0
        } else {
            val * 2.0
        });
    }

    pub fn write_string(&mut self, str: &[u16]) {
        self.realloc(str.len() * 4);

        self.pos += 1; // reserve 1 byte for short string length

        let start_pos = self.pos;
        // write the string directly to the buffer and see how much was written
        self.pos = write_utf8(&mut self.buf, str, self.pos);
        let len = self.pos - start_pos;

        if len >= 0x80 {
            make_room_for_extra_length(start_pos, len, self);
        }

        // finally, write the message length in the reserved place and restore the position
        self.pos = start_pos - 1;
        self.write_varint(len as f64);
        self.pos += len;
    }

    pub fn write_double(&mut self, val: f64) {
        self.realloc(8);
        // ieee754.write(this.buf, val, this.pos, true, 52, 8), which is exact
        // for every double when the mantissa has 52 bits.
        self.buf[self.pos..self.pos + 8].copy_from_slice(&val.to_le_bytes());
        self.pos += 8;
    }

    pub fn write_raw_message(&mut self, f: impl FnOnce(&mut Pbf)) {
        self.pos += 1; // reserve 1 byte for short message length

        // write the message directly to the buffer and see how much was written
        let start_pos = self.pos;
        f(self);
        let len = self.pos - start_pos;

        if len >= 0x80 {
            make_room_for_extra_length(start_pos, len, self);
        }

        // finally, write the message length in the reserved place and restore the position
        self.pos = start_pos - 1;
        self.write_varint(len as f64);
        self.pos += len;
    }

    pub fn write_message(&mut self, tag: u32, f: impl FnOnce(&mut Pbf)) {
        self.write_tag(tag, BYTES);
        self.write_raw_message(f);
    }

    pub fn write_packed_varint(&mut self, tag: u32, arr: &[f64]) {
        if !arr.is_empty() {
            self.write_message(tag, |pbf| write_packed_varint(arr, pbf));
        }
    }

    pub fn write_packed_svarint(&mut self, tag: u32, arr: &[f64]) {
        if !arr.is_empty() {
            self.write_message(tag, |pbf| write_packed_svarint(arr, pbf));
        }
    }

    pub fn write_varint_field(&mut self, tag: u32, val: f64) {
        self.write_tag(tag, VARINT);
        self.write_varint(val);
    }

    pub fn write_svarint_field(&mut self, tag: u32, val: f64) {
        self.write_tag(tag, VARINT);
        self.write_svarint(val);
    }

    pub fn write_string_field(&mut self, tag: u32, str: &[u16]) {
        self.write_tag(tag, BYTES);
        self.write_string(str);
    }

    pub fn write_double_field(&mut self, tag: u32, val: f64) {
        self.write_tag(tag, FIXED64);
        self.write_double(val);
    }

    pub fn write_boolean_field(&mut self, tag: u32, val: bool) {
        self.write_varint_field(tag, if val { 1.0 } else { 0.0 });
    }
}

// The range check is written the way pbf.js writes it.
#[allow(clippy::manual_range_contains)]
fn write_big_varint(val: f64, pbf: &mut Pbf) {
    let mut low: i32;
    let mut high: i32;

    if val >= 0.0 {
        low = to_int32(val % 0x100000000u64 as f64);
        high = to_int32(val / 0x100000000u64 as f64);
    } else {
        low = !to_int32(-val % 0x100000000u64 as f64);
        high = !to_int32(-val / 0x100000000u64 as f64);

        if low ^ -1 != 0 {
            low = low.wrapping_add(1);
        } else {
            low = 0;
            high = high.wrapping_add(1);
        }
    }

    if val >= 18446744073709551616.0 || val < -18446744073709551616.0 {
        throw("Error: Given varint doesn't fit into 10 bytes");
    }

    pbf.realloc(10);

    write_big_varint_low(low as u32, pbf);
    write_big_varint_high(high as u32, pbf);
}

fn write_big_varint_low(mut low: u32, pbf: &mut Pbf) {
    for _ in 0..4 {
        pbf.buf[pbf.pos] = (low & 0x7f) as u8 | 0x80;
        pbf.pos += 1;
        low >>= 7;
    }
    pbf.buf[pbf.pos] = (low & 0x7f) as u8;
}

fn write_big_varint_high(mut high: u32, pbf: &mut Pbf) {
    let lsb = ((high & 0x07) << 4) as u8;

    high >>= 3;
    pbf.buf[pbf.pos] |= lsb | if high != 0 { 0x80 } else { 0 };
    pbf.pos += 1;
    if high == 0 {
        return;
    }
    for _ in 0..4 {
        let byte = (high & 0x7f) as u8;
        high >>= 7;
        pbf.buf[pbf.pos] = byte | if high != 0 { 0x80 } else { 0 };
        pbf.pos += 1;
        if high == 0 {
            return;
        }
    }
    pbf.buf[pbf.pos] = (high & 0x7f) as u8;
    pbf.pos += 1;
}

fn make_room_for_extra_length(start_pos: usize, len: usize, pbf: &mut Pbf) {
    let extra_len = if len <= 0x3fff {
        1
    } else if len <= 0x1fffff {
        2
    } else if len <= 0xfffffff {
        3
    } else {
        ((len as f64).ln() / (std::f64::consts::LN_2 * 7.0)).floor() as usize
    };

    // if 1 byte isn't enough for encoding message length, shift the data to the right
    pbf.realloc(extra_len);
    pbf.buf
        .copy_within(start_pos..pbf.pos, start_pos + extra_len);
}

fn write_packed_varint(arr: &[f64], pbf: &mut Pbf) {
    for &v in arr {
        pbf.write_varint(v);
    }
}

fn write_packed_svarint(arr: &[f64], pbf: &mut Pbf) {
    for &v in arr {
        pbf.write_svarint(v);
    }
}

/// Encodes UTF-16 as UTF-8, writing U+FFFD for a lone surrogate. A lead
/// surrogate that follows another lead surrogate and ends the string is dropped.
fn write_utf8(buf: &mut [u8], str: &[u16], mut pos: usize) -> usize {
    let mut lead: Option<u32> = None;
    let mut put = |buf: &mut [u8], b: u32| {
        buf[pos] = b as u8;
        pos += 1;
    };
    for i in 0..str.len() {
        let mut c = str[i] as u32; // code point

        if c > 0xD7FF && c < 0xE000 {
            if let Some(l) = lead {
                if c < 0xDC00 {
                    put(buf, 0xEF);
                    put(buf, 0xBF);
                    put(buf, 0xBD);
                    lead = Some(c);
                    continue;
                } else {
                    c = ((l - 0xD800) << 10) | (c - 0xDC00) | 0x10000;
                    lead = None;
                }
            } else {
                if c > 0xDBFF || (i + 1 == str.len()) {
                    put(buf, 0xEF);
                    put(buf, 0xBF);
                    put(buf, 0xBD);
                } else {
                    lead = Some(c);
                }
                continue;
            }
        } else if lead.is_some() {
            put(buf, 0xEF);
            put(buf, 0xBF);
            put(buf, 0xBD);
            lead = None;
        }

        if c < 0x80 {
            put(buf, c);
        } else {
            if c < 0x800 {
                put(buf, c >> 0x6 | 0xC0);
            } else {
                if c < 0x10000 {
                    put(buf, c >> 0xC | 0xE0);
                } else {
                    put(buf, c >> 0x12 | 0xF0);
                    put(buf, c >> 0xC & 0x3F | 0x80);
                }
                put(buf, c >> 0x6 & 0x3F | 0x80);
            }
            put(buf, c & 0x3F | 0x80);
        }
    }
    pos
}
