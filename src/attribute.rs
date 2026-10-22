//! Port of attribute.cpp: accumulating attributes when features are merged.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock};

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

pub fn set_attribute_accum(
    attribute_accum: &mut HashMap<String, AttributeOp>,
    name: &str,
    type_: &str,
) {
    todo!("attribute::set_attribute_accum")
}

/// `set_attribute_accum(attribute_accum, const char *arg, char **argv)`, for a command line argument.
pub fn set_attribute_accum_arg(
    attribute_accum: &mut HashMap<String, AttributeOp>,
    arg: &str,
    argv: &[String],
) {
    todo!("attribute::set_attribute_accum_arg")
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
    todo!("attribute::preserve_attribute_mvt")
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
    todo!("attribute::preserve_attribute")
}

/// The C++ `numeric_operations` global.
pub static NUMERIC_OPERATIONS: LazyLock<BTreeMap<String, AttributeOp>> =
    LazyLock::new(|| todo!("attribute::NUMERIC_OPERATIONS"));
