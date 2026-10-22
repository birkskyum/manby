//! Everything a run needs to know, parsed from tippecanoe's command line options
//! or filled in by a library user. Replaces tippecanoe's global variables.

/// Where the tiles go.
#[derive(Clone, Debug, Default)]
pub enum Output {
    #[default]
    None,
    Pmtiles(String),
    Mbtiles(String),
    Directory(String),
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub inputs: Vec<String>,
    pub output: Output,
    pub force: bool,
    pub quiet: bool,
    /// Worker threads; 0 means one per available CPU, as tippecanoe chooses.
    pub threads: usize,
    /// Make different tiles from tippecanoe's where that is faster. See DESIGN.md.
    pub fast: bool,
}

/// Parses a tippecanoe-style command line (`argv[0]` included).
pub fn parse_args(argv: &[String]) -> crate::Result<Options> {
    todo!("options::parse_args")
}
