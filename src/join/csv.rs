//! The CSV file of `-c`: tile-join's `readcsv`, returning errors instead of
//! exiting, with each joined value worked out once, before the tiles, rather
//! than for every feature it is joined to.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use tippecanoe::csv::{csv_dequote, csv_split, is_number};
use tippecanoe::errors::{EXIT_OPEN, EXIT_UTF8};
use tippecanoe::mvt::{MVT_DOUBLE, MVT_NULL, MVT_STRING, is_integer, is_unsigned_integer};
use tippecanoe::text::check_utf8_raw;

use super::tile::Val;
use crate::{Error, Result};

/// A column of a CSV row, as tile-join joins it to a feature.
pub struct Field {
    /// Its column, which names it in the header.
    pub column: usize,
    /// `MVT_STRING` if quoted or not a number, `MVT_DOUBLE` if a number,
    /// `MVT_NULL` if empty with `--empty-csv-columns-are-null`.
    pub attr_type: i32,
    /// The value it becomes in the tile (`stringified_to_mvt_value`).
    pub value: Val<'static>,
    /// The type tile-stats record for it.
    pub sv_type: i32,
    /// The text tile-stats record for it.
    pub text: String,
    /// Whether `stringified_to_mvt_value` warns that the number cannot be
    /// represented, which tile-join does each time it joins it.
    warns: bool,
}

impl Field {
    pub fn warn(&self) {
        if self.warns {
            eprintln!(
                "Warning: numeric value {} could not be represented",
                cstr(&self.text)
            );
        }
    }
}

/// A row of the CSV, ready to join, keyed in [`Csv::mapping`] by its first column.
pub struct Row {
    pub fields: Vec<Field>,
    /// The row has more columns than the header, which tile-join cannot join.
    pub too_long: bool,
}

#[derive(Default)]
pub struct Csv {
    pub header: Vec<String>,
    pub mapping: HashMap<String, Row>,
}

fn cstr(s: &str) -> &str {
    match s.find('\0') {
        Some(n) => &s[..n],
        None => s,
    }
}

/// libc `strtof`, with the bytes consumed.
fn strtof(s: &str) -> (f32, usize) {
    let c = std::ffi::CString::new(cstr(s)).unwrap_or_default();
    let mut endptr: *mut libc::c_char = std::ptr::null_mut();
    // SAFETY: `c` is NUL-terminated and outlives the call, which only reads
    // it and points `endptr` into it.
    let f = unsafe { libc::strtof(c.as_ptr(), &mut endptr) };
    (f, endptr as usize - c.as_ptr() as usize)
}

/// `stringified_to_mvt_value` for the types tile-join gives joined values,
/// and whether it warns.
fn joined_value(type_: i32, s: &str) -> (Val<'static>, bool) {
    let s = cstr(s);
    if type_ != MVT_DOUBLE {
        return (Val::Str(s.to_string().into()), false);
    }
    let mut v: i64 = 0;
    let mut uv: u64 = 0;
    if is_unsigned_integer(s, &mut uv) {
        if uv <= i64::MAX as u64 {
            return (Val::Int(uv as i64), false);
        }
        return (Val::Uint(uv), false);
    }
    if is_integer(s, &mut v) {
        return (Val::Sint(v), false);
    }
    tippecanoe::cstd::set_errno(0);
    let (f, endptr) = strtof(s);
    if endptr == 0 || (f == f32::INFINITY && tippecanoe::cstd::errno() == libc::ERANGE) {
        let (d, endptr) = tippecanoe::cstd::strtod(s);
        let warns =
            endptr == 0 || (d == f64::INFINITY && tippecanoe::cstd::errno() == libc::ERANGE);
        return (Val::Double(d), warns);
    }
    let d = tippecanoe::cstd::atof(s);
    if f as f64 == d {
        (Val::Float(f), false)
    } else {
        (Val::Double(d), false)
    }
}

/// The CSV file as tile-join reads it for `-c`: the header, and each row by
/// its first column, the first row with that key winning.
#[derive(Clone, Debug, Default)]
pub struct RawCsv {
    pub header: Vec<String>,
    pub rows: HashMap<String, Vec<String>>,
}

impl RawCsv {
    /// `readcsv`.
    pub fn read(path: &str) -> Result<RawCsv> {
        let f = std::fs::File::open(path).map_err(|e| {
            Error::new(
                EXIT_OPEN,
                format!("{}: {}", path, crate::options::os_message(&e)),
            )
        })?;
        let mut f = BufReader::new(f);
        let mut csv = RawCsv::default();

        let utf8_error = |err: &[u8]| {
            let mut msg = format!("{}: ", path).into_bytes();
            msg.extend_from_slice(err);
            Error::new(EXIT_UTF8, String::from_utf8_lossy(&msg).into_owned())
        };

        let mut first = true;
        loop {
            let mut s = Vec::new();
            // a read error ends the file, as EOF from getc() would
            let _ = f.read_until(b'\n', &mut s);
            if s.is_empty() {
                break;
            }
            let err = check_utf8_raw(&s);
            if !err.is_empty() {
                return Err(utf8_error(&err));
            }
            let s = String::from_utf8_lossy(&s);
            if first {
                first = false;
                csv.header = csv_split(&s).into_iter().map(csv_dequote).collect();
                continue;
            }
            let mut line = csv_split(&s);
            if !line.is_empty() {
                line[0] = csv_dequote(std::mem::take(&mut line[0]));
            }
            if !line.is_empty() && !csv.header.is_empty() && !csv.rows.contains_key(&line[0]) {
                csv.rows.insert(line[0].clone(), line);
            }
        }
        Ok(csv)
    }

    /// Each row's values as tile-join joins them, with or without
    /// `--empty-csv-columns-are-null`.
    pub fn prepare(&self, empty_is_null: bool) -> Csv {
        let header = &self.header;
        let mut mapping = HashMap::with_capacity(self.rows.len());
        for (key, line) in &self.rows {
            let mut fields = Vec::new();
            for (i, raw) in line
                .iter()
                .enumerate()
                .skip(1)
                .take(header.len().saturating_sub(1))
            {
                let mut joinval = raw.clone();
                let mut attr_type = MVT_STRING;
                if !joinval.is_empty() {
                    if joinval.as_bytes()[0] == b'"' {
                        joinval = csv_dequote(joinval);
                    } else if is_number(&joinval) {
                        attr_type = MVT_DOUBLE;
                    }
                } else if empty_is_null {
                    attr_type = MVT_NULL;
                }
                let sv_type = if attr_type == MVT_STRING {
                    MVT_STRING
                } else {
                    MVT_DOUBLE
                };
                let (value, warns) = joined_value(sv_type, &joinval);
                fields.push(Field {
                    column: i,
                    attr_type,
                    value,
                    sv_type,
                    text: joinval,
                    warns,
                });
            }
            let too_long = line.len() > header.len();
            mapping.insert(key.clone(), Row { fields, too_long });
        }
        Csv {
            header: header.clone(),
            mapping,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tippecanoe::mvt::stringified_to_mvt_value;

    #[test]
    fn joined_values_as_tippecanoe_makes_them() {
        for s in [
            "0",
            "1",
            "-1",
            "1.5",
            "1.50",
            "3.0000",
            "-0",
            "1e5",
            "1e40",
            "1e400",
            "-1e400",
            "18446744073709551615",
            "9223372036854775807",
            "9223372036854775808",
            "-9223372036854775808",
            "0.1",
            "123456789.123",
            " 7",
            "abc",
            "",
        ] {
            for t in [MVT_DOUBLE, MVT_STRING] {
                let ours = joined_value(t, s).0.to_mvt();
                let theirs = stringified_to_mvt_value(t, s);
                assert_eq!(ours.type_, theirs.type_, "{:?} as {}", s, t);
                assert!(
                    ours == theirs || (ours.to_double().is_nan() && theirs.to_double().is_nan()),
                    "{:?} as {}",
                    s,
                    t
                );
            }
        }
    }
}
