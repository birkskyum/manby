//! Reading input files into a `FeatureStore`, in parallel. See DESIGN.md.

use crate::options::Options;
use crate::store::FeatureStore;

/// Reads every input named in `options` into one store, in input order.
pub fn read_inputs(options: &Options) -> crate::Result<FeatureStore> {
    todo!("ingest::read_inputs")
}
