//! A faithful Rust port of tippecanoe. See PORTING.md for how the C++ maps to Rust.

// Vendored libraries
pub mod getopt;
pub mod jsonpull;
pub mod milo;
pub mod pmtiles;
pub mod protozero;

// Shared definitions
pub mod cstd;
pub mod errors;
pub mod globals;
pub mod options;
pub mod version;

// One module per C++ source file
pub mod attribute;
pub mod clip;
pub mod compression;
pub mod csv;
pub mod decode;
pub mod dirtiles;
pub mod enumerate;
pub mod evaluator;
pub mod flatgeobuf;
pub mod flatgeobuf_generated;
pub mod geobuf;
pub mod geocsv;
pub mod geojson;
pub mod geojson_loop;
pub mod geometry;
pub mod json_logger;
pub mod jsontool;
pub mod mbtiles;
pub mod memfile;
pub mod mvt;
pub mod overzoom;
pub mod platform;
pub mod plugin;
pub mod pmtiles_file;
pub mod pool;
pub mod projection;
pub mod read_json;
pub mod serial;
pub mod shared_borders;
pub mod sort;
pub mod text;
pub mod thread;
pub mod tile;
pub mod tile_cache;
pub mod tile_join;
pub mod tippecanoe_main;
pub mod usage;
pub mod visvalingam;
pub mod write_json;
