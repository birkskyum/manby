//! Port of evaluator.cpp, the feature filter expressions.

use std::collections::{BTreeSet, HashMap};

use crate::jsonpull::{JsonRef, JsonTree};
use crate::mvt::{MvtFeature, MvtLayer};

/// `evaluate` over a map of attribute values.
pub fn evaluate(
    feature: &HashMap<String, crate::mvt::MvtValue>,
    layer: &str,
    filter: JsonRef<'_>,
    exclude_attributes: &mut BTreeSet<String>,
    unidecode_data: &[String],
) -> bool {
    todo!("evaluator::evaluate")
}

/// `evaluate` over a feature of a decoded tile.
pub fn evaluate_feature(
    feat: &MvtFeature,
    layer: &MvtLayer,
    filter: JsonRef<'_>,
    exclude_attributes: &mut BTreeSet<String>,
    z: i32,
    unidecode_data: &[String],
) -> bool {
    todo!("evaluator::evaluate_feature")
}

pub fn parse_filter(s: &str) -> JsonTree {
    todo!("evaluator::parse_filter")
}

pub fn read_filter(fname: &str) -> JsonTree {
    todo!("evaluator::read_filter")
}
