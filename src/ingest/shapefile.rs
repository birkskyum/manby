//! Features from shapefiles, read as Arrow tables.
//!
//! tippecanoe does not read these. A shapefile reads as the FlatGeobuf file
//! that GDAL writes from it, as Arrow tables do (`arrow.rs`), which it becomes
//! on the way. The shapes are GDAL's geometries, as well-known binary: the
//! rings of a polygon are grouped into polygons as GDAL's
//! `organizePolygons` groups them, clockwise rings being outer ones, and a
//! line of several parts is a multiline. The `.dbf` columns are typed as GDAL
//! types them and their text recoded as the `.cpg` file or the language
//! driver says. Deleted records are left out. The CRS is the `.prj` file's,
//! by its EPSG code or the ESRI name of a common one, unless `-s` gives one.

use std::fs::File;
use std::sync::Arc;

use arrow_array::builder::{
    BinaryBuilder, BooleanBuilder, Float64Builder, Int32Builder, Int64Builder, StringBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use memmap2::Mmap;
use tippecanoe::errors::{EXIT_OPEN, EXIT_READ};

use super::arrow::Batches;
use crate::Error;

const BATCH_ROWS: usize = 8192;

/// Whether a file name or `-L` format names a shapefile.
pub fn is_shapefile(format: &str, name: &str) -> bool {
    format == "shp" || format == "shapefile" || name.to_ascii_lowercase().ends_with(".shp")
}

/// The file name without `.shp`, which tippecanoe's layer naming does not
/// know to take off.
pub fn without_extension(name: &str) -> &str {
    match name.len().checked_sub(4) {
        Some(n) if n > 0 && name.is_char_boundary(n) && name[n..].eq_ignore_ascii_case(".shp") => {
            let stem = &name[..n];
            if stem.ends_with('/') { name } else { stem }
        }
        _ => name,
    }
}

/// The file that goes with the shapefile, `ext` in its case or the other.
fn sibling(path: &str, ext: &str) -> Option<String> {
    let stem = &path[..path.len() - 4];
    [ext.to_string(), ext.to_ascii_uppercase()]
        .into_iter()
        .map(|e| format!("{}{}", stem, e))
        .find(|p| std::path::Path::new(p).is_file())
}

fn map(path: &str, fname: &str) -> crate::Result<Mmap> {
    let file = File::open(path).map_err(|e| Error::new(EXIT_OPEN, format!("{}: {}", fname, e)))?;
    // SAFETY: the file is only read, and nothing should change it while it is read.
    unsafe { Mmap::map(&file) }.map_err(|e| Error::new(EXIT_OPEN, format!("{}: {}", fname, e)))
}

fn bad(fname: &str, what: &str) -> Error {
    Error::new(EXIT_READ, format!("{}: {}", fname, what))
}

/// Opens the shapefile `path` and gives the schema and batches of its
/// features. `crs` says to take the CRS from the `.prj` file.
pub fn open_shapefile(path: &str, fname: &str, crs: bool) -> crate::Result<(SchemaRef, Batches)> {
    let shp = map(path, fname)?;
    if shp.len() < 100 || i32::from_be_bytes(shp[0..4].try_into().unwrap()) != 9994 {
        return Err(bad(fname, "not a shapefile"));
    }
    let offsets = match sibling(path, ".shx") {
        Some(shx) => {
            let shx = map(&shx, fname)?;
            (100..shx.len().saturating_sub(7))
                .step_by(8)
                .map(|at| {
                    let word = |o: usize| i32::from_be_bytes(shx[o..o + 4].try_into().unwrap());
                    (word(at) as i64 * 2, word(at + 4) as i64 * 2)
                })
                .collect()
        }
        None => records(&shp),
    };

    let dbf = match sibling(path, ".dbf") {
        Some(p) => Some(Dbf::open(map(&p, fname)?, fname)?),
        None => None,
    };
    let encoding = match &dbf {
        Some(d) => Encoding::of(
            sibling(path, ".cpg")
                .and_then(|p| std::fs::read(p).ok())
                .map(|b| String::from_utf8_lossy(&b).trim().to_string())
                .as_deref(),
            d.ldid,
        ),
        None => Encoding::Utf8,
    };

    let mut fields = vec![Field::new("geometry", DataType::Binary, true)];
    if let Some(d) = &dbf {
        for f in &d.fields {
            fields.push(Field::new(
                encoding.decode(&f.name),
                f.kind.data_type(),
                true,
            ));
        }
    }
    let crs = if crs {
        match sibling(path, ".prj") {
            Some(p) => {
                let wkt =
                    String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default()).into_owned();
                match crs_of_prj(&wkt) {
                    Some(c) => format!(",\"crs\":\"{}\"", c),
                    None => {
                        return Err(bad(
                            fname,
                            &format!(
                                "the coordinates are in {}, which manby doesn't know; reproject them first, as with ogr2ogr -t_srs EPSG:4326, or give the CRS with -s",
                                prj_name(&wkt).unwrap_or("the CRS of its .prj file")
                            ),
                        ));
                    }
                }
            }
            None => ",\"crs\":null".to_string(),
        }
    } else {
        // -s says what it is
        ",\"crs\":null".to_string()
    };
    let geo = format!(
        "{{\"version\":\"1.1.0\",\"primary_column\":\"geometry\",\"columns\":{{\"geometry\":{{\"encoding\":\"WKB\"{}}}}}}}",
        crs
    );
    let schema = Arc::new(
        Schema::new(fields).with_metadata(arrow_schema::Metadata::default().with("geo", geo)),
    );
    Ok((
        schema.clone(),
        Box::new(Rows {
            shp,
            offsets,
            dbf,
            encoding,
            schema,
            next: 0,
            warned: false,
            fname: fname.to_string(),
        }),
    ))
}

/// Where the records are, walking the file, when there is no `.shx`.
fn records(shp: &[u8]) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut at = 100;
    while at + 8 <= shp.len() {
        let len = i32::from_be_bytes(shp[at + 4..at + 8].try_into().unwrap()) as i64 * 2;
        out.push((at as i64, len));
        at += 8 + len.max(0) as usize;
    }
    out
}

// ---------------------------------------------------------------------------
// .dbf

/// A column's type, as GDAL types the dBASE type.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Text,
    /// `N` or `F` without decimals and narrower than 10
    Integer,
    /// `N` or `F` without decimals and narrower than 19
    Integer64,
    Real,
    Logical,
    Date,
}

impl Kind {
    fn of(t: u8, width: usize, decimals: usize) -> Kind {
        match t {
            b'L' => Kind::Logical,
            b'D' => Kind::Date,
            b'N' | b'F' if decimals == 0 && width < 10 => Kind::Integer,
            b'N' | b'F' if decimals == 0 && width < 19 => Kind::Integer64,
            b'N' | b'F' => Kind::Real,
            _ => Kind::Text,
        }
    }

    fn data_type(self) -> DataType {
        match self {
            Kind::Text | Kind::Date => DataType::Utf8,
            Kind::Integer => DataType::Int32,
            Kind::Integer64 => DataType::Int64,
            Kind::Real => DataType::Float64,
            Kind::Logical => DataType::Boolean,
        }
    }
}

struct DbfField {
    name: Vec<u8>,
    kind: Kind,
    /// The dBASE type, which decides what is null.
    t: u8,
    offset: usize,
    width: usize,
}

struct Dbf {
    data: Mmap,
    records: usize,
    header: usize,
    record: usize,
    fields: Vec<DbfField>,
    /// The language driver.
    ldid: u8,
}

impl Dbf {
    fn open(data: Mmap, fname: &str) -> crate::Result<Dbf> {
        if data.len() < 32 {
            return Err(bad(fname, "its .dbf file is too short"));
        }
        let records = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let header = u16::from_le_bytes(data[8..10].try_into().unwrap()) as usize;
        let record = u16::from_le_bytes(data[10..12].try_into().unwrap()) as usize;
        let ldid = data[29];
        let mut fields = Vec::new();
        // the deletion flag comes first
        let mut offset = 1;
        let mut at = 32;
        while at + 32 <= header.min(data.len()) && data[at] != 0x0D {
            let d = &data[at..at + 32];
            let name: Vec<u8> = d[..11].iter().take_while(|&&b| b != 0).copied().collect();
            let mut name = name;
            while name.last() == Some(&b' ') {
                name.pop();
            }
            let width = d[16] as usize;
            let decimals = d[17] as usize;
            fields.push(DbfField {
                name,
                kind: Kind::of(d[11], width, decimals),
                t: d[11],
                offset,
                width,
            });
            offset += width;
            at += 32;
        }
        Ok(Dbf {
            data,
            records,
            header,
            record,
            fields,
            ldid,
        })
    }

    fn row(&self, i: usize) -> Option<&[u8]> {
        if i >= self.records {
            return None;
        }
        let at = self.header + i * self.record;
        self.data.get(at..at + self.record)
    }
}

/// A value as shapelib reads it: spaces taken off both ends.
fn trimmed(raw: &[u8]) -> &[u8] {
    let raw = match raw.iter().position(|&b| b == 0) {
        Some(n) => &raw[..n],
        None => raw,
    };
    let start = raw.iter().position(|&b| b != b' ').unwrap_or(raw.len());
    let end = raw
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(start, |e| e + 1);
    &raw[start..end.max(start)]
}

/// Whether shapelib takes the value as null (`DBFIsValueNULL`).
fn is_null(t: u8, v: &[u8]) -> bool {
    match t {
        b'N' | b'F' => v.first() == Some(&b'*') || v.iter().all(|&b| b == b' '),
        b'D' => {
            v.is_empty()
                || v.starts_with(b"00000000")
                || v == b" "
                || v == b"0"
                || v.iter().all(|&b| b == b'0')
        }
        b'L' => v.first() == Some(&b'?'),
        _ => false,
    }
}

/// `strtoll`'s value of the start of `v`, or 0.
fn integer_prefix(v: &[u8]) -> i64 {
    let s = std::str::from_utf8(v).unwrap_or("");
    let s = s.trim_start();
    let digits = s
        .char_indices()
        .take_while(|&(i, c)| c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')))
        .count();
    let t = &s[..digits];
    match t.parse::<i64>() {
        Ok(n) => n,
        // out of range
        Err(_) if t.starts_with('-') && t.len() > 1 => i64::MIN,
        Err(_) if t.chars().any(|c| c.is_ascii_digit()) => i64::MAX,
        Err(_) => 0,
    }
}

/// `CPLAtof`'s value of the start of `v`, or 0.
fn real_prefix(v: &[u8]) -> f64 {
    let s = std::str::from_utf8(v).unwrap_or("").trim_start();
    if let Ok(x) = s.parse::<f64>() {
        return x;
    }
    // the longest prefix that is a number
    let b = s.as_bytes();
    let mut end = 0;
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let mut digits = false;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits = true;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits = true;
        }
    }
    if digits {
        end = i;
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            let k = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > k {
                end = j;
            }
        }
    }
    s[..end].parse().unwrap_or(0.0)
}

/// A dBASE date as GDAL reads it, `YYYY-MM-DD`.
fn date(v: &[u8]) -> String {
    let s = String::from_utf8_lossy(v);
    let (y, m, d) = if s.len() >= 10 && v[2] == b'/' && v[5] == b'/' {
        (
            integer_prefix(&v[6..]),
            integer_prefix(&v[0..]),
            integer_prefix(&v[3..]),
        )
    } else {
        let n = integer_prefix(v) as i32 as i64;
        (n / 10000, (n / 100) % 100, n % 100)
    };
    format!("{:04}-{:02}-{:02}", y as i16, m as u8, d as u8)
}

/// How the `.dbf` text is encoded, by the `.cpg` file or the language driver.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Encoding {
    Utf8,
    Latin1,
    Windows1252,
    /// One that manby doesn't recode, read as UTF-8.
    Other,
}

impl Encoding {
    fn of(cpg: Option<&str>, ldid: u8) -> Encoding {
        if let Some(c) = cpg.filter(|c| !c.is_empty()) {
            let u = c.to_ascii_uppercase();
            return if u.starts_with("UTF-8") || u.starts_with("UTF8") {
                Encoding::Utf8
            } else if u == "1252" || u == "CP1252" || u == "WINDOWS-1252" {
                Encoding::Windows1252
            } else if u.starts_with("8859")
                || u.starts_with("ISO-8859-1")
                || u.starts_with("ISO8859")
            {
                let rest = u.trim_start_matches("ISO").trim_start_matches(['-', '_']);
                if rest == "8859-1" || rest == "88591" || rest == "8859_1" {
                    Encoding::Latin1
                } else {
                    Encoding::Other
                }
            } else {
                Encoding::Other
            };
        }
        match ldid {
            0 => Encoding::Utf8,
            87 => Encoding::Latin1,
            3 | 88 | 89 => Encoding::Windows1252,
            _ => Encoding::Other,
        }
    }

    fn decode(self, b: &[u8]) -> String {
        match self {
            Encoding::Utf8 | Encoding::Other => String::from_utf8_lossy(b).into_owned(),
            Encoding::Latin1 => b.iter().map(|&c| c as char).collect(),
            Encoding::Windows1252 => b.iter().map(|&c| windows_1252(c)).collect(),
        }
    }
}

fn windows_1252(c: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&c) {
        char::from_u32(HIGH[(c - 0x80) as usize] as u32).unwrap_or('\u{FFFD}')
    } else {
        c as char
    }
}

// ---------------------------------------------------------------------------
// .prj

/// The name a `.prj` file gives its CRS.
fn prj_name(wkt: &str) -> Option<&str> {
    let start = wkt.find('"')? + 1;
    let end = start + wkt[start..].find('"')?;
    Some(&wkt[start..end])
}

/// The CRS of a `.prj` file, as `EPSG:4326`: its own authority's code, or
/// the code of a common CRS by the name ESRI gives it.
fn crs_of_prj(wkt: &str) -> Option<String> {
    let wkt = wkt.trim();
    // the CRS's own AUTHORITY is the last thing in it, closed by its bracket
    let body = wkt.strip_suffix(']')?.trim_end();
    if let Some(at) = body.rfind("AUTHORITY[") {
        let tail = &body[at + "AUTHORITY[".len()..];
        if let Some(inner) = tail.strip_suffix(']')
            && !inner.contains(']')
        {
            let mut parts = inner.split(',').map(|p| p.trim().trim_matches('"'));
            if let (Some(authority), Some(code)) = (parts.next(), parts.next())
                && !code.is_empty()
                && code.bytes().all(|b| b.is_ascii_digit())
            {
                return Some(format!("{}:{}", authority.to_ascii_uppercase(), code));
            }
        }
    }
    let name = prj_name(wkt)?;
    known_name(name)
}

/// The code of a common CRS by its ESRI or EPSG name.
fn known_name(name: &str) -> Option<String> {
    let fixed = match name {
        "GCS_WGS_1984" | "WGS 84" | "WGS84" => "EPSG:4326",
        "GCS_North_American_1983" | "NAD83" => "EPSG:4269",
        "GCS_North_American_1927" | "NAD27" => "EPSG:4267",
        "GCS_ETRS_1989" | "ETRS89" => "EPSG:4258",
        "GCS_GDA_1994" | "GDA94" => "EPSG:4283",
        "GCS_GDA2020" | "GDA2020" => "EPSG:7844",
        "WGS_1984_Web_Mercator_Auxiliary_Sphere"
        | "WGS_84_Pseudo_Mercator"
        | "WGS 84 / Pseudo-Mercator"
        | "WGS_1984_Web_Mercator" => "EPSG:3857",
        "British_National_Grid" | "OSGB_1936_British_National_Grid" => "EPSG:27700",
        "RD_New" | "Amersfoort_RD_New" => "EPSG:28992",
        "RGF_1993_Lambert_93" => "EPSG:2154",
        "ETRS_1989_LAEA" | "ETRS_1989_LAEA_Europe" => "EPSG:3035",
        "NAD_1983_Contiguous_USA_Albers" => "EPSG:5070",
        "USA_Contiguous_Albers_Equal_Area_Conic" => "ESRI:102003",
        _ => "",
    };
    if !fixed.is_empty() {
        return Some(fixed.to_string());
    }
    // UTM zones
    let zone = |prefix: &str| -> Option<(u32, char)> {
        let rest = name.strip_prefix(prefix)?;
        let (digits, hemisphere) = rest.split_at(rest.len().checked_sub(1)?);
        let n: u32 = digits.parse().ok()?;
        let h = hemisphere.chars().next()?;
        ((1..=60).contains(&n) && (h == 'N' || h == 'S')).then_some((n, h))
    };
    let code = if let Some((n, h)) = zone("WGS_1984_UTM_Zone_") {
        if h == 'N' { 32600 + n } else { 32700 + n }
    } else if let Some((n, 'N')) = zone("NAD_1983_UTM_Zone_") {
        26900 + n
    } else if let Some((n, 'N')) = zone("NAD_1927_UTM_Zone_") {
        26700 + n
    } else if let Some((n, 'N')) = zone("ETRS_1989_UTM_Zone_") {
        25800 + n
    } else if let Some(n) = name
        .strip_prefix("GDA_1994_MGA_Zone_")
        .and_then(|z| z.parse::<u32>().ok())
    {
        28300 + n
    } else {
        7800 + name
            .strip_prefix("GDA2020_MGA_Zone_")
            .and_then(|z| z.parse::<u32>().ok())?
    };
    Some(format!("EPSG:{}", code))
}

// ---------------------------------------------------------------------------
// .shp

type Point = (f64, f64);

/// A shape as GDAL makes it, as well-known binary, or None for none.
fn shape_wkb(content: &[u8], warned: &mut bool, fname: &str) -> Option<Vec<u8>> {
    let t = i32::from_le_bytes(content.get(0..4)?.try_into().ok()?);
    let f64_at = |o: usize| -> Option<f64> {
        Some(f64::from_le_bytes(content.get(o..o + 8)?.try_into().ok()?))
    };
    let i32_at = |o: usize| -> Option<i32> {
        Some(i32::from_le_bytes(content.get(o..o + 4)?.try_into().ok()?))
    };
    let mut out = Vec::new();
    match t {
        0 => None,
        1 | 11 | 21 => {
            point(&mut out, (f64_at(4)?, f64_at(12)?));
            Some(out)
        }
        8 | 18 | 28 => {
            let n = i32_at(36)?;
            if n <= 0 {
                return None;
            }
            let points: Vec<Point> = (0..n as usize)
                .map(|i| Some((f64_at(40 + 16 * i)?, f64_at(48 + 16 * i)?)))
                .collect::<Option<_>>()?;
            header(&mut out, 4, points.len());
            for p in points {
                point(&mut out, p);
            }
            Some(out)
        }
        3 | 13 | 23 | 5 | 15 | 25 => {
            let parts = i32_at(36)?;
            let n = i32_at(40)?;
            if parts <= 0 || n < 0 {
                return None;
            }
            let (parts, n) = (parts as usize, n as usize);
            let starts: Vec<usize> = (0..parts)
                .map(|i| i32_at(44 + 4 * i).map(|s| s.max(0) as usize))
                .collect::<Option<_>>()?;
            let at = 44 + 4 * parts;
            let points: Vec<Point> = (0..n)
                .map(|i| Some((f64_at(at + 16 * i)?, f64_at(at + 16 * i + 8)?)))
                .collect::<Option<_>>()?;
            let ring = |i: usize| -> &[Point] {
                let s = starts[i].min(n);
                let e = if i + 1 < parts {
                    starts[i + 1].min(n)
                } else {
                    n
                };
                &points[s..e.max(s)]
            };
            let rings: Vec<&[Point]> = (0..parts).map(ring).collect();
            if t == 3 || t == 13 || t == 23 {
                if parts == 1 {
                    line(&mut out, &points);
                } else {
                    header(&mut out, 5, parts);
                    for r in rings {
                        line(&mut out, r);
                    }
                }
            } else {
                let polygons = if parts == 1 {
                    vec![vec![0]]
                } else {
                    organize(&rings, t == 5)
                };
                if polygons.len() == 1 {
                    polygon(&mut out, &polygons[0], &rings);
                } else {
                    header(&mut out, 6, polygons.len());
                    for p in &polygons {
                        polygon(&mut out, p, &rings);
                    }
                }
            }
            Some(out)
        }
        _ => {
            if !*warned {
                *warned = true;
                eprintln!(
                    "{}: shapes of type {} are left out, as multipatches",
                    fname, t
                );
            }
            None
        }
    }
}

fn header(out: &mut Vec<u8>, t: u32, n: usize) {
    out.push(1);
    out.extend_from_slice(&t.to_le_bytes());
    out.extend_from_slice(&(n as u32).to_le_bytes());
}

fn point(out: &mut Vec<u8>, p: Point) {
    out.push(1);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&p.0.to_le_bytes());
    out.extend_from_slice(&p.1.to_le_bytes());
}

fn coordinates(out: &mut Vec<u8>, points: &[Point]) {
    out.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for p in points {
        out.extend_from_slice(&p.0.to_le_bytes());
        out.extend_from_slice(&p.1.to_le_bytes());
    }
}

fn line(out: &mut Vec<u8>, points: &[Point]) {
    out.push(1);
    out.extend_from_slice(&2u32.to_le_bytes());
    coordinates(out, points);
}

fn polygon(out: &mut Vec<u8>, rings: &[usize], all: &[&[Point]]) {
    header(out, 3, rings.len());
    for &r in rings {
        coordinates(out, all[r]);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Envelope {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl Envelope {
    fn of(ring: &[Point]) -> Envelope {
        let mut e = Envelope {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        };
        for &(x, y) in ring {
            e.min_x = e.min_x.min(x);
            e.min_y = e.min_y.min(y);
            e.max_x = e.max_x.max(x);
            e.max_y = e.max_y.max(y);
        }
        e
    }

    fn contains(&self, o: &Envelope) -> bool {
        self.min_x <= o.min_x
            && self.min_y <= o.min_y
            && self.max_x >= o.max_x
            && self.max_y >= o.max_y
    }

    fn intersects(&self, o: &Envelope) -> bool {
        self.min_x <= o.max_x
            && self.max_x >= o.min_x
            && self.min_y <= o.max_y
            && self.max_y >= o.min_y
    }
}

/// GDAL's `OGRLineString::isClockwise`.
fn is_clockwise(p: &[Point]) -> bool {
    let n = p.len();
    if n < 2 {
        return true;
    }
    let mut fallback = false;
    // the lowest rightmost vertex
    let mut v = 0;
    for i in 1..n - 1 {
        if p[i].1 < p[v].1 || (p[i].1 == p[v].1 && p[i].0 > p[v].0) {
            v = i;
            fallback = false;
        } else if p[i].1 == p[v].1 && p[i].0 == p[v].0 {
            fallback = true;
        }
    }
    const EPSILON: f64 = 1.0e-5;
    let near = |a: Point, b: Point| (a.0 - b.0).abs() < EPSILON && (a.1 - b.1).abs() < EPSILON;
    let prev = if v == 0 { n - 2 } else { v - 1 };
    if near(p[prev], p[v]) {
        fallback = true;
    }
    let (dx0, dy0) = (p[prev].0 - p[v].0, p[prev].1 - p[v].1);
    let next = if v + 1 >= n - 1 { 0 } else { v + 1 };
    if near(p[next], p[v]) {
        fallback = true;
    }
    let (dx1, dy1) = (p[next].0 - p[v].0, p[next].1 - p[v].1);
    let cross = dx1 * dy0 - dx0 * dy1;
    if !fallback {
        if cross > 0.0 {
            return false;
        } else if cross < 0.0 {
            return true;
        }
    }
    green(p) < 0.0
}

/// The sum of Green's formula that GDAL's orientation test and area share.
fn green(p: &[Point]) -> f64 {
    let n = p.len();
    if n < 2 {
        return 0.0;
    }
    let mut sum = p[0].0 * (p[1].1 - p[n - 1].1);
    for i in 1..n - 1 {
        sum += p[i].0 * (p[i + 1].1 - p[i - 1].1);
    }
    sum + p[n - 1].0 * (p[0].1 - p[n - 2].1)
}

/// GDAL's `OGRLinearRing::isPointInRing`.
fn point_in_ring(ring: &[Point], (x, y): Point, test_envelope: bool) -> bool {
    if ring.len() < 4 {
        return false;
    }
    if test_envelope {
        let e = Envelope::of(ring);
        if !(x >= e.min_x && x <= e.max_x && y >= e.min_y && y <= e.max_y) {
            return false;
        }
    }
    let mut crossings = 0;
    let (mut px, mut py) = (ring[0].0 - x, ring[0].1 - y);
    for p in &ring[1..] {
        let (x1, y1) = (p.0 - x, p.1 - y);
        let (x2, y2) = (px, py);
        if (y1 > 0.0 && y2 <= 0.0) || (y2 > 0.0 && y1 <= 0.0) {
            let intersection = (x1 * y2 - x2 * y1) / (y2 - y1);
            if 0.0 < intersection {
                crossings += 1;
            }
        }
        px = x1;
        py = y1;
    }
    crossings % 2 == 1
}

/// GDAL's `OGRLinearRing::isPointOnRingBoundary`, without the envelope test.
fn point_on_ring(ring: &[Point], (x, y): Point) -> bool {
    if ring.len() < 4 {
        return false;
    }
    let (mut px, mut py) = (x - ring[0].0, y - ring[0].1);
    for i in 1..ring.len() {
        let (dx1, dy1) = (x - ring[i].0, y - ring[i].1);
        let (dx2, dy2) = (px, py);
        if dx1 * dy2 - dx2 * dy1 == 0.0 && !(dx1 == dx2 && dy1 == dy2) {
            let sx = ring[i].0 - ring[i - 1].0;
            let sy = ring[i].1 - ring[i - 1].1;
            let cross = dx2 * sx + dy2 * sy;
            if cross >= 0.0 && cross <= sx * sx + sy * sy {
                return true;
            }
        }
        px = dx1;
        py = dy1;
    }
    false
}

/// Whether ring `i` is inside ring `j`, as GDAL's fast test has it, by the
/// first point of `i` that is not on the boundary of `j`.
fn inside(i: &[Point], j: &[Point]) -> bool {
    let first = i[0];
    if !point_on_ring(j, first) {
        return point_in_ring(j, first, false);
    }
    let mut previous = first;
    let mut k = 1;
    while k < i.len() {
        let p = i[k];
        k += 1;
        if p == previous {
            continue;
        }
        if point_on_ring(j, p) {
        } else {
            return point_in_ring(j, p, false);
        }
        previous = p;
    }
    if i.len() > 2 {
        // every point on the boundary: the middle of a segment
        let mut previous = i[0];
        for &p in &i[1..] {
            if p == previous {
                continue;
            }
            let middle = ((p.0 + previous.0) / 2.0, (p.1 + previous.1) / 2.0);
            if point_on_ring(j, middle) {
            } else {
                return point_in_ring(j, middle, false);
            }
            previous = p;
        }
    }
    false
}

/// The rings grouped into polygons, each its outer ring and then its holes,
/// as GDAL's shapefile driver groups them with `organizePolygons`.
fn organize(rings: &[&[Point]], two_d: bool) -> Vec<Vec<usize>> {
    let n = rings.len();
    // a multipolygon written as one polygon with holes (QGIS <= 3.28.11)
    let mut slow = false;
    if two_d && !rings[1..].iter().any(|r| is_clockwise(r)) {
        let first = Envelope::of(rings[0]);
        for r in &rings[1..] {
            if !first.intersects(&Envelope::of(r)) {
                slow = true;
                break;
            }
            let (mut left, mut right, mut bottom, mut top) = (
                (f64::INFINITY, 0.0),
                (f64::NEG_INFINITY, 0.0),
                (0.0, f64::INFINITY),
                (0.0, f64::NEG_INFINITY),
            );
            for &p in &r[..r.len().saturating_sub(1)] {
                if p.0 < left.0 || (p.0 == left.0 && p.1 < left.1) {
                    left = p;
                }
                if p.0 > right.0 || (p.0 == right.0 && p.1 > right.1) {
                    right = p;
                }
                if p.1 < bottom.1 || (p.1 == bottom.1 && p.0 > bottom.0) {
                    bottom = p;
                }
                if p.1 > top.1 || (p.1 == top.1 && p.0 < top.0) {
                    top = p;
                }
            }
            if [left, right, bottom, top]
                .iter()
                .all(|&p| !point_in_ring(rings[0], p, true))
            {
                slow = true;
                break;
            }
        }
    }
    let only_ccw = !slow;

    struct Ring {
        index: usize,
        envelope: Envelope,
        area: f64,
        clockwise: bool,
        top: bool,
        enclosing: Option<usize>,
    }
    let mut mixed = false;
    let mut cw_count = 0;
    let mut cw_index = 0;
    let mut v: Vec<Ring> = Vec::with_capacity(n);
    for (i, r) in rings.iter().enumerate() {
        let mut ring = Ring {
            index: i,
            envelope: Envelope::of(r),
            area: 0.0,
            clockwise: false,
            top: false,
            enclosing: None,
        };
        if r.len() >= 4 {
            ring.area = 0.5 * green(r).abs();
            ring.clockwise = is_clockwise(r);
            if ring.clockwise {
                cw_index = i;
                cw_count += 1;
            }
        } else {
            mixed = true;
        }
        v.push(ring);
    }

    // one outer ring: the others are its holes
    if only_ccw && cw_count == 1 {
        let mut p = vec![cw_index];
        p.extend((0..n).filter(|&i| i != cw_index));
        return vec![p];
    }

    if !mixed {
        // by descending area (std::sort, stably here)
        v.sort_by(|a, b| {
            b.area
                .partial_cmp(&a.area)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    v[0].top = true;
    for i in 1..v.len() {
        if mixed {
            break;
        }
        if only_ccw && v[i].clockwise {
            v[i].top = true;
            continue;
        }
        let mut found = None;
        for j in (0..i).rev() {
            if only_ccw && !v[j].clockwise {
                continue;
            }
            if v[j].envelope.contains(&v[i].envelope) {
                let within = (only_ccw && j == 0) || inside(rings[v[i].index], rings[v[j].index]);
                if within {
                    found = Some(j);
                    break;
                }
            }
        }
        match found {
            Some(j) if v[j].top => {
                v[i].top = false;
                v[i].enclosing = Some(v[j].index);
            }
            _ => v[i].top = true,
        }
    }
    if mixed {
        for r in v.iter_mut() {
            r.top = true;
        }
    }
    v.sort_by_key(|r| r.index);

    let mut polygons: Vec<Vec<usize>> = Vec::new();
    let mut at: Vec<Option<usize>> = vec![None; n];
    for r in &v {
        if r.top {
            at[r.index] = Some(polygons.len());
            polygons.push(vec![r.index]);
        }
    }
    for r in &v {
        if let (false, Some(e)) = (r.top, r.enclosing)
            && let Some(p) = at[e]
        {
            polygons[p].push(r.index);
        }
    }
    polygons
}

// ---------------------------------------------------------------------------

/// The batches of a shapefile's features, in the order of its records.
struct Rows {
    shp: Mmap,
    offsets: Vec<(i64, i64)>,
    dbf: Option<Dbf>,
    encoding: Encoding,
    schema: SchemaRef,
    next: usize,
    warned: bool,
    fname: String,
}

enum Builder {
    Text(StringBuilder),
    Integer(Int32Builder),
    Integer64(Int64Builder),
    Real(Float64Builder),
    Logical(BooleanBuilder),
}

impl Rows {
    fn batch(&mut self) -> Option<Result<RecordBatch, ArrowError>> {
        if self.next >= self.offsets.len() {
            return None;
        }
        let mut geometry = BinaryBuilder::new();
        let mut builders: Vec<Builder> = self
            .dbf
            .iter()
            .flat_map(|d| d.fields.iter())
            .map(|f| match f.kind {
                Kind::Text | Kind::Date => Builder::Text(StringBuilder::new()),
                Kind::Integer => Builder::Integer(Int32Builder::new()),
                Kind::Integer64 => Builder::Integer64(Int64Builder::new()),
                Kind::Real => Builder::Real(Float64Builder::new()),
                Kind::Logical => Builder::Logical(BooleanBuilder::new()),
            })
            .collect();
        let mut rows = 0;
        while self.next < self.offsets.len() && rows < BATCH_ROWS {
            let i = self.next;
            self.next += 1;
            let record = self.dbf.as_ref().and_then(|d| d.row(i));
            if record.is_some_and(|r| r[0] == b'*') {
                continue;
            }
            let (at, len) = self.offsets[i];
            let content = (at >= 0 && len >= 0)
                .then(|| {
                    self.shp
                        .get(at as usize + 8..at as usize + 8 + len as usize)
                })
                .flatten();
            match content.and_then(|c| shape_wkb(c, &mut self.warned, &self.fname)) {
                Some(w) => geometry.append_value(&w),
                None => geometry.append_null(),
            }
            let fields = self
                .dbf
                .as_ref()
                .map(|d| d.fields.as_slice())
                .unwrap_or(&[]);
            for (f, b) in fields.iter().zip(builders.iter_mut()) {
                let raw = record.and_then(|r| r.get(f.offset..f.offset + f.width));
                let v = raw.map(trimmed);
                let null = v.is_none_or(|v| is_null(f.t, v));
                match b {
                    Builder::Text(b) => match v {
                        Some(v) if !v.is_empty() && f.kind == Kind::Text => {
                            b.append_value(self.encoding.decode(v))
                        }
                        Some(v) if f.kind == Kind::Date && !null => b.append_value(date(v)),
                        _ => b.append_null(),
                    },
                    Builder::Integer(b) => match v {
                        Some(v) if !null => b.append_value(
                            integer_prefix(v).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                        ),
                        _ => b.append_null(),
                    },
                    Builder::Integer64(b) => match v {
                        Some(v) if !null => b.append_value(integer_prefix(v)),
                        _ => b.append_null(),
                    },
                    Builder::Real(b) => match v {
                        Some(v) if !null => b.append_value(real_prefix(v)),
                        _ => b.append_null(),
                    },
                    Builder::Logical(b) => match v {
                        Some(v) if !null => {
                            b.append_value(matches!(v.first(), Some(b'T' | b't' | b'Y' | b'y')))
                        }
                        _ => b.append_null(),
                    },
                }
            }
            rows += 1;
        }
        if rows == 0 {
            return self.batch();
        }
        let mut arrays: Vec<ArrayRef> = vec![Arc::new(geometry.finish())];
        for b in builders {
            arrays.push(match b {
                Builder::Text(mut b) => Arc::new(b.finish()),
                Builder::Integer(mut b) => Arc::new(b.finish()),
                Builder::Integer64(mut b) => Arc::new(b.finish()),
                Builder::Real(mut b) => Arc::new(b.finish()),
                Builder::Logical(mut b) => Arc::new(b.finish()),
            });
        }
        Some(RecordBatch::try_new(self.schema.clone(), arrays))
    }
}

impl Iterator for Rows {
    type Item = Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.batch()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64, clockwise: bool) -> Vec<Point> {
        let mut r = vec![(x, y), (x, y + s), (x + s, y + s), (x + s, y), (x, y)];
        if !clockwise {
            r.reverse();
        }
        r
    }

    #[test]
    fn orientation() {
        assert!(is_clockwise(&square(0.0, 0.0, 1.0, true)));
        assert!(!is_clockwise(&square(0.0, 0.0, 1.0, false)));
    }

    #[test]
    fn rings_become_polygons_with_holes() {
        let a = square(0.0, 0.0, 10.0, true);
        let hole_a = square(1.0, 1.0, 2.0, false);
        let b = square(20.0, 0.0, 10.0, true);
        let hole_b = square(21.0, 1.0, 2.0, false);
        // holes before their outer rings, as a file may have them
        let rings: Vec<&[Point]> = vec![&hole_b, &a, &b, &hole_a];
        assert_eq!(organize(&rings, true), vec![vec![1, 3], vec![2, 0]]);
        // one outer ring takes every other ring
        let rings: Vec<&[Point]> = vec![&hole_a, &a];
        assert_eq!(organize(&rings, true), vec![vec![1, 0]]);
        // an island in a lake is a polygon of its own
        let lake = square(2.0, 2.0, 6.0, false);
        let island = square(3.0, 3.0, 2.0, true);
        let rings: Vec<&[Point]> = vec![&a, &lake, &island, &b];
        assert_eq!(organize(&rings, true), vec![vec![0, 1], vec![2], vec![3]]);
    }

    #[test]
    fn wrong_winding_is_corrected() {
        // two outer rings written counterclockwise after the first
        let a = square(0.0, 0.0, 10.0, true);
        let b = square(20.0, 0.0, 10.0, false);
        let rings: Vec<&[Point]> = vec![&a, &b];
        assert_eq!(organize(&rings, true), vec![vec![0], vec![1]]);
    }

    #[test]
    fn values_as_gdal_reads_them() {
        assert_eq!(trimmed(b"  ab c  "), b"ab c");
        assert_eq!(integer_prefix(b" 12.7"), 12);
        assert_eq!(integer_prefix(b"-3"), -3);
        assert_eq!(real_prefix(b"1.5e3x"), 1500.0);
        assert!(is_null(b'N', b""));
        assert!(is_null(b'N', b"****"));
        assert!(is_null(b'D', b"00000000"));
        assert_eq!(date(b"20060102"), "2006-01-02");
        assert_eq!(date(b"01/02/2006"), "2006-01-02");
        assert_eq!(Encoding::of(Some("UTF-8"), 0), Encoding::Utf8);
        assert_eq!(Encoding::of(None, 87), Encoding::Latin1);
        assert_eq!(Encoding::Windows1252.decode(&[0x80, b'a', 0xE9]), "€aé");
    }

    #[test]
    fn crs_from_prj() {
        assert_eq!(
            crs_of_prj(r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#).as_deref(),
            Some("EPSG:4326")
        );
        assert_eq!(
            crs_of_prj(r#"PROJCS["NAD_1983_UTM_Zone_10N",GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],UNIT["Meter",1.0]]"#).as_deref(),
            Some("EPSG:26910")
        );
        assert_eq!(
            crs_of_prj(r#"PROJCS["NAD83 / UTM zone 10N",GEOGCS["NAD83",AUTHORITY["EPSG","4269"]],UNIT["metre",1],AUTHORITY["EPSG","26910"]]"#).as_deref(),
            Some("EPSG:26910")
        );
        assert_eq!(crs_of_prj(r#"PROJCS["Somewhere",GEOGCS["X"]]"#), None);
    }
}
