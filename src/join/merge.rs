//! tile-join's queue of tileset readers, which decides which input tiles go
//! into each output tile and in what order, and the order in which the
//! inputs' metadata is read at the end.
//!
//! This follows tile-join's `tileset_reader` and its `decode` loop step for
//! step: the readers sorted by their current tile (zoom, x, TMS y, then the
//! tile's bytes, see `key.rs`), the first taken, its tile added, the reader
//! advanced and put back in order. Overzoomed tiles are made the same way,
//! from the same parent tiles, in the same order, but a batch at a time on all
//! threads instead of one at a time, which changes nothing else, since each is
//! a function of its parent tile alone.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use rayon::prelude::*;
use tippecanoe::errors::{EXIT_IMPOSSIBLE, EXIT_PROTOBUF};
use tippecanoe::geometry::SourceTile;
use tippecanoe::mvt::MvtTile;

use super::key::{Key, TieBreak, cmp};
use super::source::{Loc, Source, TileRef};
use super::tile::{decompress, is_compressed};
use crate::{Error, Result};

/// A tile's zoom, x and y.
type Zxy = (i64, i64, i64);

/// Overzoomed tiles made at a time, on all threads.
const OVERZOOM_BATCH: usize = 256;

/// What the readers' order depends on beyond their tiles.
pub struct Settings<'a> {
    pub want_overzoom: bool,
    pub buffer: i32,
    pub tie_break: TieBreak,
    pub unidecode_data: &'a [String],
}

/// An overzoomed tile made ahead of its turn, with the child tiles that will
/// have features in them.
struct Made {
    xy: (u32, u32),
    data: Arc<[u8]>,
    children: Vec<(u32, u32)>,
}

pub struct Reader<'s> {
    pub source: &'s Source,
    /// The input's number on the command line.
    pub index: usize,

    zoom: i64,
    x: i64,
    y: i64,
    /// The current tile: where its data is.
    tile: TileRef,
    /// The current data was made by overzooming, so it is never one of the
    /// tiles that `key.rs` compares by their zlib bytes.
    made: bool,
    key: Option<Key>,
    current_tile_is_overzoomed: bool,
    done: bool,

    maxzoom_so_far: i32,
    tiles_at_maxzoom_so_far: Vec<(u32, u32)>,
    overzoomed_tiles: Vec<(u32, u32)>,
    next_overzoomed_tiles: Vec<(u32, u32)>,
    overzoom_consumed_at_this_zoom: bool,

    pending: VecDeque<TileRef>,
    exhausted: bool,
    ready: VecDeque<Made>,
}

/// `tilecmp`: x descending, then y ascending, so that popping from the end
/// gives tile-join's reading order.
fn tilecmp(a: &(u32, u32), b: &(u32, u32)) -> Ordering {
    if b.0 < a.0 {
        return Ordering::Less;
    }
    if b.0 == a.0 {
        if b.1 > a.1 {
            return Ordering::Less;
        }
        if b.1 == a.1 {
            return Ordering::Equal;
        }
    }
    Ordering::Greater
}

impl<'s> Reader<'s> {
    fn new(source: &'s Source, index: usize) -> Reader<'s> {
        Reader {
            source,
            index,
            zoom: 0,
            x: 0,
            y: 0,
            tile: TileRef {
                z: 0,
                x: 0,
                y: 0,
                loc: Loc::Data(Arc::from(&[][..])),
            },
            made: true,
            key: None,
            current_tile_is_overzoomed: false,
            done: false,
            maxzoom_so_far: -1,
            tiles_at_maxzoom_so_far: Vec::new(),
            overzoomed_tiles: Vec::new(),
            next_overzoomed_tiles: Vec::new(),
            overzoom_consumed_at_this_zoom: false,
            pending: VecDeque::new(),
            exhausted: false,
            ready: VecDeque::new(),
        }
    }

    /// `begin_reading`: the reader at its first tile.
    pub fn begin(source: &'s Source, index: usize, s: &Settings<'_>) -> Result<Reader<'s>> {
        let mut r = Reader::new(source, index);
        r.advance(s)?;
        Ok(r)
    }

    fn next_source_tile(&mut self) -> Option<TileRef> {
        while self.pending.is_empty() && !self.exhausted {
            match self.source.next_chunk() {
                Some(chunk) => self.pending.extend(chunk),
                None => self.exhausted = true,
            }
        }
        self.pending.pop_front()
    }

    fn set_data(&mut self, tile: TileRef, made: bool) {
        self.tile = tile;
        self.made = made;
        self.key = None;
    }

    /// The key of the current tile, worked out the first time it is needed.
    fn key(&mut self, s: &Settings<'_>) -> Result<&Key> {
        if self.key.is_none() {
            let data = self.source.data(&self.tile)?;
            let key = if self.made {
                Key::Bytes(data)
            } else {
                Key::new(data, self.source.tippecanoe_made, s.tie_break)
            };
            self.key = Some(key);
        }
        Ok(self.key.as_ref().expect("just set"))
    }

    /// `all_done` for this reader alone.
    fn finished(&self) -> bool {
        self.done && !self.overzoom_consumed_at_this_zoom
    }

    /// `current()`: the tile, marking an overzoomed one as used.
    fn current(&mut self) -> (i64, i64, i64, TileRef) {
        if self.current_tile_is_overzoomed {
            self.overzoom_consumed_at_this_zoom = true;
        }
        (self.zoom, self.x, self.y, self.tile.clone())
    }

    fn advance(&mut self, s: &Settings<'_>) -> Result<()> {
        if self.done {
            if !s.want_overzoom {
                return Err(Error::new(
                    EXIT_IMPOSSIBLE,
                    "overzoom advance called without -O",
                ));
            }
            if self.overzoomed_tiles.is_empty() {
                self.next_overzoom();
                self.overzoom_consumed_at_this_zoom = false;
            }
            let Some(xy) = self.overzoomed_tiles.last().copied() else {
                // we have nothing to overzoom; give up
                self.current_tile_is_overzoomed = false;
                self.zoom = 32;
                return Ok(());
            };
            if self.ready.is_empty() {
                self.make_overzoomed(s)?;
            }
            self.overzoomed_tiles.pop();
            let made = self.ready.pop_front().expect("a batch was made");
            debug_assert_eq!(made.xy, xy);
            self.next_overzoomed_tiles.extend_from_slice(&made.children);
            self.x = xy.0 as i64;
            self.y = xy.1 as i64;
            self.set_data(
                TileRef {
                    z: self.zoom,
                    x: self.x,
                    y: self.y,
                    loc: Loc::Data(made.data),
                },
                true,
            );
            self.current_tile_is_overzoomed = true;
            return Ok(());
        }

        self.current_tile_is_overzoomed = false;
        match self.next_source_tile() {
            Some(t) => {
                self.zoom = t.z;
                self.x = t.x;
                self.y = t.y;
                self.set_data(t, false);
            }
            None => self.done = true,
        }

        if self.done {
            if s.want_overzoom {
                self.next_overzoom();
                self.advance(s)?;
            } else {
                self.zoom = 32;
            }
        } else {
            if self.zoom > self.maxzoom_so_far as i64 {
                self.maxzoom_so_far = self.zoom as i32;
                self.tiles_at_maxzoom_so_far.clear();
            }
            if s.want_overzoom {
                self.tiles_at_maxzoom_so_far
                    .push((self.x as u32, self.y as u32));
            }
        }
        Ok(())
    }

    fn next_overzoom(&mut self) {
        self.zoom += 1;
        self.overzoomed_tiles.clear();
        self.ready.clear();

        // +1 because maxzoom_so_far is initially -1, an invalid shift
        let scale: i64 = 1i64.wrapping_shl((self.zoom + 1) as u32)
            / 1i64.wrapping_shl((self.maxzoom_so_far + 1) as u32);

        if self.zoom == self.maxzoom_so_far as i64 + 1 {
            for xy in &self.tiles_at_maxzoom_so_far {
                for xx in 0..scale {
                    for yy in 0..scale {
                        self.overzoomed_tiles.push((
                            (xy.0 as i64 * scale + xx) as u32,
                            (xy.1 as i64 * scale + yy) as u32,
                        ));
                    }
                }
            }
        } else {
            self.overzoomed_tiles = std::mem::take(&mut self.next_overzoomed_tiles);
        }

        self.overzoomed_tiles.sort_by(tilecmp);
        self.overzoom_consumed_at_this_zoom = false;
    }

    /// Makes the next overzoomed tiles, in the order they will be taken,
    /// as `retrieve_overzoom` makes each.
    fn make_overzoomed(&mut self, s: &Settings<'_>) -> Result<()> {
        let n = self.overzoomed_tiles.len().min(OVERZOOM_BATCH);
        let tiles: Vec<(u32, u32)> = self
            .overzoomed_tiles
            .iter()
            .rev()
            .take(n)
            .copied()
            .collect();
        let z = self.zoom;
        let maxzoom = self.maxzoom_so_far as i64;
        let parent_of = |xy: &(u32, u32)| {
            let (mut pz, mut px, mut py) = (z, xy.0 as i64, xy.1 as i64);
            while pz > maxzoom {
                pz -= 1;
                px /= 2;
                py /= 2;
            }
            (pz, px, py)
        };

        let parents: BTreeSet<Zxy> = tiles.iter().map(parent_of).collect();
        let source = self.source;
        let decoded: Vec<Result<(Zxy, Arc<MvtTile>)>> = parents
            .into_par_iter()
            .map(|p| Ok((p, Arc::new(parent_tile(source, p)?))))
            .collect();
        let mut parent_tiles: HashMap<Zxy, Arc<MvtTile>> = HashMap::new();
        for d in decoded {
            let (p, t) = d?;
            parent_tiles.insert(p, t);
        }

        let made: Vec<Made> = tiles
            .par_iter()
            .map(|xy| {
                let p = parent_of(xy);
                let parent = &parent_tiles[&p];
                let mut children = Vec::new();
                let data = if parent.layers.is_empty() {
                    Vec::new()
                } else {
                    let tv = vec![SourceTile {
                        tile: (**parent).clone(),
                        z: p.0 as i32,
                        x: p.1 as i32,
                        y: p.2 as i32,
                    }];
                    tippecanoe::clip::overzoom_source_tiles(
                        &tv,
                        z as i32,
                        xy.0 as i32,
                        xy.1 as i32,
                        -1,
                        s.buffer,
                        &BTreeSet::new(),
                        &BTreeSet::new(),
                        &[],
                        false,
                        Some(&mut children),
                        false,
                        None,
                        false,
                        &HashMap::new(),
                        s.unidecode_data,
                        0.0,
                        0.0,
                        &[],
                        "",
                        "",
                        usize::MAX,
                        &[],
                        false,
                    )
                };
                Made {
                    xy: *xy,
                    data: data.into(),
                    children,
                }
            })
            .collect();
        self.ready.extend(made);
        Ok(())
    }

    /// `operator<`: by zoom, x, TMS y, then the tiles' bytes.
    fn lt(&mut self, r: &mut Reader<'_>, s: &Settings<'_>) -> Result<bool> {
        if self.zoom < r.zoom {
            return Ok(true);
        }
        if self.zoom > r.zoom {
            return Ok(false);
        }
        if self.x < r.x {
            return Ok(true);
        }
        if self.x > r.x {
            return Ok(false);
        }
        let sorty = 1i64
            .wrapping_shl(self.zoom as u32)
            .wrapping_sub(1)
            .wrapping_sub(self.y) as i32;
        let r_sorty = 1i64
            .wrapping_shl(r.zoom as u32)
            .wrapping_sub(1)
            .wrapping_sub(r.y) as i32;
        if sorty < r_sorty {
            return Ok(true);
        }
        if sorty > r_sorty {
            return Ok(false);
        }
        let a = self.key(s)?;
        let b = r.key(s)?;
        Ok(cmp(a, b) == Ordering::Less)
    }
}

/// tile-join's `get_tile` for overzooming, decoded, without the hash tables
/// tippecanoe-rs's decoder gives each layer, which only output layers use.
fn parent_tile(source: &Source, (z, x, y): Zxy) -> Result<MvtTile> {
    let data = source.get_tile(z, x, y)?;
    let mut content = MvtTile::default();
    if data.is_empty() {
        return Ok(content);
    }
    let mut buf = Vec::new();
    let src: &[u8] = if is_compressed(&data) {
        let mut d = libdeflater::Decompressor::new();
        if !decompress(&mut d, &data, &mut buf) {
            return Err(Error::new(tippecanoe::errors::EXIT_MVT, String::new()));
        }
        &buf
    } else {
        &data
    };
    let mut was_compressed = false;
    if content.try_decode(src, &mut was_compressed).is_err() {
        return Err(Error::new(
            EXIT_PROTOBUF,
            format!("PBF decoding error in tile {}/{}/{}", z, x, y),
        ));
    }
    for layer in &mut content.layers {
        layer.key_dedup = Vec::new();
        layer.value_dedup = Vec::new();
    }
    Ok(content)
}

/// The input tiles of one output tile, in the order tile-join appends them.
pub struct Group {
    pub z: i64,
    pub x: i64,
    pub y: i64,
    /// (input number, tile)
    pub members: Vec<(usize, TileRef)>,
}

/// The readers in tile-join's order, with the next tile of the first.
pub struct Queue<'s> {
    pub readers: Vec<Reader<'s>>,
    /// Reader numbers, first to last.
    pub order: Vec<usize>,
}

impl<'s> Queue<'s> {
    pub fn new() -> Queue<'s> {
        Queue {
            readers: Vec::new(),
            order: Vec::new(),
        }
    }

    /// `insert_reader`: before the first reader that it sorts before.
    pub fn insert(&mut self, r: usize, s: &Settings<'_>) -> Result<()> {
        let mut at = self.order.len();
        for (i, &here) in self.order.iter().enumerate() {
            let (a, b) = two(&mut self.readers, r, here);
            if a.lt(b, s)? {
                at = i;
                break;
            }
        }
        self.order.insert(at, r);
        Ok(())
    }

    /// Adds a reader that has begun.
    pub fn add(&mut self, reader: Reader<'s>, s: &Settings<'_>) -> Result<()> {
        self.readers.push(reader);
        self.insert(self.readers.len() - 1, s)
    }

    fn all_done(&self) -> bool {
        self.readers.iter().all(|r| r.finished())
    }

    /// Runs tile-join's loop until `limit` groups are ready or the readers
    /// are done, adding the groups for zooms `minzoom` to `maxzoom` to `out`.
    /// Returns false when the readers are done.
    pub fn run(
        &mut self,
        s: &Settings<'_>,
        minzoom: i32,
        maxzoom: i32,
        limit: usize,
        out: &mut Vec<Group>,
    ) -> Result<bool> {
        loop {
            if self.order.is_empty() || self.all_done() {
                return Ok(false);
            }
            if out.len() >= limit {
                // stop between tiles, never within one
                let r = &self.readers[self.order[0]];
                let last = &out[out.len() - 1];
                if !(r.zoom == last.z && r.x == last.x && r.y == last.y) {
                    return Ok(true);
                }
            }
            let head = self.order.remove(0);
            let (z, x, y, tile) = self.readers[head].current();
            if z >= minzoom as i64 && z <= maxzoom as i64 {
                match out.last_mut() {
                    Some(g) if g.z == z && g.x == x && g.y == y => {
                        g.members.push((self.readers[head].index, tile))
                    }
                    _ => out.push(Group {
                        z,
                        x,
                        y,
                        members: vec![(self.readers[head].index, tile)],
                    }),
                }
            }
            self.readers[head].advance(s)?;
            self.insert(head, s)?;
        }
    }
}

impl Default for Queue<'_> {
    fn default() -> Self {
        Queue::new()
    }
}

/// Two distinct readers, mutably.
fn two<'a, 's>(
    readers: &'a mut [Reader<'s>],
    a: usize,
    b: usize,
) -> (&'a mut Reader<'s>, &'a mut Reader<'s>) {
    assert_ne!(a, b);
    if a < b {
        let (l, r) = readers.split_at_mut(b);
        (&mut l[a], &mut r[0])
    } else {
        let (l, r) = readers.split_at_mut(a);
        (&mut r[0], &mut l[b])
    }
}
