//! The FlatGeobuf header and feature tables, ported from flatgeobuf/header_generated.h and
//! flatgeobuf/feature_generated.h (or generated from the same schema).
//!
//! This follows the shape of what `flatc --rust` generates for the schema, with one
//! change: strings are read as bytes, since the C++ verifier does not require them
//! to be UTF-8 and `std::string` keeps whatever bytes they hold.

#![allow(non_upper_case_globals, clippy::all)]

use flatbuffers::{
    Follow, ForwardsUOffset, InvalidFlatbuffer, SimpleToVerifyInSlice, Table, VOffsetT, Vector,
    Verifiable, Verifier,
};

/// A flatbuffers string, verified as the C++ verifier does it: a vector of bytes
/// followed by a NUL, with no check that the bytes are UTF-8.
pub struct CppStr<'a>(std::marker::PhantomData<&'a [u8]>);

impl<'a> Follow<'a> for CppStr<'a> {
    type Inner = &'a [u8];
    #[inline]
    unsafe fn follow(buf: &'a [u8], loc: usize) -> Self::Inner {
        // SAFETY: the caller has verified that a string is at loc.
        unsafe { <&'a [u8]>::follow(buf, loc) }
    }
}

impl Verifiable for CppStr<'_> {
    #[inline]
    fn run_verifier(v: &mut Verifier, pos: usize) -> Result<(), InvalidFlatbuffer> {
        <Vector<'_, u8>>::run_verifier(v, pos)?;
        let len = v.get_uoffset(pos)? as usize;
        let end = pos.saturating_add(4).saturating_add(len);
        if v.get_u8(end)? != 0 {
            return Err(InvalidFlatbuffer::MissingNullTerminator {
                range: pos + 4..end,
                error_trace: Default::default(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
#[repr(transparent)]
pub struct GeometryType(pub u8);

#[allow(non_upper_case_globals)]
impl GeometryType {
    pub const Unknown: Self = Self(0);
    pub const Point: Self = Self(1);
    pub const LineString: Self = Self(2);
    pub const Polygon: Self = Self(3);
    pub const MultiPoint: Self = Self(4);
    pub const MultiLineString: Self = Self(5);
    pub const MultiPolygon: Self = Self(6);
    pub const GeometryCollection: Self = Self(7);
    pub const CircularString: Self = Self(8);
    pub const CompoundCurve: Self = Self(9);
    pub const CurvePolygon: Self = Self(10);
    pub const MultiCurve: Self = Self(11);
    pub const MultiSurface: Self = Self(12);
    pub const Curve: Self = Self(13);
    pub const Surface: Self = Self(14);
    pub const PolyhedralSurface: Self = Self(15);
    pub const TIN: Self = Self(16);
    pub const Triangle: Self = Self(17);

    pub const ENUM_MIN: u8 = 0;
    pub const ENUM_MAX: u8 = 17;
}

impl<'a> Follow<'a> for GeometryType {
    type Inner = Self;
    #[inline]
    unsafe fn follow(buf: &'a [u8], loc: usize) -> Self::Inner {
        // SAFETY: the caller has verified that a byte is at loc.
        let b = unsafe { flatbuffers::read_scalar_at::<u8>(buf, loc) };
        Self(b)
    }
}

impl Verifiable for GeometryType {
    #[inline]
    fn run_verifier(v: &mut Verifier, pos: usize) -> Result<(), InvalidFlatbuffer> {
        u8::run_verifier(v, pos)
    }
}

impl SimpleToVerifyInSlice for GeometryType {}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
#[repr(transparent)]
pub struct ColumnType(pub u8);

#[allow(non_upper_case_globals)]
impl ColumnType {
    pub const Byte: Self = Self(0);
    pub const UByte: Self = Self(1);
    pub const Bool: Self = Self(2);
    pub const Short: Self = Self(3);
    pub const UShort: Self = Self(4);
    pub const Int: Self = Self(5);
    pub const UInt: Self = Self(6);
    pub const Long: Self = Self(7);
    pub const ULong: Self = Self(8);
    pub const Float: Self = Self(9);
    pub const Double: Self = Self(10);
    pub const String: Self = Self(11);
    pub const Json: Self = Self(12);
    pub const DateTime: Self = Self(13);
    pub const Binary: Self = Self(14);

    pub const ENUM_MIN: u8 = 0;
    pub const ENUM_MAX: u8 = 14;
}

impl<'a> Follow<'a> for ColumnType {
    type Inner = Self;
    #[inline]
    unsafe fn follow(buf: &'a [u8], loc: usize) -> Self::Inner {
        // SAFETY: the caller has verified that a byte is at loc.
        let b = unsafe { flatbuffers::read_scalar_at::<u8>(buf, loc) };
        Self(b)
    }
}

impl Verifiable for ColumnType {
    #[inline]
    fn run_verifier(v: &mut Verifier, pos: usize) -> Result<(), InvalidFlatbuffer> {
        u8::run_verifier(v, pos)
    }
}

impl SimpleToVerifyInSlice for ColumnType {}

#[derive(Copy, Clone, PartialEq)]
pub struct Column<'a> {
    pub _tab: Table<'a>,
}

impl<'a> Follow<'a> for Column<'a> {
    type Inner = Column<'a>;
    #[inline]
    unsafe fn follow(buf: &'a [u8], loc: usize) -> Self::Inner {
        Self {
            // SAFETY: the caller has verified that a table is at loc.
            _tab: unsafe { Table::new(buf, loc) },
        }
    }
}

impl<'a> Column<'a> {
    pub const VT_NAME: VOffsetT = 4;
    pub const VT_TYPE: VOffsetT = 6;
    pub const VT_TITLE: VOffsetT = 8;
    pub const VT_DESCRIPTION: VOffsetT = 10;
    pub const VT_WIDTH: VOffsetT = 12;
    pub const VT_PRECISION: VOffsetT = 14;
    pub const VT_SCALE: VOffsetT = 16;
    pub const VT_NULLABLE: VOffsetT = 18;
    pub const VT_UNIQUE: VOffsetT = 20;
    pub const VT_PRIMARY_KEY: VOffsetT = 22;
    pub const VT_METADATA: VOffsetT = 24;

    #[inline]
    pub fn name(&self) -> &'a [u8] {
        // SAFETY: verified, and the field is required
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Column::VT_NAME, None)
                .unwrap()
        }
    }
    #[inline]
    pub fn type_(&self) -> ColumnType {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ColumnType>(Column::VT_TYPE, Some(ColumnType::Byte))
                .unwrap()
        }
    }
    #[inline]
    pub fn title(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Column::VT_TITLE, None)
        }
    }
    #[inline]
    pub fn description(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Column::VT_DESCRIPTION, None)
        }
    }
    #[inline]
    pub fn width(&self) -> i32 {
        // SAFETY: verified
        unsafe { self._tab.get::<i32>(Column::VT_WIDTH, Some(-1)).unwrap() }
    }
    #[inline]
    pub fn precision(&self) -> i32 {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<i32>(Column::VT_PRECISION, Some(-1))
                .unwrap()
        }
    }
    #[inline]
    pub fn scale(&self) -> i32 {
        // SAFETY: verified
        unsafe { self._tab.get::<i32>(Column::VT_SCALE, Some(-1)).unwrap() }
    }
    #[inline]
    pub fn nullable(&self) -> bool {
        // SAFETY: verified
        unsafe { self._tab.get::<u8>(Column::VT_NULLABLE, Some(1)).unwrap() != 0 }
    }
    #[inline]
    pub fn unique(&self) -> bool {
        // SAFETY: verified
        unsafe { self._tab.get::<u8>(Column::VT_UNIQUE, Some(0)).unwrap() != 0 }
    }
    #[inline]
    pub fn primary_key(&self) -> bool {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<u8>(Column::VT_PRIMARY_KEY, Some(0))
                .unwrap()
                != 0
        }
    }
    #[inline]
    pub fn metadata(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Column::VT_METADATA, None)
        }
    }
}

impl Verifiable for Column<'_> {
    #[inline]
    fn run_verifier(v: &mut Verifier, pos: usize) -> Result<(), InvalidFlatbuffer> {
        v.visit_table(pos)?
            .visit_field::<ForwardsUOffset<CppStr>>("name", Self::VT_NAME, true)?
            .visit_field::<ColumnType>("type_", Self::VT_TYPE, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("title", Self::VT_TITLE, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("description", Self::VT_DESCRIPTION, false)?
            .visit_field::<i32>("width", Self::VT_WIDTH, false)?
            .visit_field::<i32>("precision", Self::VT_PRECISION, false)?
            .visit_field::<i32>("scale", Self::VT_SCALE, false)?
            .visit_field::<u8>("nullable", Self::VT_NULLABLE, false)?
            .visit_field::<u8>("unique", Self::VT_UNIQUE, false)?
            .visit_field::<u8>("primary_key", Self::VT_PRIMARY_KEY, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("metadata", Self::VT_METADATA, false)?
            .finish();
        Ok(())
    }
}

#[derive(Copy, Clone, PartialEq)]
pub struct Crs<'a> {
    pub _tab: Table<'a>,
}

impl<'a> Follow<'a> for Crs<'a> {
    type Inner = Crs<'a>;
    #[inline]
    unsafe fn follow(buf: &'a [u8], loc: usize) -> Self::Inner {
        Self {
            // SAFETY: the caller has verified that a table is at loc.
            _tab: unsafe { Table::new(buf, loc) },
        }
    }
}

impl<'a> Crs<'a> {
    pub const VT_ORG: VOffsetT = 4;
    pub const VT_CODE: VOffsetT = 6;
    pub const VT_NAME: VOffsetT = 8;
    pub const VT_DESCRIPTION: VOffsetT = 10;
    pub const VT_WKT: VOffsetT = 12;
    pub const VT_CODE_STRING: VOffsetT = 14;

    #[inline]
    pub fn org(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe { self._tab.get::<ForwardsUOffset<CppStr>>(Crs::VT_ORG, None) }
    }
    #[inline]
    pub fn code(&self) -> i32 {
        // SAFETY: verified
        unsafe { self._tab.get::<i32>(Crs::VT_CODE, Some(0)).unwrap() }
    }
    #[inline]
    pub fn name(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe { self._tab.get::<ForwardsUOffset<CppStr>>(Crs::VT_NAME, None) }
    }
    #[inline]
    pub fn description(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Crs::VT_DESCRIPTION, None)
        }
    }
    #[inline]
    pub fn wkt(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe { self._tab.get::<ForwardsUOffset<CppStr>>(Crs::VT_WKT, None) }
    }
    #[inline]
    pub fn code_string(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Crs::VT_CODE_STRING, None)
        }
    }
}

impl Verifiable for Crs<'_> {
    #[inline]
    fn run_verifier(v: &mut Verifier, pos: usize) -> Result<(), InvalidFlatbuffer> {
        v.visit_table(pos)?
            .visit_field::<ForwardsUOffset<CppStr>>("org", Self::VT_ORG, false)?
            .visit_field::<i32>("code", Self::VT_CODE, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("name", Self::VT_NAME, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("description", Self::VT_DESCRIPTION, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("wkt", Self::VT_WKT, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("code_string", Self::VT_CODE_STRING, false)?
            .finish();
        Ok(())
    }
}

#[derive(Copy, Clone, PartialEq)]
pub struct Header<'a> {
    pub _tab: Table<'a>,
}

impl<'a> Follow<'a> for Header<'a> {
    type Inner = Header<'a>;
    #[inline]
    unsafe fn follow(buf: &'a [u8], loc: usize) -> Self::Inner {
        Self {
            // SAFETY: the caller has verified that a table is at loc.
            _tab: unsafe { Table::new(buf, loc) },
        }
    }
}

impl<'a> Header<'a> {
    pub const VT_NAME: VOffsetT = 4;
    pub const VT_ENVELOPE: VOffsetT = 6;
    pub const VT_GEOMETRY_TYPE: VOffsetT = 8;
    pub const VT_HAS_Z: VOffsetT = 10;
    pub const VT_HAS_M: VOffsetT = 12;
    pub const VT_HAS_T: VOffsetT = 14;
    pub const VT_HAS_TM: VOffsetT = 16;
    pub const VT_COLUMNS: VOffsetT = 18;
    pub const VT_FEATURES_COUNT: VOffsetT = 20;
    pub const VT_INDEX_NODE_SIZE: VOffsetT = 22;
    pub const VT_CRS: VOffsetT = 24;
    pub const VT_TITLE: VOffsetT = 26;
    pub const VT_DESCRIPTION: VOffsetT = 28;
    pub const VT_METADATA: VOffsetT = 30;

    #[inline]
    pub fn name(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Header::VT_NAME, None)
        }
    }
    #[inline]
    pub fn envelope(&self) -> Option<Vector<'a, f64>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, f64>>>(Header::VT_ENVELOPE, None)
        }
    }
    #[inline]
    pub fn geometry_type(&self) -> GeometryType {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<GeometryType>(Header::VT_GEOMETRY_TYPE, Some(GeometryType::Unknown))
                .unwrap()
        }
    }
    #[inline]
    pub fn has_z(&self) -> bool {
        // SAFETY: verified
        unsafe { self._tab.get::<u8>(Header::VT_HAS_Z, Some(0)).unwrap() != 0 }
    }
    #[inline]
    pub fn has_m(&self) -> bool {
        // SAFETY: verified
        unsafe { self._tab.get::<u8>(Header::VT_HAS_M, Some(0)).unwrap() != 0 }
    }
    #[inline]
    pub fn has_t(&self) -> bool {
        // SAFETY: verified
        unsafe { self._tab.get::<u8>(Header::VT_HAS_T, Some(0)).unwrap() != 0 }
    }
    #[inline]
    pub fn has_tm(&self) -> bool {
        // SAFETY: verified
        unsafe { self._tab.get::<u8>(Header::VT_HAS_TM, Some(0)).unwrap() != 0 }
    }
    #[inline]
    pub fn columns(&self) -> Option<Vector<'a, ForwardsUOffset<Column<'a>>>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, ForwardsUOffset<Column>>>>(
                    Header::VT_COLUMNS,
                    None,
                )
        }
    }
    #[inline]
    pub fn features_count(&self) -> u64 {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<u64>(Header::VT_FEATURES_COUNT, Some(0))
                .unwrap()
        }
    }
    #[inline]
    pub fn index_node_size(&self) -> u16 {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<u16>(Header::VT_INDEX_NODE_SIZE, Some(16))
                .unwrap()
        }
    }
    #[inline]
    pub fn crs(&self) -> Option<Crs<'a>> {
        // SAFETY: verified
        unsafe { self._tab.get::<ForwardsUOffset<Crs>>(Header::VT_CRS, None) }
    }
    #[inline]
    pub fn title(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Header::VT_TITLE, None)
        }
    }
    #[inline]
    pub fn description(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Header::VT_DESCRIPTION, None)
        }
    }
    #[inline]
    pub fn metadata(&self) -> Option<&'a [u8]> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<CppStr>>(Header::VT_METADATA, None)
        }
    }
}

impl Verifiable for Header<'_> {
    #[inline]
    fn run_verifier(v: &mut Verifier, pos: usize) -> Result<(), InvalidFlatbuffer> {
        v.visit_table(pos)?
            .visit_field::<ForwardsUOffset<CppStr>>("name", Self::VT_NAME, false)?
            .visit_field::<ForwardsUOffset<Vector<'_, f64>>>("envelope", Self::VT_ENVELOPE, false)?
            .visit_field::<GeometryType>("geometry_type", Self::VT_GEOMETRY_TYPE, false)?
            .visit_field::<u8>("has_z", Self::VT_HAS_Z, false)?
            .visit_field::<u8>("has_m", Self::VT_HAS_M, false)?
            .visit_field::<u8>("has_t", Self::VT_HAS_T, false)?
            .visit_field::<u8>("has_tm", Self::VT_HAS_TM, false)?
            .visit_field::<ForwardsUOffset<Vector<'_, ForwardsUOffset<Column>>>>(
                "columns",
                Self::VT_COLUMNS,
                false,
            )?
            .visit_field::<u64>("features_count", Self::VT_FEATURES_COUNT, false)?
            .visit_field::<u16>("index_node_size", Self::VT_INDEX_NODE_SIZE, false)?
            .visit_field::<ForwardsUOffset<Crs>>("crs", Self::VT_CRS, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("title", Self::VT_TITLE, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("description", Self::VT_DESCRIPTION, false)?
            .visit_field::<ForwardsUOffset<CppStr>>("metadata", Self::VT_METADATA, false)?
            .finish();
        Ok(())
    }
}

#[derive(Copy, Clone, PartialEq)]
pub struct Geometry<'a> {
    pub _tab: Table<'a>,
}

impl<'a> Follow<'a> for Geometry<'a> {
    type Inner = Geometry<'a>;
    #[inline]
    unsafe fn follow(buf: &'a [u8], loc: usize) -> Self::Inner {
        Self {
            // SAFETY: the caller has verified that a table is at loc.
            _tab: unsafe { Table::new(buf, loc) },
        }
    }
}

impl<'a> Geometry<'a> {
    pub const VT_ENDS: VOffsetT = 4;
    pub const VT_XY: VOffsetT = 6;
    pub const VT_Z: VOffsetT = 8;
    pub const VT_M: VOffsetT = 10;
    pub const VT_T: VOffsetT = 12;
    pub const VT_TM: VOffsetT = 14;
    pub const VT_TYPE: VOffsetT = 16;
    pub const VT_PARTS: VOffsetT = 18;

    #[inline]
    pub fn ends(&self) -> Option<Vector<'a, u32>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, u32>>>(Geometry::VT_ENDS, None)
        }
    }
    #[inline]
    pub fn xy(&self) -> Option<Vector<'a, f64>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, f64>>>(Geometry::VT_XY, None)
        }
    }
    #[inline]
    pub fn z(&self) -> Option<Vector<'a, f64>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, f64>>>(Geometry::VT_Z, None)
        }
    }
    #[inline]
    pub fn m(&self) -> Option<Vector<'a, f64>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, f64>>>(Geometry::VT_M, None)
        }
    }
    #[inline]
    pub fn t(&self) -> Option<Vector<'a, f64>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, f64>>>(Geometry::VT_T, None)
        }
    }
    #[inline]
    pub fn tm(&self) -> Option<Vector<'a, u64>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, u64>>>(Geometry::VT_TM, None)
        }
    }
    #[inline]
    pub fn type_(&self) -> GeometryType {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<GeometryType>(Geometry::VT_TYPE, Some(GeometryType::Unknown))
                .unwrap()
        }
    }
    #[inline]
    pub fn parts(&self) -> Option<Vector<'a, ForwardsUOffset<Geometry<'a>>>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, ForwardsUOffset<Geometry>>>>(
                    Geometry::VT_PARTS,
                    None,
                )
        }
    }
}

impl Verifiable for Geometry<'_> {
    #[inline]
    fn run_verifier(v: &mut Verifier, pos: usize) -> Result<(), InvalidFlatbuffer> {
        v.visit_table(pos)?
            .visit_field::<ForwardsUOffset<Vector<'_, u32>>>("ends", Self::VT_ENDS, false)?
            .visit_field::<ForwardsUOffset<Vector<'_, f64>>>("xy", Self::VT_XY, false)?
            .visit_field::<ForwardsUOffset<Vector<'_, f64>>>("z", Self::VT_Z, false)?
            .visit_field::<ForwardsUOffset<Vector<'_, f64>>>("m", Self::VT_M, false)?
            .visit_field::<ForwardsUOffset<Vector<'_, f64>>>("t", Self::VT_T, false)?
            .visit_field::<ForwardsUOffset<Vector<'_, u64>>>("tm", Self::VT_TM, false)?
            .visit_field::<GeometryType>("type_", Self::VT_TYPE, false)?
            .visit_field::<ForwardsUOffset<Vector<'_, ForwardsUOffset<Geometry>>>>(
                "parts",
                Self::VT_PARTS,
                false,
            )?
            .finish();
        Ok(())
    }
}

#[derive(Copy, Clone, PartialEq)]
pub struct Feature<'a> {
    pub _tab: Table<'a>,
}

impl<'a> Follow<'a> for Feature<'a> {
    type Inner = Feature<'a>;
    #[inline]
    unsafe fn follow(buf: &'a [u8], loc: usize) -> Self::Inner {
        Self {
            // SAFETY: the caller has verified that a table is at loc.
            _tab: unsafe { Table::new(buf, loc) },
        }
    }
}

impl<'a> Feature<'a> {
    pub const VT_GEOMETRY: VOffsetT = 4;
    pub const VT_PROPERTIES: VOffsetT = 6;
    pub const VT_COLUMNS: VOffsetT = 8;

    #[inline]
    pub fn geometry(&self) -> Option<Geometry<'a>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Geometry>>(Feature::VT_GEOMETRY, None)
        }
    }
    #[inline]
    pub fn properties(&self) -> Option<Vector<'a, u8>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, u8>>>(Feature::VT_PROPERTIES, None)
        }
    }
    #[inline]
    pub fn columns(&self) -> Option<Vector<'a, ForwardsUOffset<Column<'a>>>> {
        // SAFETY: verified
        unsafe {
            self._tab
                .get::<ForwardsUOffset<Vector<'a, ForwardsUOffset<Column>>>>(
                    Feature::VT_COLUMNS,
                    None,
                )
        }
    }
}

impl Verifiable for Feature<'_> {
    #[inline]
    fn run_verifier(v: &mut Verifier, pos: usize) -> Result<(), InvalidFlatbuffer> {
        v.visit_table(pos)?
            .visit_field::<ForwardsUOffset<Geometry>>("geometry", Self::VT_GEOMETRY, false)?
            .visit_field::<ForwardsUOffset<Vector<'_, u8>>>(
                "properties",
                Self::VT_PROPERTIES,
                false,
            )?
            .visit_field::<ForwardsUOffset<Vector<'_, ForwardsUOffset<Column>>>>(
                "columns",
                Self::VT_COLUMNS,
                false,
            )?
            .finish();
        Ok(())
    }
}

/// `VerifySizePrefixedHeaderBuffer` and then `GetSizePrefixedHeader`.
pub fn size_prefixed_root_as_header(buf: &[u8]) -> Result<Header<'_>, InvalidFlatbuffer> {
    flatbuffers::size_prefixed_root::<Header>(buf)
}

/// `VerifySizePrefixedFeatureBuffer` and then `GetSizePrefixedFeature`.
pub fn size_prefixed_root_as_feature(buf: &[u8]) -> Result<Feature<'_>, InvalidFlatbuffer> {
    flatbuffers::size_prefixed_root::<Feature>(buf)
}
