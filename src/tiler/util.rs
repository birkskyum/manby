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
    if seq.is_multiple_of(*increment) {
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

/// Threads that help with the work inside a tile. They live as long as the
/// tiling, so that their memory is reused from tile to tile. A task that asks
/// for help also works on its own parts, and takes back any part that no
/// helper has started, so it never waits for help that is busy elsewhere,
/// and never picks up another tile's work while it waits.
pub struct Helpers {
    shared: std::sync::Arc<HelperQueue>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

struct HelperQueue {
    queue: std::sync::Mutex<(std::collections::VecDeque<std::sync::Arc<Batch>>, bool)>,
    ready: std::sync::Condvar,
}

/// One call's parts. `job` is only called for a part claimed from `next`,
/// and the caller waits until every part is done, so the job outlives every
/// call of it.
struct Batch {
    job: *const (dyn Fn(usize) + Sync),
    parts: usize,
    next: std::sync::atomic::AtomicUsize,
    done: std::sync::Mutex<usize>,
    finished: std::sync::Condvar,
    panic: std::sync::Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

// SAFETY: `job` points to a `Sync` closure that the caller keeps alive until
// every part has run; `Batch` is otherwise made of thread-safe parts.
unsafe impl Send for Batch {}
// SAFETY: as above.
unsafe impl Sync for Batch {}

impl Batch {
    /// Runs parts until none is left unclaimed.
    fn work(&self) {
        loop {
            let i = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if i >= self.parts {
                return;
            }
            // SAFETY: part i was claimed here, so the caller is still waiting
            // and the job is alive.
            let job = unsafe { &*self.job };
            if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(i))) {
                self.panic.lock().unwrap().get_or_insert(p);
            }
            let mut done = self.done.lock().unwrap();
            *done += 1;
            if *done == self.parts {
                self.finished.notify_all();
            }
        }
    }
}

impl Helpers {
    /// `threads` helpers, which with the asking thread make up to
    /// `threads + 1` working on one tile.
    pub fn new(threads: usize) -> Helpers {
        let shared = std::sync::Arc::new(HelperQueue {
            queue: std::sync::Mutex::new((std::collections::VecDeque::new(), false)),
            ready: std::sync::Condvar::new(),
        });
        let threads = (0..threads)
            .map(|_| {
                let shared = shared.clone();
                std::thread::spawn(move || {
                    loop {
                        let batch = {
                            let mut q = shared.queue.lock().unwrap();
                            loop {
                                if let Some(b) = q.0.pop_front() {
                                    break b;
                                }
                                if q.1 {
                                    return;
                                }
                                q = shared.ready.wait(q).unwrap();
                            }
                        };
                        batch.work();
                    }
                })
            })
            .collect();
        Helpers { shared, threads }
    }

    /// How many threads may work on one call at once.
    pub fn width(&self) -> usize {
        self.threads.len() + 1
    }

    /// Calls `job` for each of `0..parts`, on this thread and on helpers.
    pub fn run(&self, parts: usize, job: &(dyn Fn(usize) + Sync)) {
        if parts <= 1 || self.threads.is_empty() {
            for i in 0..parts {
                job(i);
            }
            return;
        }
        // SAFETY: the lifetime is erased only for the queue; see `Batch`.
        let job: *const (dyn Fn(usize) + Sync) = unsafe { std::mem::transmute(job) };
        let batch = std::sync::Arc::new(Batch {
            job,
            parts,
            next: std::sync::atomic::AtomicUsize::new(0),
            done: std::sync::Mutex::new(0),
            finished: std::sync::Condvar::new(),
            panic: std::sync::Mutex::new(None),
        });
        // one helper woken for each queued entry, not every helper
        let helping = parts.min(self.width()) - 1;
        {
            let mut q = self.shared.queue.lock().unwrap();
            for _ in 0..helping {
                q.0.push_back(batch.clone());
            }
        }
        for _ in 0..helping {
            self.shared.ready.notify_one();
        }
        batch.work();
        let mut done = batch.done.lock().unwrap();
        while *done < parts {
            done = batch.finished.wait(done).unwrap();
        }
        drop(done);
        if let Some(p) = batch.panic.lock().unwrap().take() {
            std::panic::resume_unwind(p);
        }
    }
}

impl Drop for Helpers {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().1 = true;
        self.shared.ready.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Runs `f` over `0..n`, split into consecutive ranges each of at least
/// `min_len`, on this thread and the helpers, and returns the results in order.
pub fn par_ranges<R: Send>(
    helpers: &Helpers,
    n: usize,
    min_len: usize,
    f: impl Fn(std::ops::Range<usize>) -> R + Sync,
) -> Vec<R> {
    let parts = (n / min_len.max(1)).clamp(1, helpers.width());
    if parts <= 1 {
        return vec![f(0..n)];
    }
    let range = |i: usize| n * i / parts..n * (i + 1) / parts;
    let results: Vec<std::sync::Mutex<Option<R>>> =
        (0..parts).map(|_| std::sync::Mutex::new(None)).collect();
    helpers.run(parts, &|i| {
        let r = f(range(i));
        *results[i].lock().unwrap() = Some(r);
    });
    results
        .into_iter()
        .map(|r| r.into_inner().unwrap().unwrap())
        .collect()
}

/// Runs `f` on every item, each item taken by whichever of this thread and
/// the helpers is free next, for work whose cost varies a lot from item to
/// item. Each item is changed by exactly one call.
pub fn par_each_mut<T: Send>(helpers: &Helpers, items: &mut [T], f: impl Fn(usize, &mut T) + Sync) {
    let n = items.len();
    let workers = helpers.width().min(n);
    if workers <= 1 {
        for (i, item) in items.iter_mut().enumerate() {
            f(i, item);
        }
        return;
    }

    struct Items<T>(*mut T);
    // SAFETY: the pointer is only used to reach distinct items, each from one
    // thread, while `items` stays mutably borrowed for the call.
    unsafe impl<T: Send> Sync for Items<T> {}

    let base = Items(items.as_mut_ptr());
    let next = std::sync::atomic::AtomicUsize::new(0);
    helpers.run(workers, &|_| {
        let base = &base;
        loop {
            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if i >= n {
                break;
            }
            // SAFETY: i < n, and fetch_add hands out each index once.
            let item = unsafe { &mut *base.0.add(i) };
            f(i, item);
        }
    });
}
