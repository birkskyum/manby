//! Port of pmtiles/pmtiles.hpp, the PMTiles v3 format library.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryZxy {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub offset: u64,
    pub length: u32,
}

impl EntryZxy {
    pub fn new(z: u8, x: u32, y: u32, offset: u64, length: u32) -> EntryZxy {
        EntryZxy {
            z,
            x,
            y,
            offset,
            length,
        }
    }
}
