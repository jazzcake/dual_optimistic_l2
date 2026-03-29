// New file — no direct SUI equivalent.
// SUI's Context (sui/consensus/core/src/context.rs) is a heavyweight struct that
// bundles Arc<Committee>, NodeParameters, Arc<Metrics>, and protocol_config.
// This stripped-down version retains only what our simulation needs.

use std::sync::Arc;

use crate::committee::{AuthorityIndex, Committee};

/// Minimal per-node runtime context shared across consensus components.
///
/// Passed as `Arc<Context>` everywhere — cloning is cheap.
#[derive(Clone, Debug)]
pub struct Context {
    /// Index of the local authority within the committee.
    pub own_index: AuthorityIndex,
    /// The epoch committee (immutable for one epoch).
    pub committee: Arc<Committee>,
}

impl Context {
    pub fn new(own_index: AuthorityIndex, committee: Committee) -> Self {
        debug_assert!(
            committee.is_valid_index(own_index),
            "own_index {} is not a valid committee member (size {})",
            own_index.value(),
            committee.size()
        );
        Self {
            own_index,
            committee: Arc::new(committee),
        }
    }

    /// Create a test context for a committee of `n` equal-stake authorities.
    /// Returns `(context_for_node_0, committee_arc)`.
    pub fn new_for_test(n: usize) -> Self {
        let committee = crate::committee::make_test_committee(0, n);
        Self::new(AuthorityIndex::new_for_test(0), committee)
    }
}
