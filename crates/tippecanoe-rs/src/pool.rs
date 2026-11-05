//! Port of pool.hpp and pool.cpp: the string pool of attribute keys and values,
//! with a binary search tree (kept in a memfile) to deduplicate them.

// Kept in the shape of the C++ rather than in the shape clippy prefers.
#![allow(
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::identity_op,
    clippy::if_same_then_else,
    clippy::needless_late_init,
    clippy::only_used_in_recursion,
    clippy::manual_clamp,
    clippy::manual_div_ceil,
    clippy::manual_range_contains,
    clippy::vec_init_then_push,
    clippy::int_plus_one,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use std::sync::atomic::{AtomicBool, Ordering};

use crate::cstd::perror;
use crate::errors::EXIT_WRITE;
use crate::globals::{CPUS, MEMSIZE, get};
use crate::memfile::{Memfile, memfile_full, memfile_write};
use crate::text::fnv1a_cstr;

/// `struct stringpool`, a node of the search tree. In the tree memfile each node
/// is four native-endian 64-bit words in this order.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stringpool {
    pub left: u64,
    pub right: u64,

    pub off: u64,
    /// hash of the string at this node
    pub hash: u64,
}

pub const STRINGPOOL_SIZE: usize = 32;

const LEFT: usize = 0;
const RIGHT: usize = 8;
const OFF: usize = 16;
const HASH: usize = 24;

impl Stringpool {
    pub fn to_bytes(&self) -> [u8; STRINGPOOL_SIZE] {
        let mut out = [0u8; STRINGPOOL_SIZE];
        out[LEFT..LEFT + 8].copy_from_slice(&self.left.to_ne_bytes());
        out[RIGHT..RIGHT + 8].copy_from_slice(&self.right.to_ne_bytes());
        out[OFF..OFF + 8].copy_from_slice(&self.off.to_ne_bytes());
        out[HASH..HASH + 8].copy_from_slice(&self.hash.to_ne_bytes());
        out
    }
}

fn get_word(map: &[u8], at: usize) -> u64 {
    u64::from_ne_bytes(map[at..at + 8].try_into().unwrap())
}

fn set_word(map: &mut [u8], at: usize, v: u64) {
    map[at..at + 8].copy_from_slice(&v.to_ne_bytes());
}

/// The NUL-terminated string that starts at `off`.
fn cstr_at(map: &[u8], off: usize) -> &[u8] {
    let s = &map[off..];
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    &s[..end]
}

#[inline]
fn swizzlecmp(a: &[u8], atype: i32, ahash: u64, b: &[u8], btype: i32, bhash: u64) -> i64 {
    if ahash as i64 == bhash as i64 {
        if atype == btype {
            // strcmp
            match a.cmp(b) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }
        } else {
            (atype - btype) as i64
        }
    } else {
        ahash.wrapping_sub(bhash) as i64
    }
}

/// Writes the type byte and the NUL-terminated string to the pool.
fn write_entry(poolfile: &mut Memfile, s: &[u8], type_: i8, in_memory: &mut bool) {
    if memfile_write(poolfile, &[type_ as u8], in_memory) < 0 {
        perror("memfile write");
        std::process::exit(EXIT_WRITE);
    }
    let mut z = Vec::with_capacity(s.len() + 1);
    z.extend_from_slice(s);
    z.push(0);
    if memfile_write(poolfile, &z, in_memory) < 0 {
        perror("memfile write");
        std::process::exit(EXIT_WRITE);
    }
}

/// `s` is a C string: it ends at its first NUL, if it has one.
#[allow(clippy::absurd_extreme_comparisons)]
pub fn addpool(
    poolfile: &mut Memfile,
    treefile: &mut Memfile,
    s: &[u8],
    type_: i8,
    dedup: &mut [isize],
) -> i64 {
    let s = match s.iter().position(|&c| c == 0) {
        Some(end) => &s[..end],
        None => s,
    };

    let hash = fnv1a_cstr(s, type_ as u8);
    let hash_off = (hash % dedup.len() as u64) as usize;

    if dedup[hash_off] >= 0
        && poolfile.map[dedup[hash_off] as usize] as i8 == type_
        && cstr_at(&poolfile.map, dedup[hash_off] as usize + 1) == s
    {
        // printf("hit for %s\n", s);
        return dedup[hash_off] as i64;
    } else {
        // printf("miss for %s\n", s);
    }

    // The link being followed: None for `treefile->tree`, otherwise the
    // offset of a left or right word within the tree memfile.
    let mut sp: Option<usize> = None;
    let mut depth: usize = 0;

    // In typical data, traversal depth generally stays under 2.5x
    let mut max = (3.0 * ((treefile.off / STRINGPOOL_SIZE) as f64).ln() / 2f64.ln()) as usize;
    if max < 30 {
        max = 30;
    }

    loop {
        let node = match sp {
            None => treefile.tree,
            Some(at) => get_word(&treefile.map, at),
        };
        if node == 0 {
            break;
        }
        let node = node as usize;

        let n_off = get_word(&treefile.map, node + OFF) as usize;
        let n_hash = get_word(&treefile.map, node + HASH);
        let cmp = swizzlecmp(
            s,
            type_ as i32,
            hash,
            cstr_at(&poolfile.map, n_off + 1),
            poolfile.map[n_off] as i8 as i32,
            n_hash,
        );

        if cmp < 0 {
            sp = Some(node + LEFT);
        } else if cmp > 0 {
            sp = Some(node + RIGHT);
        } else {
            dedup[hash_off] = n_off as isize;
            return n_off as i64;
        }

        depth += 1;
        if depth > max {
            // Search is very deep, so string is probably unique.
            // Add it to the pool without adding it to the search tree.
            // This might go either to memory or the file, depending on whether
            // the pool is full yet.

            let off = poolfile.off as i64;
            let mut in_memory = false;

            write_entry(poolfile, s, type_, &mut in_memory);

            if in_memory {
                dedup[hash_off] = off as isize;
            }
            return off;
        }
    }

    // Size of memory divided by 10 from observation of OOM errors (when supposedly
    // 20% of memory is full) and onset of thrashing (when supposedly 15% of memory
    // is full) on ECS.
    if poolfile.off + treefile.off > get(&MEMSIZE) / get(&CPUS) / 10 {
        // If the pool and search tree get to be larger than physical memory,
        // then searching will start thrashing. Switch to appending strings
        // to the file instead of keeping them in memory.

        if poolfile.fp.is_none() {
            memfile_full(poolfile);
        }
    }

    if poolfile.fp.is_some() {
        // We are now appending to the file, so don't try to keep tree references
        // to the newly-added strings.

        let off = poolfile.off as i64;
        let mut in_memory = false;
        write_entry(poolfile, s, type_, &mut in_memory);
        return off;
    }

    // *sp is probably in the memory-mapped file, and will move if the file grows.
    let ssp: i64 = match sp {
        None => -1,
        Some(at) => at as i64,
    };

    let off = poolfile.off as i64;
    let mut in_memory = false;
    write_entry(poolfile, s, type_, &mut in_memory);
    if in_memory {
        dedup[hash_off] = off as isize;
    }

    if off >= i64::MAX || treefile.off as i64 >= i64::MAX {
        // Tree or pool is bigger than 2GB
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.load(Ordering::Relaxed) {
            eprintln!("Warning: string pool is very large.");
            WARNED.store(true, Ordering::Relaxed);
        }
        return off;
    }

    let tsp = Stringpool {
        left: 0,
        right: 0,
        off: off as u64,
        hash,
    };

    let p = treefile.off as u64;
    let mut tree_in_memory = false;
    if memfile_write(treefile, &tsp.to_bytes(), &mut tree_in_memory) < 0 {
        perror("memfile write");
        std::process::exit(EXIT_WRITE);
    }

    if ssp == -1 {
        treefile.tree = p;
    } else {
        set_word(&mut treefile.map, ssp as usize, p);
    }
    off
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memfile::{memfile_close, memfile_open};
    use std::sync::atomic::Ordering;

    fn tmpfd() -> i32 {
        let mut name = String::from("/tmp/pool-test.XXXXXX");
        let fd = crate::tippecanoe_main::mkstemp_cloexec(&mut name);
        assert!(fd >= 0);
        std::fs::remove_file(&name).unwrap();
        fd
    }

    #[test]
    fn dedup_and_tree() {
        if get(&CPUS) == 0 {
            CPUS.store(1, Ordering::Relaxed);
        }
        if get(&MEMSIZE) == 0 {
            MEMSIZE.store(1 << 30, Ordering::Relaxed);
        }

        let mut pool = memfile_open(tmpfd()).unwrap();
        let mut tree = memfile_open(tmpfd()).unwrap();
        let mut in_memory = false;
        memfile_write(&mut tree, &Stringpool::default().to_bytes(), &mut in_memory);

        let mut dedup = vec![-1isize; 655536];

        let a = addpool(&mut pool, &mut tree, b"name", 0, &mut dedup);
        let b = addpool(&mut pool, &mut tree, b"other", 0, &mut dedup);
        let c = addpool(&mut pool, &mut tree, b"name", 2, &mut dedup);
        assert_eq!(a, 0);
        assert_eq!(b, 6);
        assert_eq!(c, 13);

        // Found again through the dedup table and through the tree
        assert_eq!(addpool(&mut pool, &mut tree, b"name", 0, &mut dedup), a);
        for d in dedup.iter_mut() {
            *d = -1;
        }
        assert_eq!(addpool(&mut pool, &mut tree, b"other", 0, &mut dedup), b);
        assert_eq!(addpool(&mut pool, &mut tree, b"name", 2, &mut dedup), c);
        assert_eq!(addpool(&mut pool, &mut tree, b"name", 0, &mut dedup), a);

        // A C string ends at its first NUL
        assert_eq!(
            addpool(&mut pool, &mut tree, b"other\0more", 0, &mut dedup),
            b
        );

        assert_eq!(&pool.map, b"\0name\0\0other\0\x02name\0");
        assert_eq!(tree.off, 4 * STRINGPOOL_SIZE);

        for i in 0..2000 {
            let s = format!("value{}", i);
            let off = addpool(&mut pool, &mut tree, s.as_bytes(), 0, &mut dedup);
            assert_eq!(cstr_at(&pool.map, off as usize + 1), s.as_bytes());
        }
        for i in 0..2000 {
            let s = format!("value{}", i);
            let before = pool.off;
            let off = addpool(&mut pool, &mut tree, s.as_bytes(), 0, &mut dedup);
            assert_eq!(pool.off, before);
            assert_eq!(cstr_at(&pool.map, off as usize + 1), s.as_bytes());
        }

        assert_eq!(memfile_close(pool), 0);
        assert_eq!(memfile_close(tree), 0);
    }
}
