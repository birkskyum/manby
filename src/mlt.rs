//! MapLibre Tiles (MLT), written instead of Mapbox Vector Tiles with
//! `--tile-format=mlt`. See DESIGN.md.
//!
//! The tiler makes each tile's MVT bytes as usual, and `encode_tile` turns
//! them into MLT with mlt-core before the tile is compressed and its size
//! checked. An MLT tile therefore holds the layers, features, ids, geometry
//! and attribute values of the MVT tile, in the same order, typed as MLT's
//! columns require: one type per attribute and layer, where integers mixed
//! with floats become 64-bit floats and any other mix becomes strings.
//!
//! mlt-core needs Rust 1.98, so it is behind the `mlt` cargo feature.

/// The tile encoding manby writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TileFormat {
    #[default]
    Mvt,
    Mlt,
}

impl TileFormat {
    pub fn parse(s: &str) -> Option<TileFormat> {
        match s {
            "mvt" | "pbf" => Some(TileFormat::Mvt),
            "mlt" => Some(TileFormat::Mlt),
            _ => None,
        }
    }

    /// The `format` of the tileset metadata (MBTiles, TileJSON, PMTiles).
    pub fn metadata_format(self) -> &'static str {
        match self {
            TileFormat::Mvt => "pbf",
            TileFormat::Mlt => "mlt",
        }
    }

    /// The extension of tile files in a directory, with its dot.
    pub fn extension(self) -> &'static str {
        match self {
            TileFormat::Mvt => ".pbf",
            TileFormat::Mlt => ".mlt",
        }
    }
}

/// Which encodings the MLT encoder may use, by the clients the tiles are for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MltProfile {
    /// For MapLibre GL JS and every other client: no polygon triangles (GL
    /// JS ignores them), and FastPFOR and FSST only for tiles stored without
    /// gzip, since under gzip they make tiles larger.
    #[default]
    Web,
    /// For MapLibre Native: polygon triangles where they save the client
    /// more time than their bytes cost, and every codec.
    Native,
    /// The fewest bytes. Sorts features within each layer when that is
    /// smaller, so their drawing order can differ from MVT.
    MinSize,
    /// The quickest to encode: no sorting, no optional codecs.
    Fast,
}

impl MltProfile {
    pub fn parse(s: &str) -> Option<MltProfile> {
        match s {
            "web" => Some(MltProfile::Web),
            "native" => Some(MltProfile::Native),
            "min-size" => Some(MltProfile::MinSize),
            "fast" => Some(MltProfile::Fast),
            _ => None,
        }
    }
}

/// How tiles are encoded as MLT.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MltSettings {
    pub profile: MltProfile,
    /// The MLT bytes are gzipped afterwards (no `-pC`).
    pub gzipped: bool,
}

/// Whether this manby was built with MLT support.
pub const AVAILABLE: bool = cfg!(feature = "mlt");

#[cfg(feature = "mlt")]
mod imp {
    use mlt_core::encoder::{EncoderConfig, Network, Profile};
    use mlt_core::{Decoder, Parser, TileLayer};

    use super::{MltProfile, MltSettings};

    /// mlt-core's encoder settings for `s`.
    pub fn encoder_config(s: &MltSettings) -> EncoderConfig {
        // Tiles behind gzip are priced like tiles a client fetches over a
        // network; tiles without it are what the client stores and reads.
        let network = if s.gzipped {
            Network::Cellular4G
        } else {
            Network::Offline
        };
        EncoderConfig::from_profile(match s.profile {
            MltProfile::Web => Profile::Web(network),
            MltProfile::Native => Profile::Native(network),
            MltProfile::MinSize => Profile::MinSize,
            MltProfile::Fast => Profile::FastEncoding,
        })
    }

    /// The MLT tile with the content of the MVT tile `mvt`.
    pub fn encode_tile(mvt: &[u8], s: &MltSettings) -> Result<Vec<u8>, String> {
        let config = encoder_config(s);
        let mut out = Vec::with_capacity(mvt.len());
        for layer in mlt_core::mvt::mvt_to_tile_layers(mvt).map_err(|e| e.to_string())? {
            out.extend_from_slice(
                &layer
                    .encode(config)
                    .map_err(|e| format!("MLT layer {}: {}", layer.name(), e))?,
            );
        }
        Ok(out)
    }

    /// The layers of an MLT tile, decoded.
    pub fn decode_tile(mlt: &[u8]) -> Result<Vec<TileLayer>, String> {
        let mut decoder = Decoder::default();
        let mut layers = Vec::new();
        for layer in Parser::default()
            .parse_layers(mlt)
            .map_err(|e| e.to_string())?
        {
            match layer.into_tile(&mut decoder).map_err(|e| e.to_string())? {
                Some(l) => layers.push(l),
                None => return Err("the tile has a layer of an unknown kind".to_string()),
            }
        }
        Ok(layers)
    }
}

#[cfg(feature = "mlt")]
pub use imp::{decode_tile, encoder_config};

/// The MLT tile with the content of the MVT tile `mvt`.
pub fn encode_tile(mvt: &[u8], settings: &MltSettings) -> Result<Vec<u8>, String> {
    #[cfg(feature = "mlt")]
    {
        imp::encode_tile(mvt, settings)
    }
    #[cfg(not(feature = "mlt"))]
    {
        let _ = (mvt, settings);
        Err(NOT_BUILT.to_string())
    }
}

/// The error for `--tile-format=mlt` in a manby built without the feature.
pub const NOT_BUILT: &str = "this manby was built without MLT support; build it with `cargo +1.98.1 build --release --features mlt`";
