// New file — no SUI equivalent.
// SimulatedNode: a ConsensusNode wrapped with a deterministic fake clock and
// a seeded pseudo-random number generator (StdRng / ChaCha12 from the `rand`
// workspace dependency).  Used exclusively in test simulations.

use std::sync::Arc;

use rand::{RngCore, SeedableRng};
use rand::rngs::StdRng;
use tokio::sync::{broadcast, mpsc};

use shared::ConsensusEvent;

use crate::{
    committee::AuthorityIndex,
    context::Context,
    node::ConsensusNode,
    types::{BlockRef, Round, TestBlock, VerifiedBlock},
};

// ---------------------------------------------------------------------------
// FakeClock
// ---------------------------------------------------------------------------

/// Monotonically increasing fake clock for deterministic test timestamps.
///
/// Each call to `now_ms` returns a value 1 ms larger than the previous one,
/// starting at 1.  There is no wall-clock dependency.
pub struct FakeClock {
    counter: u64,
}

impl FakeClock {
    pub fn new() -> Self {
        Self { counter: 0 }
    }

    /// Returns the next fake timestamp in milliseconds.
    pub fn now_ms(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }
}

// ---------------------------------------------------------------------------
// SimulatedNode
// ---------------------------------------------------------------------------

/// A `ConsensusNode` augmented with simulation utilities.
///
/// * `rng` — seeded `StdRng` (ChaCha12 internally) for any per-node randomness.
/// * `clock` — monotonic `FakeClock` used for block timestamps.
/// * `inbox` — channel receiver; feed it blocks via `SimulatedNetwork::broadcast`.
pub struct SimulatedNode {
    /// Index of this node in the simulated network.
    pub index: usize,
    /// Seeded PRNG; reproducible across runs with the same `seed`.
    pub rng: StdRng,
    /// Deterministic clock.
    pub clock: FakeClock,
    /// The underlying consensus engine.
    pub node: ConsensusNode,
    /// Broadcast receiver for `ConsensusEvent`s emitted by `node`.
    pub rx: broadcast::Receiver<ConsensusEvent>,
    /// Incoming blocks delivered by `SimulatedNetwork`.
    pub inbox: mpsc::UnboundedReceiver<VerifiedBlock>,
}

impl SimulatedNode {
    /// Create a `SimulatedNode`.
    ///
    /// `seed` is mixed with `index` so that each node in a multi-node
    /// simulation has a distinct but reproducible RNG state.
    pub fn new(
        index: usize,
        seed: u64,
        context: Arc<Context>,
        inbox: mpsc::UnboundedReceiver<VerifiedBlock>,
    ) -> Self {
        debug_assert!(
            context.committee.is_valid_index(AuthorityIndex::new_for_test(index as u32)),
            "node index {} is not a valid committee index (size {})",
            index,
            context.committee.size()
        );

        let (node, rx) = ConsensusNode::new(context);
        Self {
            index,
            rng: StdRng::seed_from_u64(seed.wrapping_add(index as u64)),
            clock: FakeClock::new(),
            node,
            rx,
            inbox,
        }
    }

    // -----------------------------------------------------------------------
    // Block construction
    // -----------------------------------------------------------------------

    /// Build a block at `round` with the given `ancestors`, accept it locally,
    /// and return it (so the caller can broadcast it to other nodes).
    ///
    /// The timestamp is taken from `FakeClock` for full determinism.
    pub fn build_block(&mut self, round: Round, ancestors: Vec<BlockRef>) -> VerifiedBlock {
        debug_assert!(round > 0, "use genesis_blocks() for round 0");
        debug_assert!(
            !ancestors.is_empty(),
            "every non-genesis block must have at least one ancestor"
        );

        let ts = self.clock.now_ms();
        let block = TestBlock::new(round, self.index as u32)
            .set_ancestors(ancestors)
            .set_timestamp_ms(ts)
            .build();

        self.node.accept_block(block.clone());
        block
    }

    /// Generate a deterministic u64 from the node's seeded RNG.
    ///
    /// Can be used by tests to derive random-but-reproducible values
    /// (e.g. shuffled delivery order).
    pub fn next_random(&mut self) -> u64 {
        self.rng.next_u64()
    }

    // -----------------------------------------------------------------------
    // Event / inbox helpers
    // -----------------------------------------------------------------------

    /// Drain all pending `ConsensusEvent`s from the broadcast receiver.
    pub fn drain_events(&mut self) -> Vec<ConsensusEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Process all blocks currently waiting in the network inbox.
    pub fn process_inbox(&mut self) {
        while let Ok(block) = self.inbox.try_recv() {
            self.node.accept_block(block);
        }
    }
}
