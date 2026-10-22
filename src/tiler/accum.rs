//! tippecanoe's `preserve_attribute`, for the kept features of a tile being
//! made. tippecanoe keeps an accumulated number as a string, formatting it
//! with `dtoa_milo` after every step and reading it back with `atof`. The
//! formatting reads back as the same double, so the steps can run on the
//! double, and the string is written once, by `settle`, when the tile's
//! features are chosen.

use std::sync::Arc;

use tippecanoe::attribute::AttributeOp;
use tippecanoe::mvt::{MVT_DOUBLE, MVT_STRING};
use tippecanoe::serial::{KeyPool, SerialVal};

/// The number behind one of a feature's full values, while it differs from
/// the value's string.
#[derive(Clone, Copy, Debug)]
pub struct Acc {
    /// Which full value.
    i: u32,
    v: f64,
    /// For a mean or a count, the count that goes after the number.
    count: Option<usize>,
    /// Whether the string is behind.
    dirty: bool,
}

/// `serial_val::to_double`: `atof`, which stops at the NUL before a count.
pub fn to_double(s: &str) -> f64 {
    let s = match s.as_bytes().iter().position(|&b| b == 0) {
        Some(end) => &s[..end],
        None => s,
    };
    s.parse::<f64>()
        .unwrap_or_else(|_| tippecanoe::cstd::atof(s))
}

/// `serial_val::get_count`.
fn get_count(s: &str) -> usize {
    match s.find('\0') {
        None => 0,
        Some(found) => {
            let c = &s[found + 1..];
            c.parse::<i64>()
                .unwrap_or_else(|_| tippecanoe::cstd::atoll(c)) as usize
        }
    }
}

fn find(accs: &[Acc], i: usize) -> Option<usize> {
    accs.iter().position(|a| a.i as usize == i)
}

/// Accumulates `val`, whose `to_double` is `num`, for `key` onto the full
/// keys and values of a kept feature, as `preserve_attribute` does.
#[allow(clippy::too_many_arguments)]
pub fn preserve(
    op: &AttributeOp,
    key: &str,
    type_: i32,
    val: &str,
    num: f64,
    full_keys: &mut Vec<Arc<String>>,
    full_values: &mut Vec<SerialVal>,
    accs: &mut Vec<Acc>,
    key_pool: &mut KeyPool,
) {
    let Some(i) = full_keys.iter().position(|k| k.as_str() == key) else {
        let v = match op {
            AttributeOp::Sum | AttributeOp::Max | AttributeOp::Min => SerialVal::new(type_, val),
            AttributeOp::Count => {
                let mut v = SerialVal::default();
                v.set_double_count(1.0, 1);
                v
            }
            AttributeOp::Mean => {
                let mut v = SerialVal::default();
                v.set_double_count(num, 1);
                v
            }
            _ => {
                eprintln!(
                    "can't happen: operation that isn't used by --accumulate-numeric-attributes"
                );
                std::process::exit(tippecanoe::errors::EXIT_IMPOSSIBLE);
            }
        };
        full_keys.push(key_pool.pool(key));
        full_values.push(v);
        return;
    };

    let a = find(accs, i);
    let existing = match a {
        Some(a) => accs[a].v,
        None => to_double(&full_values[i].s),
    };
    let existing_count = |accs: &[Acc]| match a.and_then(|a| accs[a].count) {
        Some(c) => c,
        None => get_count(&full_values[i].s),
    };
    let (v, count, dirty) = match op {
        AttributeOp::Sum => (existing + num, None, true),
        AttributeOp::Product => (existing * num, None, true),
        AttributeOp::Max | AttributeOp::Min => {
            let maybe = num;
            let better = match op {
                AttributeOp::Max => maybe > existing,
                _ => maybe < existing,
            };
            if !better {
                if a.is_none() {
                    accs.push(Acc {
                        i: i as u32,
                        v: existing,
                        count: None,
                        dirty: false,
                    });
                }
                return;
            }
            full_values[i] = SerialVal::new(type_, val);
            (maybe, None, false)
        }
        AttributeOp::Mean => {
            let count = existing_count(accs);
            if count <= 1 {
                ((existing + num) / 2.0, Some(2), true)
            } else {
                let sum = existing * count as f64 + num;
                (sum / (count + 1) as f64, Some(count + 1), true)
            }
        }
        AttributeOp::Count => {
            let count = existing_count(accs);
            if count <= 1 {
                (2.0, Some(2), true)
            } else {
                ((count + 1) as f64, Some(count + 1), true)
            }
        }
        AttributeOp::Concat | AttributeOp::Comma => {
            let fv = &mut full_values[i];
            fv.type_ = MVT_STRING;
            if matches!(op, AttributeOp::Comma) {
                fv.s.push(',');
            }
            fv.s.push_str(val);
            return;
        }
    };
    let acc = Acc {
        i: i as u32,
        // as the string would read back, which has no negative zero
        v: if dirty { v + 0.0 } else { v },
        count,
        dirty,
    };
    match a {
        Some(a) => accs[a] = acc,
        None => accs.push(acc),
    }
}

/// Writes out the strings of the accumulated numbers.
pub fn settle(full_values: &mut [SerialVal], accs: &mut Vec<Acc>) {
    for a in accs.drain(..) {
        if !a.dirty {
            continue;
        }
        let fv = &mut full_values[a.i as usize];
        match a.count {
            Some(c) => fv.set_double_count(a.v, c),
            None => *fv = SerialVal::from_double(a.v),
        }
        debug_assert_eq!(fv.type_, MVT_DOUBLE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tippecanoe::attribute::preserve_attribute;
    use tippecanoe::mvt::{MVT_INT, MVT_STRING};

    /// The doubles that tippecanoe's numbers go through come back from their
    /// strings unchanged, but for the sign of zero, which is what lets
    /// `preserve` skip the strings.
    #[test]
    fn dtoa_reads_back_as_the_same_double() {
        let mut x: u64 = 0x9E3779B97F4A7C15;
        let check = |v: f64| {
            let s = tippecanoe::milo::dtoa_milo(v);
            let back = tippecanoe::cstd::atof(&s);
            let v = v + 0.0;
            assert!(
                back.to_bits() == v.to_bits() || (v.is_nan() && back.is_nan()),
                "{v} {s}"
            );
        };
        for _ in 0..1_000_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            check(f64::from_bits(x));
            check((x % 1_000_000) as f64 / 1000.0);
            check((x % 1_000_000_000) as f64 * 0.001 + 0.1);
        }
        for v in [
            0.0,
            -0.0,
            1e-310,
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ] {
            check(v);
        }
    }

    /// `preserve` and `settle` give what tippecanoe-rs's `preserve_attribute`
    /// gives, step by step.
    #[test]
    fn matches_preserve_attribute() {
        use AttributeOp::*;
        let vals: Vec<(i32, String)> = (0..200)
            .map(|k| match k % 7 {
                0 => (MVT_STRING, format!("{}", k * 37 % 1000)),
                1 => (MVT_INT, format!("{}", k as i64 - 50)),
                2 => (MVT_STRING, "abc".to_string()),
                3 => (MVT_DOUBLE, format!("{}.{}", k, k % 13)),
                4 => (MVT_DOUBLE, "1e300".to_string()),
                5 => (
                    MVT_STRING,
                    if k % 2 == 0 { " 12x" } else { "-0" }.to_string(),
                ),
                _ => (MVT_DOUBLE, format!("-{}.25", k)),
            })
            .collect();
        for op in [Sum, Product, Mean, Concat, Comma, Max, Min, Count] {
            // these need the value to be there already, from the kept feature
            let seed = match op {
                Concat | Comma => Some(SerialVal::new(MVT_STRING, "x")),
                Product => Some(SerialVal::new(MVT_DOUBLE, "1.5")),
                _ => None,
            };
            let (mut k1, mut v1, mut p1) = (Vec::new(), Vec::new(), KeyPool::default());
            let (mut k2, mut v2, mut p2) = (Vec::new(), Vec::new(), KeyPool::default());
            let mut accs = Vec::new();
            if let Some(seed) = seed {
                k1.push(p1.pool("a"));
                v1.push(seed.clone());
                k2.push(p2.pool("a"));
                v2.push(seed);
            }
            for (n, (t, s)) in vals.iter().enumerate() {
                preserve_attribute(&op, "a", &SerialVal::new(*t, s), &mut k1, &mut v1, &mut p1);
                preserve(
                    &op,
                    "a",
                    *t,
                    s,
                    to_double(s),
                    &mut k2,
                    &mut v2,
                    &mut accs,
                    &mut p2,
                );
                if n % 17 == 0 {
                    settle(&mut v2, &mut accs);
                    assert_eq!(v1, v2, "{op:?} after {n}");
                }
            }
            settle(&mut v2, &mut accs);
            assert_eq!(v1, v2, "{op:?}");
        }
    }
}
