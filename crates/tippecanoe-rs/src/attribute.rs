//! Port of attribute.cpp: accumulating attributes when features are merged.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock};

use crate::errors::{EXIT_ARGS, EXIT_IMPOSSIBLE, EXIT_JSON};
use crate::jsonpull::{JsonPull, JsonType};
use crate::mvt::MvtValue;
use crate::serial::{KeyPool, SerialVal};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttributeOp {
    Sum,
    Product,
    Mean,
    Concat,
    Comma,
    Max,
    Min,
    Count,
}

/// The C++ `numeric_operations` global.
pub static NUMERIC_OPERATIONS: LazyLock<BTreeMap<String, AttributeOp>> = LazyLock::new(|| {
    BTreeMap::from([
        ("sum".to_string(), AttributeOp::Sum),
        ("min".to_string(), AttributeOp::Min),
        ("max".to_string(), AttributeOp::Max),
        ("count".to_string(), AttributeOp::Count),
    ])
});

pub fn set_attribute_accum(
    attribute_accum: &mut HashMap<String, AttributeOp>,
    name: &str,
    type_: &str,
) {
    let t = match type_ {
        "sum" => AttributeOp::Sum,
        "product" => AttributeOp::Product,
        "mean" => AttributeOp::Mean,
        "max" => AttributeOp::Max,
        "min" => AttributeOp::Min,
        "concat" => AttributeOp::Concat,
        "comma" => AttributeOp::Comma,
        "count" => AttributeOp::Count,
        _ => {
            eprintln!(
                "Attribute method ({}) must be sum, product, mean, max, min, concat, comma, or count",
                type_
            );
            std::process::exit(EXIT_ARGS);
        }
    };

    // std::unordered_map::insert leaves an existing entry alone
    attribute_accum.entry(name.to_string()).or_insert(t);
}

/// `set_attribute_accum(attribute_accum, const char *arg, char **argv)`, for a command line argument.
pub fn set_attribute_accum_arg(
    attribute_accum: &mut HashMap<String, AttributeOp>,
    arg: &str,
    argv: &[String],
) {
    let argv0 = argv.first().map(String::as_str).unwrap_or("");

    if arg.starts_with('{') {
        let mut jp = JsonPull::begin_string(arg.as_bytes());
        let Some(o) = jp.read_tree() else {
            eprintln!("{}: -E{}: {}", argv0, arg, jp.error.unwrap_or("(null)"));
            std::process::exit(EXIT_JSON);
        };

        let o = o.root();
        if o.type_() != JsonType::Hash {
            eprintln!("{}: -E{}: not a JSON object", argv0, arg);
            std::process::exit(EXIT_JSON);
        }

        for (i, (key, value)) in o.entries().into_iter().enumerate() {
            if key.type_() != JsonType::String {
                eprintln!("{}: -E{}: key {} not a string", argv0, arg, i);
                std::process::exit(EXIT_JSON);
            }
            if value.type_() != JsonType::String {
                eprintln!("{}: -E{}: value {} not a string", argv0, arg, i);
                std::process::exit(EXIT_JSON);
            }

            set_attribute_accum(attribute_accum, key.string(), value.string());
        }

        return;
    }

    let Some(s) = arg.find(':') else {
        eprintln!("-E{} option must be in the form -Ename:method", arg);
        std::process::exit(EXIT_ARGS);
    };

    let name = &arg[..s];
    let type_ = &arg[s + 1..];

    set_attribute_accum(attribute_accum, name, type_);
}

/// What `preserve_attribute1` needs from its template parameter, which is
/// either `serial_val` or `mvt_value`.
trait AttributeValue: Clone + Default {
    /// The conversion constructor from `double`.
    fn from_double(v: f64) -> Self;
    fn to_double(&self) -> f64;
    fn get_count(&self) -> usize;
    fn set_double_count(&mut self, v: f64, c: usize);
    fn get_string_value(&self) -> String;
    fn set_string_value(&mut self, val: &str);
}

impl AttributeValue for SerialVal {
    fn from_double(v: f64) -> Self {
        SerialVal::from_double(v)
    }
    fn to_double(&self) -> f64 {
        SerialVal::to_double(self)
    }
    fn get_count(&self) -> usize {
        SerialVal::get_count(self)
    }
    fn set_double_count(&mut self, v: f64, c: usize) {
        SerialVal::set_double_count(self, v, c)
    }
    fn get_string_value(&self) -> String {
        SerialVal::get_string_value(self)
    }
    fn set_string_value(&mut self, val: &str) {
        SerialVal::set_string_value(self, val)
    }
}

impl AttributeValue for MvtValue {
    fn from_double(v: f64) -> Self {
        MvtValue::from_double(v)
    }
    fn to_double(&self) -> f64 {
        MvtValue::to_double(self)
    }
    fn get_count(&self) -> usize {
        MvtValue::get_count(self)
    }
    fn set_double_count(&mut self, v: f64, c: usize) {
        MvtValue::set_double_count(self, v, c)
    }
    fn get_string_value(&self) -> String {
        MvtValue::get_string_value(self)
    }
    fn set_string_value(&mut self, val: &str) {
        MvtValue::set_string_value(self, val)
    }
}

fn preserve_attribute1<T: AttributeValue>(
    op: &AttributeOp,
    key: &str,
    val: &T,
    full_keys: &mut Vec<Arc<String>>,
    full_values: &mut Vec<T>,
    key_pool: &mut KeyPool,
) {
    for i in 0..full_keys.len() {
        if key == full_keys[i].as_str() {
            match op {
                AttributeOp::Sum => {
                    full_values[i] = T::from_double(full_values[i].to_double() + val.to_double());
                    return;
                }

                AttributeOp::Product => {
                    full_values[i] = T::from_double(full_values[i].to_double() * val.to_double());
                    return;
                }

                AttributeOp::Max => {
                    let existing = full_values[i].to_double();
                    let maybe = val.to_double();
                    if maybe > existing {
                        full_values[i] = val.clone();
                    }
                    return;
                }

                AttributeOp::Min => {
                    let existing = full_values[i].to_double();
                    let maybe = val.to_double();
                    if maybe < existing {
                        full_values[i] = val.clone();
                    }
                    return;
                }

                AttributeOp::Mean => {
                    let mut count = full_values[i].get_count();
                    if count <= 1 {
                        let v = (full_values[i].to_double() + val.to_double()) / 2.0;
                        full_values[i].set_double_count(v, 2);
                    } else {
                        let sum = full_values[i].to_double() * count as f64 + val.to_double();
                        count += 1;
                        full_values[i].set_double_count(sum / count as f64, count);
                    }
                    return;
                }

                AttributeOp::Concat => {
                    let v = full_values[i].get_string_value() + &val.get_string_value();
                    full_values[i].set_string_value(&v);
                    return;
                }

                AttributeOp::Comma => {
                    let v = full_values[i].get_string_value() + "," + &val.get_string_value();
                    full_values[i].set_string_value(&v);
                    return;
                }

                AttributeOp::Count => {
                    let count = full_values[i].get_count();
                    if count <= 1 {
                        full_values[i].set_double_count(2.0, 2);
                    } else {
                        full_values[i].set_double_count((count + 1) as f64, count + 1);
                    }
                    return;
                }
            }
        }
    }

    // not found, so we are making a new value

    let mut v = T::default();
    match op {
        AttributeOp::Sum | AttributeOp::Max | AttributeOp::Min => {
            v = val.clone();
        }

        AttributeOp::Count => {
            v.set_double_count(1.0, 1);
        }

        AttributeOp::Mean => {
            v.set_double_count(val.to_double(), 1);
        }

        _ => {
            eprintln!("can't happen: operation that isn't used by --accumulate-numeric-attributes");
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }

    full_keys.push(key_pool.pool(key));
    full_values.push(v);
}

/// `preserve_attribute` for `mvt_value`.
pub fn preserve_attribute_mvt(
    op: &AttributeOp,
    key: &str,
    val: &MvtValue,
    full_keys: &mut Vec<Arc<String>>,
    full_values: &mut Vec<MvtValue>,
    key_pool: &mut KeyPool,
) {
    preserve_attribute1(op, key, val, full_keys, full_values, key_pool);
}

/// `preserve_attribute` for `serial_val`.
pub fn preserve_attribute(
    op: &AttributeOp,
    key: &str,
    val: &SerialVal,
    full_keys: &mut Vec<Arc<String>>,
    full_values: &mut Vec<SerialVal>,
    key_pool: &mut KeyPool,
) {
    preserve_attribute1(op, key, val, full_keys, full_values, key_pool);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::{MVT_DOUBLE, MVT_STRING, stringified_to_mvt_value};

    fn show_sv(v: &SerialVal) -> String {
        format!("{}[{}]", v.type_, v.s.replace('\0', "|"))
    }

    fn show_mv(v: &MvtValue) -> String {
        format!("{}[{}]{}", v.type_, v.to_string_value(), v.get_count())
    }

    fn run<T: AttributeValue>(steps: &[(AttributeOp, T)], show: fn(&T) -> String) -> String {
        let mut keys = Vec::new();
        let mut values: Vec<T> = Vec::new();
        let mut kp = KeyPool::default();
        let mut out = Vec::new();
        for (op, v) in steps {
            preserve_attribute1(op, "a", v, &mut keys, &mut values, &mut kp);
            out.push(show(&values[0]));
        }
        assert_eq!(values.len(), 1);
        out.join(" ")
    }

    fn sv(d: f64) -> SerialVal {
        SerialVal::from_double(d)
    }

    fn ss(s: &str) -> SerialVal {
        SerialVal::new(MVT_STRING, s)
    }

    fn mv(s: &str) -> MvtValue {
        stringified_to_mvt_value(MVT_DOUBLE, s)
    }

    fn ms(s: &str) -> MvtValue {
        stringified_to_mvt_value(MVT_STRING, s)
    }

    use AttributeOp::*;

    /// Expected strings are the output of the same steps run through the C++.
    #[test]
    fn serial_val_matches_the_cpp() {
        assert_eq!(
            run(&[(Sum, sv(1.5)), (Sum, sv(2.0)), (Sum, sv(0.1))], show_sv),
            "2[1.5] 2[3.5] 2[3.6]"
        );
        assert_eq!(
            run(
                &[(Sum, sv(1.5)), (Product, sv(2.0)), (Product, sv(-3.0))],
                show_sv
            ),
            "2[1.5] 2[3] 2[-9]"
        );
        assert_eq!(
            run(
                &[
                    (Mean, sv(1.0)),
                    (Mean, sv(2.0)),
                    (Mean, sv(4.0)),
                    (Mean, sv(10.0))
                ],
                show_sv
            ),
            "2[1|1] 2[1.5|2] 2[2.3333333333333337|3] 2[4.25|4]"
        );
        assert_eq!(
            run(&[(Max, sv(1.0)), (Max, sv(5.0)), (Max, sv(3.0))], show_sv),
            "2[1] 2[5] 2[5]"
        );
        assert_eq!(
            run(&[(Min, sv(1.0)), (Min, sv(5.0)), (Min, sv(-3.0))], show_sv),
            "2[1] 2[1] 2[-3]"
        );
        assert_eq!(
            run(
                &[
                    (Count, sv(7.0)),
                    (Count, sv(7.0)),
                    (Count, sv(7.0)),
                    (Count, sv(7.0))
                ],
                show_sv
            ),
            "2[1|1] 2[2|2] 2[3|3] 2[4|4]"
        );
        assert_eq!(
            run(
                &[
                    (Sum, ss("x")),
                    (Concat, ss("y")),
                    (Comma, ss("z")),
                    (Concat, sv(1.5))
                ],
                show_sv
            ),
            "0[x] 0[xy] 0[xy,z] 0[xy,z1.5]"
        );
        assert_eq!(
            run(
                &[(Mean, sv(3.0)), (Mean, sv(4.0)), (Comma, ss("z"))],
                show_sv
            ),
            "2[3|1] 2[3.5|2] 0[3.5|2,z]"
        );
    }

    #[test]
    fn mvt_value_matches_the_cpp() {
        assert_eq!(
            run(
                &[(Sum, mv("1.5")), (Sum, mv("2")), (Sum, mv("0.1"))],
                show_mv
            ),
            "1[1.5]0 2[3.5]0 2[3.6]0"
        );
        assert_eq!(
            run(
                &[(Sum, mv("1.5")), (Product, mv("2")), (Product, mv("-3"))],
                show_mv
            ),
            "1[1.5]0 2[3]0 2[-9]0"
        );
        assert_eq!(
            run(
                &[
                    (Mean, mv("1")),
                    (Mean, mv("2")),
                    (Mean, mv("4")),
                    (Mean, mv("10"))
                ],
                show_mv
            ),
            "2[1]1 2[1.5]2 2[2.3333333333333337]3 2[4.25]4"
        );
        assert_eq!(
            run(&[(Max, mv("1")), (Max, mv("5")), (Max, mv("3"))], show_mv),
            "3[1]0 3[5]0 3[5]0"
        );
        assert_eq!(
            run(&[(Min, mv("1")), (Min, mv("5")), (Min, mv("-3"))], show_mv),
            "3[1]0 3[1]0 5[-3]0"
        );
        assert_eq!(
            run(
                &[(Count, mv("7")), (Count, mv("7")), (Count, mv("7"))],
                show_mv
            ),
            "2[1]1 2[2]2 2[3]3"
        );
        assert_eq!(
            run(
                &[
                    (Sum, ms("x")),
                    (Concat, ms("y")),
                    (Comma, ms("z")),
                    (Concat, mv("1.5"))
                ],
                show_mv
            ),
            "0[x]0 0[xy]0 0[xy,z]0 0[xy,z1.5]0"
        );
        assert_eq!(
            run(
                &[(Mean, mv("3")), (Mean, mv("4")), (Comma, ms("z"))],
                show_mv
            ),
            "2[3]1 2[3.5]2 0[3.5,z]2"
        );
    }

    #[test]
    fn accum_from_name_and_method() {
        let mut accum = HashMap::new();
        set_attribute_accum(&mut accum, "pop", "mean");
        set_attribute_accum(&mut accum, "pop", "sum");
        assert_eq!(accum.get("pop"), Some(&Mean));
        set_attribute_accum_arg(&mut accum, "name:comma", &["tippecanoe".to_string()]);
        assert_eq!(accum.get("name"), Some(&Comma));
        assert_eq!(NUMERIC_OPERATIONS.get("count"), Some(&Count));
        assert_eq!(NUMERIC_OPERATIONS.len(), 4);
    }
}
