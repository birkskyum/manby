//! Port of libc++'s `std::sort` (LLVM libc++ 22, `__algorithm/sort.h` and the heap helpers it
//! falls back to), for element types that are not arithmetic and comparators that are not
//! `std::less`, which is the only path `std::sort` takes for Clipper2's `IntersectNode`.
//!
//! `std::sort` is not stable, so elements that compare equal come out in an order that
//! depends on the exact sequence of comparisons and moves. Clipper2 sorts intersection
//! nodes that often share a point, and the order they are processed in reaches the output,
//! so this follows libc++ step by step instead of using `slice::sort_unstable_by`.
//! (libstdc++ uses a different introsort, so the C++ build on Linux can order ties differently.)

// Ensures that *x, *y and *z are ordered according to the comparator c.
// stable, 2-3 compares, 0-2 swaps
fn sort3<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    x: usize,
    y: usize,
    z: usize,
    c: &mut F,
) -> bool {
    if !c(&v[y], &v[x]) {
        // if x <= y
        if !c(&v[z], &v[y]) {
            // if y <= z
            return false; // x <= y && y <= z
        }
        // x <= y && y > z
        v.swap(y, z); // x <= z && y < z
        if c(&v[y], &v[x]) {
            // if x > y
            v.swap(x, y); // x < y && y <= z
        }
        return true; // x <= y && y < z
    }
    if c(&v[z], &v[y]) {
        // x > y, if y > z
        v.swap(x, z); // x < y && y < z
        return true;
    }
    v.swap(x, y); // x > y && y <= z
    // x < y && x <= z
    if c(&v[z], &v[y]) {
        // if y > z
        v.swap(y, z); // x <= y && y < z
    }
    true
} // x <= y && y <= z

// stable, 3-6 compares, 0-5 swaps
fn sort4<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    x1: usize,
    x2: usize,
    x3: usize,
    x4: usize,
    c: &mut F,
) {
    sort3(v, x1, x2, x3, c);
    if c(&v[x4], &v[x3]) {
        v.swap(x3, x4);
        if c(&v[x3], &v[x2]) {
            v.swap(x2, x3);
            if c(&v[x2], &v[x1]) {
                v.swap(x1, x2);
            }
        }
    }
}

// stable, 4-10 compares, 0-9 swaps
#[allow(clippy::too_many_arguments)]
fn sort5<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    x1: usize,
    x2: usize,
    x3: usize,
    x4: usize,
    x5: usize,
    c: &mut F,
) {
    sort4(v, x1, x2, x3, x4, c);
    if c(&v[x5], &v[x4]) {
        v.swap(x4, x5);
        if c(&v[x4], &v[x3]) {
            v.swap(x3, x4);
            if c(&v[x3], &v[x2]) {
                v.swap(x2, x3);
                if c(&v[x2], &v[x1]) {
                    v.swap(x1, x2);
                }
            }
        }
    }
}

// Sort the iterator range [first, last) using the comparator comp using
// the insertion sort algorithm.
fn insertion_sort<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    last: usize,
    comp: &mut F,
) {
    if first == last {
        return;
    }
    let mut i = first + 1;
    while i != last {
        let mut j = i - 1;
        if comp(&v[i], &v[j]) {
            let t = v[i];
            let mut k = j;
            j = i;
            loop {
                v[j] = v[k];
                j = k;
                if j == first {
                    break;
                }
                k -= 1;
                if !comp(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
        }
        i += 1;
    }
}

// Sort the iterator range [first, last) using the comparator comp using
// the insertion sort algorithm.  Insertion sort has two loops, outer and inner.
// The implementation below has no bounds check (unguarded) for the inner loop.
// Assumes that there is an element in the position (first - 1) and that each
// element in the input range is greater or equal to the element at first - 1.
fn insertion_sort_unguarded<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    last: usize,
    comp: &mut F,
) {
    if first == last {
        return;
    }
    let mut i = first + 1;
    while i != last {
        let mut j = i - 1;
        if comp(&v[i], &v[j]) {
            let t = v[i];
            let mut k = j;
            j = i;
            loop {
                v[j] = v[k];
                j = k;
                k -= 1;
                if !comp(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
        }
        i += 1;
    }
}

fn insertion_sort_incomplete<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    last: usize,
    comp: &mut F,
) -> bool {
    match last - first {
        0 | 1 => return true,
        2 => {
            if comp(&v[last - 1], &v[first]) {
                v.swap(first, last - 1);
            }
            return true;
        }
        3 => {
            sort3(v, first, first + 1, last - 1, comp);
            return true;
        }
        4 => {
            sort4(v, first, first + 1, first + 2, last - 1, comp);
            return true;
        }
        5 => {
            sort5(v, first, first + 1, first + 2, first + 3, last - 1, comp);
            return true;
        }
        _ => {}
    }
    let mut j = first + 2;
    sort3(v, first, first + 1, j, comp);
    const LIMIT: u32 = 8;
    let mut count = 0;
    let mut i = j + 1;
    while i != last {
        if comp(&v[i], &v[j]) {
            let t = v[i];
            let mut k = j;
            j = i;
            loop {
                v[j] = v[k];
                j = k;
                if j == first {
                    break;
                }
                k -= 1;
                if !comp(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
            count += 1;
            if count == LIMIT {
                i += 1;
                return i == last;
            }
        }
        j = i;
        i += 1;
    }
    true
}

// Partition [first, last) using the comparator comp.  *first has the
// chosen pivot.  Elements that are equivalent are kept to the right of the
// pivot.  Returns the iterator for the pivot and a bool value which is true if
// the provided range is already sorted, false otherwise.  We assume that the
// length of the range is at least three elements.
fn partition_with_equals_on_right<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    comp: &mut F,
) -> (usize, bool) {
    let begin = first;
    let pivot = v[first];
    // Find the first element greater or equal to the pivot.  It will be always
    // guarded because introsort will do the median-of-three before calling
    // this.
    loop {
        first += 1;
        if !comp(&v[first], &pivot) {
            break;
        }
    }

    // Find the last element less than the pivot.
    if begin == first - 1 {
        while first < last {
            last -= 1;
            if comp(&v[last], &pivot) {
                break;
            }
        }
    } else {
        // Guarded.
        loop {
            last -= 1;
            if comp(&v[last], &pivot) {
                break;
            }
        }
    }

    // If the first element greater than or equal to the pivot is at or after the
    // last element less than the pivot, then we have covered the entire range
    // without swapping elements.  This implies the range is already partitioned.
    let already_partitioned = first >= last;
    // Go through the remaining elements.  Swap pairs of elements (one to the
    // right of the pivot and the other to left of the pivot) that are not on the
    // correct side of the pivot.
    while first < last {
        v.swap(first, last);
        loop {
            first += 1;
            if !comp(&v[first], &pivot) {
                break;
            }
        }
        loop {
            last -= 1;
            if comp(&v[last], &pivot) {
                break;
            }
        }
    }
    // Move the pivot to its correct position.
    let pivot_pos = first - 1;
    if begin != pivot_pos {
        v[begin] = v[pivot_pos];
    }
    v[pivot_pos] = pivot;
    (pivot_pos, already_partitioned)
}

// Similar to the above function.  Elements equivalent to the pivot are put to
// the left of the pivot.  Returns the iterator to the pivot element.
fn partition_with_equals_on_left<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    comp: &mut F,
) -> usize {
    let begin = first;
    let pivot = v[first];
    if comp(&pivot, &v[last - 1]) {
        // Guarded.
        loop {
            first += 1;
            if comp(&pivot, &v[first]) {
                break;
            }
        }
    } else {
        while {
            first += 1;
            first < last && !comp(&pivot, &v[first])
        } {}
    }

    if first < last {
        // It will be always guarded because introsort will do the
        // median-of-three before calling this.
        loop {
            last -= 1;
            if !comp(&pivot, &v[last]) {
                break;
            }
        }
    }
    while first < last {
        v.swap(first, last);
        loop {
            first += 1;
            if comp(&pivot, &v[first]) {
                break;
            }
        }
        loop {
            last -= 1;
            if !comp(&pivot, &v[last]) {
                break;
            }
        }
    }
    let pivot_pos = first - 1;
    if begin != pivot_pos {
        v[begin] = v[pivot_pos];
    }
    v[pivot_pos] = pivot;
    first
}

// __sift_down with __assume_both_children == false (not an arithmetic type)
fn sift_down<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    comp: &mut F,
    len: isize,
    start: isize,
) {
    let at = |i: isize| first + i as usize;
    // left-child of start is at 2 * start + 1
    // right-child of start is at 2 * start + 2
    let mut start = start;
    let mut child = start;

    if len < 2 || (len - 2) / 2 < child {
        return;
    }

    child = 2 * child + 1;

    if (child + 1) < len && comp(&v[at(child)], &v[at(child + 1)]) {
        // right-child exists and is greater than left-child
        child += 1;
    }

    // check if we are in heap-order
    if comp(&v[at(child)], &v[at(start)]) {
        // we are, start is larger than its largest child
        return;
    }

    let top = v[at(start)];
    loop {
        // we are not in heap-order, swap the parent with its largest child
        v[at(start)] = v[at(child)];
        start = child;

        if (len - 2) / 2 < child {
            break;
        }

        // recompute the child based off of the updated parent
        child = 2 * child + 1;

        if (child + 1) < len && comp(&v[at(child)], &v[at(child + 1)]) {
            // right-child exists and is greater than left-child
            child += 1;
        }

        // check if we are in heap-order
        if comp(&v[at(child)], &top) {
            break;
        }
    }
    v[at(start)] = top;
}

fn floyd_sift_down<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    comp: &mut F,
    len: isize,
) -> usize {
    let mut hole = first;
    let mut child_i = first;
    let mut child: isize = 0;

    loop {
        child_i += (child + 1) as usize;
        child = 2 * child + 1;

        if (child + 1) < len && comp(&v[child_i], &v[child_i + 1]) {
            // right-child exists and is greater than left-child
            child_i += 1;
            child += 1;
        }

        // swap hole with its largest child
        v[hole] = v[child_i];
        hole = child_i;

        // if hole is now a leaf, we're done
        if child > (len - 2) / 2 {
            return hole;
        }
    }
}

fn sift_up<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    last: usize,
    comp: &mut F,
    len: isize,
) {
    if len > 1 {
        let mut len = (len - 2) / 2;
        let mut ptr = first + len as usize;
        let mut last = last - 1;

        if comp(&v[ptr], &v[last]) {
            let t = v[last];
            loop {
                v[last] = v[ptr];
                last = ptr;
                if len == 0 {
                    break;
                }
                len = (len - 1) / 2;
                ptr = first + len as usize;
                if !comp(&v[ptr], &t) {
                    break;
                }
            }
            v[last] = t;
        }
    }
}

fn make_heap<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    last: usize,
    comp: &mut F,
) {
    let n = (last - first) as isize;
    let sift_down_n = n;
    if n > 1 {
        // start from the first parent, there is no need to consider children
        let mut start = (sift_down_n - 2) / 2;
        while start >= 0 {
            sift_down(v, first, comp, sift_down_n, start);
            start -= 1;
        }
    }
}

fn pop_heap<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    last: usize,
    comp: &mut F,
    len: isize,
) {
    if len > 1 {
        let top = v[first]; // create a hole at first
        let mut hole = floyd_sift_down(v, first, comp, len);
        let last = last - 1;

        if hole == last {
            v[hole] = top;
        } else {
            v[hole] = v[last];
            hole += 1;
            v[last] = top;
            sift_up(v, first, hole, comp, (hole - first) as isize);
        }
    }
}

fn sort_heap<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    mut last: usize,
    comp: &mut F,
) {
    let mut n = (last - first) as isize;
    while n > 1 {
        pop_heap(v, first, last, comp, n);
        last -= 1;
        n -= 1;
    }
}

// __partial_sort as called from __introsort, with middle == last
fn partial_sort<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    first: usize,
    middle: usize,
    last: usize,
    comp: &mut F,
) {
    if first == middle {
        return;
    }
    make_heap(v, first, middle, comp);
    let len = (middle - first) as isize;
    let mut i = middle;
    while i != last {
        if comp(&v[i], &v[first]) {
            v.swap(i, first);
            sift_down(v, first, comp, len, 0);
        }
        i += 1;
    }
    sort_heap(v, first, middle, comp);
}

// The main sorting function.  Implements introsort combined with other ideas:
//  - option of using block quick sort for partitioning,
//  - guarded and unguarded insertion sort for small lengths,
//  - Tuckey's ninther technique for computing the pivot,
//  - check on whether partition was not required.
// The implementation is partly based on Orson Peters' pattern-defeating
// quicksort, published at: <https://github.com/orlp/pdqsort>.
fn introsort<T: Copy, F: FnMut(&T, &T) -> bool>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    comp: &mut F,
    mut depth: isize,
    mut leftmost: bool,
) {
    // Upper bound for using insertion sort for sorting.
    const LIMIT: usize = 24;
    // Lower bound for using Tuckey's ninther technique for median computation.
    const NINTHER_THRESHOLD: usize = 128;
    loop {
        let len = last - first;
        match len {
            0 | 1 => return,
            2 => {
                if comp(&v[last - 1], &v[first]) {
                    v.swap(first, last - 1);
                }
                return;
            }
            3 => {
                sort3(v, first, first + 1, last - 1, comp);
                return;
            }
            4 => {
                sort4(v, first, first + 1, first + 2, last - 1, comp);
                return;
            }
            5 => {
                sort5(v, first, first + 1, first + 2, first + 3, last - 1, comp);
                return;
            }
            _ => {}
        }
        // Use insertion sort if the length of the range is below the specified limit.
        if len < LIMIT {
            if leftmost {
                insertion_sort(v, first, last, comp);
            } else {
                insertion_sort_unguarded(v, first, last, comp);
            }
            return;
        }
        if depth == 0 {
            // Fallback to heap sort as Introsort suggests.
            partial_sort(v, first, last, last, comp);
            return;
        }
        depth -= 1;
        {
            let half_len = len / 2;
            // Use Tuckey's ninther technique or median of 3 for pivot selection
            // depending on the length of the range being sorted.
            if len > NINTHER_THRESHOLD {
                sort3(v, first, first + half_len, last - 1, comp);
                sort3(v, first + 1, first + (half_len - 1), last - 2, comp);
                sort3(v, first + 2, first + (half_len + 1), last - 3, comp);
                sort3(
                    v,
                    first + (half_len - 1),
                    first + half_len,
                    first + (half_len + 1),
                    comp,
                );
                v.swap(first, first + half_len);
            } else {
                sort3(v, first + half_len, first, last - 1, comp);
            }
        }
        // The elements to the left of the current iterator range are already
        // sorted.  If the current iterator range to be sorted is not the
        // leftmost part of the entire iterator range and the pivot is same as
        // the highest element in the range to the left, then we know that all
        // the elements in the range [first, pivot] would be equal to the pivot,
        // assuming the equal elements are put on the left side when
        // partitioned.  This also means that we do not need to sort the left
        // side of the partition.
        if !leftmost && !comp(&v[first - 1], &v[first]) {
            first = partition_with_equals_on_left(v, first, last, comp);
            continue;
        }
        let (i, already_partitioned) = partition_with_equals_on_right(v, first, last, comp);
        // [first, i) < *i and *i <= [i+1, last)
        // If we were given a perfect partition, see if insertion sort is quick...
        if already_partitioned {
            let fs = insertion_sort_incomplete(v, first, i, comp);
            if insertion_sort_incomplete(v, i + 1, last, comp) {
                if fs {
                    return;
                }
                last = i;
                continue;
            } else if fs {
                first = i + 1;
                continue;
            }
        }
        // Sort the left partiton recursively and the right partition with tail recursion elimination.
        introsort(v, first, i, comp, depth, leftmost);
        leftmost = false;
        first = i + 1;
    }
}

/// `std::sort(v.begin(), v.end(), comp)` as libc++ does it, where `comp(a, b)` is the
/// C++ "less than" predicate.
pub fn sort<T: Copy, F: FnMut(&T, &T) -> bool>(v: &mut [T], mut comp: F) {
    let n = v.len();
    if n == 0 {
        // log(0) is undefined, so don't try computing the depth
        return;
    }
    let depth_limit = 2 * (usize::BITS - 1 - n.leading_zeros()) as isize;
    introsort(v, 0, n, &mut comp, depth_limit, true);
}

#[cfg(test)]
mod tests {
    use super::sort;

    // Expected permutations come from libc++'s std::sort on the same keys, comparing only
    // the key so that ties keep whatever order libc++ leaves them in.
    fn check(keys: &[i32], expected_ids: &[usize]) {
        let mut v: Vec<(i32, usize)> = keys.iter().copied().zip(0..).collect();
        sort(&mut v, |a, b| a.0 < b.0);
        let ids: Vec<usize> = v.iter().map(|p| p.1).collect();
        assert_eq!(ids, expected_ids);
    }

    #[test]
    fn insertion_sort_ties() {
        check(&[1, 0, 1, 0, 1, 0, 1], &[1, 3, 5, 0, 2, 4, 6]);
    }

    #[test]
    fn introsort_ties() {
        let keys: Vec<i32> = (0..30).map(|i| [3, 1, 2, 0][i % 4]).collect();
        check(
            &keys,
            &[
                7, 27, 23, 3, 19, 15, 11, 29, 9, 13, 5, 17, 21, 25, 1, 10, 6, 14, 18, 22, 2, 26, 8,
                12, 16, 4, 20, 24, 28, 0,
            ][..],
        );
    }

    #[test]
    fn sorts() {
        let mut v: Vec<i64> = (0..1000).map(|i| (i * 7919) % 1009).collect();
        sort(&mut v, |a, b| a < b);
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
    }
}
