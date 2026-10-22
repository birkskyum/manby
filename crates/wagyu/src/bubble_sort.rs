//! Port of `bubble_sort.hpp`.

use crate::config::Result;

pub(crate) fn bubble_sort<T: Copy, C, M>(v: &mut [T], mut c: C, mut m: M) -> Result<()>
where
    C: FnMut(T, T) -> bool,
    M: FnMut(T, T) -> Result<()>,
{
    if v.is_empty() {
        return Ok(());
    }
    let last = v.len() - 1;
    loop {
        let mut modified = false;
        for itr in 0..last {
            let next = itr + 1;
            if !c(v[itr], v[next]) {
                m(v[itr], v[next])?;
                v.swap(itr, next);
                modified = true;
            }
        }
        if !modified {
            break;
        }
    }
    Ok(())
}
