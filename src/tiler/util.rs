//! Small functions from tippecanoe's tile.cpp that tippecanoe-rs keeps private.

use std::cmp::Ordering;

use tippecanoe::geometry::{Draw, DrawVec, VT_LINETO, VT_MOVETO};
use tippecanoe::mvt::{MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_SINT, MVT_UINT, MvtValue};
use tippecanoe::text::bit_reverse;

/// `SHIFT_RIGHT(a)`.
pub fn shift_right(a: i64, geometry_scale: i32) -> i64 {
    (a as f64 / (1i64 << geometry_scale) as f64).round() as i64
}

/// Turns a C++ "less than" comparator into the `Ordering` that `sort_by` wants.
/// Both sorts are stable, so for a strict weak ordering they give the same order.
pub fn by_less<T>(less: impl Fn(&T, &T) -> bool) -> impl FnMut(&T, &T) -> Ordering {
    move |a, b| {
        if less(a, b) {
            Ordering::Less
        } else if less(b, a) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

/// `std::stable_sort` with `operator<`.
pub fn stable_sort_less<T: PartialOrd>(v: &mut [T]) {
    v.sort_by(by_less(|a: &T, b: &T| a < b));
}

pub fn choose_mingap(gaps: &mut [u64], f: f64, existing_gap: u64) -> u64 {
    // equal integers are indistinguishable, so this orders them as std::stable_sort does
    gaps.sort_unstable();

    let mut ix = (gaps.len().wrapping_sub(1) as f64 * (1.0 - f)) as usize;
    while ix + 1 < gaps.len() && gaps[ix] <= existing_gap {
        ix += 1;
    }

    gaps[ix]
}

pub fn choose_minextent(extents: &mut [i64], f: f64, existing_extent: i64) -> i64 {
    // equal integers are indistinguishable, so this orders them as std::stable_sort does
    extents.sort_unstable();

    let mut ix = (extents.len().wrapping_sub(1) as f64 * (1.0 - f)) as usize;
    while ix + 1 < extents.len() && extents[ix] <= existing_extent {
        ix += 1;
    }

    extents[ix]
}

pub fn choose_mindrop_sequence(drop_sequences: &mut [u64], f: f64, existing: u64) -> u64 {
    if drop_sequences.is_empty() {
        return u64::MAX;
    }

    // equal integers are indistinguishable, so this orders them as std::stable_sort does
    drop_sequences.sort_unstable();

    let mut ix = ((drop_sequences.len() - 1) as f64 * (1.0 - f)) as usize;
    while ix + 1 < drop_sequences.len() && drop_sequences[ix] <= existing {
        ix += 1;
    }

    drop_sequences[ix]
}

pub fn choose_minattribute(
    attribute_values: &mut [f64],
    f: f64,
    existing_attribute: f64,
    descending: bool,
) -> f64 {
    if attribute_values.is_empty() {
        return existing_attribute;
    }

    stable_sort_less(attribute_values);

    if descending {
        let mut ix = ((attribute_values.len() - 1) as f64 * f) as usize;
        while ix > 0 && attribute_values[ix] >= existing_attribute {
            ix -= 1;
        }

        if attribute_values[ix] >= existing_attribute {
            return existing_attribute;
        }

        attribute_values[ix]
    } else {
        let mut ix = ((attribute_values.len() - 1) as f64 * (1.0 - f)).ceil() as usize;
        if ix >= attribute_values.len() {
            ix = attribute_values.len() - 1;
        }
        while ix + 1 < attribute_values.len() && attribute_values[ix] <= existing_attribute {
            ix += 1;
        }

        if attribute_values[ix] <= existing_attribute {
            return existing_attribute;
        }

        attribute_values[ix]
    }
}

pub fn calculate_drop_sequence(feature_minzoom: i8, index: u64) -> u64 {
    // std::max with 0ULL does nothing to an unsigned value
    let zoom: u64 = (feature_minzoom as i64 as u64).min(31);
    let mut out: u64 = zoom << (64 - 5);
    out |= bit_reverse(index) & !(31u64 << (64 - 5));
    !out
}

/// Keeps only a sample of 100K values for choosing thresholds.
pub fn add_sample_to<T: Copy>(vals: &mut Vec<T>, val: T, increment: &mut usize, seq: usize) {
    if seq % *increment == 0 {
        vals.push(val);

        if vals.len() > 100000 {
            let mut tmp = Vec::with_capacity(vals.len() / 2 + 1);
            let mut i = 0;
            while i < vals.len() {
                tmp.push(vals[i]);
                i += 2;
            }

            *increment *= 2;
            *vals = tmp;
        }
    }
}

/// A rectangular placeholder for a polygon that cleaning made disappear.
pub fn revive_polygon(geom: &DrawVec, mut area: f64, z: i32, detail: i32) -> DrawVec {
    let divisor: i64 = 1i64 << (32 - detail - z);
    area /= divisor.wrapping_mul(divisor) as f64;

    if area == 0.0 {
        return DrawVec::new();
    }

    let height = area.sqrt().ceil() as i32;
    let mut width = (area / height as f64).round() as i32;
    if width == 0 {
        width = 1;
    }

    let mut sx: i64 = 0;
    let mut sy: i64 = 0;
    let mut n: i64 = 0;
    for g in geom {
        if g.op == VT_MOVETO || g.op == VT_LINETO {
            sx += g.x;
            sy += g.y;
            n += 1;
        }
    }

    if n > 0 {
        sx /= n;
        sy /= n;

        let width = width as i64;
        let height = height as i64;

        vec![
            Draw::new(VT_MOVETO, sx - (width / 2), sy - (height / 2)),
            Draw::new(VT_LINETO, sx - (width / 2) + width, sy - (height / 2)),
            Draw::new(
                VT_LINETO,
                sx - (width / 2) + width,
                sy - (height / 2) + height,
            ),
            Draw::new(VT_LINETO, sx - (width / 2), sy - (height / 2) + height),
            Draw::new(VT_LINETO, sx - (width / 2), sy - (height / 2)),
        ]
    } else {
        DrawVec::new()
    }
}

/// Makes numbers of any type comparable as doubles, for `--order-by`.
pub fn coerce_double(mut v: MvtValue) -> MvtValue {
    let d = match v.type_ {
        MVT_INT => v.numeric_value.int_value() as f64,
        MVT_UINT => v.numeric_value.uint_value() as f64,
        MVT_SINT => v.numeric_value.sint_value() as f64,
        MVT_FLOAT => v.numeric_value.float_value() as f64,
        _ => return v,
    };
    v.type_ = MVT_DOUBLE;
    v.numeric_value.set_double_value(d);
    v
}

/// Whether the geometry has any line segment of nonzero length.
pub fn draws_something(geom: &DrawVec) -> bool {
    for i in 1..geom.len() {
        if geom[i].op == VT_LINETO && (geom[i].x != geom[i - 1].x || geom[i].y != geom[i - 1].y) {
            return true;
        }
    }

    false
}

/// Runs `f` over `0..n`, split into consecutive ranges on up to `threads`
/// scoped threads when each gets at least `min_len`, and returns the results
/// in order. Tile tasks use this instead of rayon so that a thread waiting for
/// its helpers never picks up another tile's task in the meantime.
pub fn par_ranges<R: Send>(
    n: usize,
    min_len: usize,
    threads: usize,
    f: impl Fn(std::ops::Range<usize>) -> R + Sync,
) -> Vec<R> {
    let parts = (n / min_len.max(1)).clamp(1, threads.max(1));
    if parts <= 1 {
        return vec![f(0..n)];
    }
    let range = |i: usize| n * i / parts..n * (i + 1) / parts;
    let f = &f;
    std::thread::scope(|s| {
        let handles: Vec<_> = (1..parts).map(|i| s.spawn(move || f(range(i)))).collect();
        let mut out = Vec::with_capacity(parts);
        out.push(f(range(0)));
        for h in handles {
            match h.join() {
                Ok(r) => out.push(r),
                Err(e) => std::panic::resume_unwind(e),
            }
        }
        out
    })
}

/// Like `par_ranges`, for changing the items of a slice; `f` gets each part
/// and the index of its first item.
pub fn par_chunks_mut<T: Send>(
    items: &mut [T],
    min_len: usize,
    threads: usize,
    f: impl Fn(usize, &mut [T]) + Sync,
) {
    let n = items.len();
    let parts = (n / min_len.max(1)).clamp(1, threads.max(1));
    if parts <= 1 {
        f(0, items);
        return;
    }
    let size = n.div_ceil(parts);
    let f = &f;
    std::thread::scope(|s| {
        let mut chunks = items.chunks_mut(size).enumerate();
        let (_, first) = chunks.next().unwrap();
        let handles: Vec<_> = chunks
            .map(|(i, c)| s.spawn(move || f(i * size, c)))
            .collect();
        f(0, first);
        for h in handles {
            if let Err(e) = h.join() {
                std::panic::resume_unwind(e);
            }
        }
    });
}
