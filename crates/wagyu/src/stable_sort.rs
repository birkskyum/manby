//! Port of libc++'s `std::stable_sort` (`__algorithm/stable_sort.h` and `__insertion_sort`).
//!
//! PORT: wagyu sorts intersections with `intersect_list_sorter`, which compares y values with
//! a 4 ULP tolerance and so is not a strict weak ordering when three values sit within a few
//! ULPs of each other. Different stable sorts can then disagree, and Rust's `sort_by` may
//! even panic. This copies the libc++ algorithm (insertion sort up to 128 elements for
//! trivially copy-assignable types, 0 otherwise, then merge sort through a full buffer), so
//! the comparator is called in the same order as the C++ build on macOS.
//! The comparator is a C++ style "less than" predicate.

pub(crate) fn stable_sort<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    trivially_copy_assignable: bool,
    mut comp: F,
) {
    // __stable_sort_switch
    let switch = if trivially_copy_assignable { 128 } else { 0 };
    let len = v.len();
    let mut buf: Vec<T> = Vec::new();
    if len > switch {
        buf = v.to_vec();
    }
    stable_sort_impl(v, &mut comp, &mut buf, switch);
}

fn insertion_sort<T: Copy, F: FnMut(&T, &T) -> bool>(v: &mut [T], comp: &mut F) {
    if v.is_empty() {
        return;
    }
    for i in 1..v.len() {
        let mut j = i - 1;
        if comp(&v[i], &v[j]) {
            let t = v[i];
            let mut k = j;
            j = i;
            loop {
                v[j] = v[k];
                j = k;
                if j == 0 {
                    break;
                }
                k -= 1;
                if !comp(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
        }
    }
}

fn insertion_sort_move<T: Copy, F: FnMut(&T, &T) -> bool>(src: &[T], dst: &mut [T], comp: &mut F) {
    if src.is_empty() {
        return;
    }
    dst[0] = src[0];
    let mut last2 = 1;
    for x in &src[1..] {
        let mut j2 = last2;
        let mut i2 = j2 - 1;
        if comp(x, &dst[i2]) {
            dst[j2] = dst[i2];
            j2 -= 1;
            while i2 != 0 {
                i2 -= 1;
                if !comp(x, &dst[i2]) {
                    break;
                }
                dst[j2] = dst[i2];
                j2 -= 1;
            }
            dst[j2] = *x;
        } else {
            dst[j2] = *x;
        }
        last2 += 1;
    }
}

fn merge_move_construct<T: Copy, F: FnMut(&T, &T) -> bool>(
    a: &[T],
    b: &[T],
    out: &mut [T],
    comp: &mut F,
) {
    let (mut i, mut j, mut r) = (0, 0, 0);
    loop {
        if i == a.len() {
            while j != b.len() {
                out[r] = b[j];
                j += 1;
                r += 1;
            }
            return;
        }
        if j == b.len() {
            while i != a.len() {
                out[r] = a[i];
                i += 1;
                r += 1;
            }
            return;
        }
        if comp(&b[j], &a[i]) {
            out[r] = b[j];
            j += 1;
        } else {
            out[r] = a[i];
            i += 1;
        }
        r += 1;
    }
}

fn merge_move_assign<T: Copy, F: FnMut(&T, &T) -> bool>(
    a: &[T],
    b: &[T],
    out: &mut [T],
    comp: &mut F,
) {
    let (mut i, mut j, mut r) = (0, 0, 0);
    while i != a.len() {
        if j == b.len() {
            while i != a.len() {
                out[r] = a[i];
                i += 1;
                r += 1;
            }
            return;
        }
        if comp(&b[j], &a[i]) {
            out[r] = b[j];
            j += 1;
        } else {
            out[r] = a[i];
            i += 1;
        }
        r += 1;
    }
    while j != b.len() {
        out[r] = b[j];
        j += 1;
        r += 1;
    }
}

fn stable_sort_move<T: Copy, F: FnMut(&T, &T) -> bool>(
    src: &mut [T],
    comp: &mut F,
    dst: &mut [T],
    switch: usize,
) {
    let len = src.len();
    match len {
        0 => return,
        1 => {
            dst[0] = src[0];
            return;
        }
        2 => {
            if comp(&src[1], &src[0]) {
                dst[0] = src[1];
                dst[1] = src[0];
            } else {
                dst[0] = src[0];
                dst[1] = src[1];
            }
            return;
        }
        _ => {}
    }
    if len <= 8 {
        insertion_sort_move(src, dst, comp);
        return;
    }
    let l2 = len / 2;
    {
        let (s1, s2) = src.split_at_mut(l2);
        let (d1, d2) = dst.split_at_mut(l2);
        stable_sort_impl(s1, comp, d1, switch);
        stable_sort_impl(s2, comp, d2, switch);
    }
    let (s1, s2) = src.split_at(l2);
    merge_move_construct(s1, s2, dst, comp);
}

fn stable_sort_impl<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    comp: &mut F,
    buff: &mut [T],
    switch: usize,
) {
    let len = v.len();
    match len {
        0 | 1 => return,
        2 => {
            if comp(&v[1], &v[0]) {
                v.swap(0, 1);
            }
            return;
        }
        _ => {}
    }
    if len <= switch {
        insertion_sort(v, comp);
        return;
    }
    let l2 = len / 2;
    // The buffer always covers the range here, so libc++'s `__inplace_merge` fallback
    // (only used when the temporary buffer cannot be allocated) is never reached.
    assert!(len <= buff.len());
    let buff = &mut buff[..len];
    {
        let (v1, v2) = v.split_at_mut(l2);
        let (b1, b2) = buff.split_at_mut(l2);
        stable_sort_move(v1, comp, b1, switch);
        stable_sort_move(v2, comp, b2, switch);
    }
    let (b1, b2) = buff.split_at(l2);
    merge_move_assign(b1, b2, v, comp);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_stably() {
        let mut seed: u64 = 1;
        for len in 0..600 {
            for &tca in &[true, false] {
                let mut v: Vec<(u32, usize)> = (0..len)
                    .map(|i| {
                        seed = seed
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        (((seed >> 33) % 17) as u32, i)
                    })
                    .collect();
                let mut expected = v.clone();
                expected.sort_by_key(|e| e.0);
                stable_sort(&mut v, tca, |a, b| a.0 < b.0);
                assert_eq!(v, expected);
            }
        }
    }
}
