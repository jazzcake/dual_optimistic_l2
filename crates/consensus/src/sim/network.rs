// New file — no SUI equivalent.
// SimulatedNetwork: a purely in-process, channel-based message bus for test simulations.
// Supports per-pair network partitions for fault-injection tests.

use std::collections::BTreeSet;

use tokio::sync::mpsc;

use crate::types::VerifiedBlock;

// ---------------------------------------------------------------------------
// SimulatedNetwork
// ---------------------------------------------------------------------------

/// In-process message router connecting N simulated consensus nodes.
///
/// `broadcast` delivers a block to every node except the sender, optionally
/// honouring partition rules.  All delivery is synchronous (the caller must
/// drain receiver inboxes after broadcasting).
pub struct SimulatedNetwork {
    n: usize,
    senders: Vec<mpsc::UnboundedSender<VerifiedBlock>>,
    /// Set of (from, to) pairs for which message delivery is blocked.
    partitioned: BTreeSet<(usize, usize)>,
}

impl SimulatedNetwork {
    /// Create a network for `n` nodes.
    ///
    /// Returns the network together with one `UnboundedReceiver<VerifiedBlock>` per node.
    /// The `i`-th receiver belongs to node `i`.
    pub fn new(n: usize) -> (Self, Vec<mpsc::UnboundedReceiver<VerifiedBlock>>) {
        debug_assert!(n > 0, "SimulatedNetwork must have at least one node");

        let mut senders = Vec::with_capacity(n);
        let mut receivers = Vec::with_capacity(n);
        for _ in 0..n {
            let (tx, rx) = mpsc::unbounded_channel();
            senders.push(tx);
            receivers.push(rx);
        }
        (
            Self {
                n,
                senders,
                partitioned: BTreeSet::new(),
            },
            receivers,
        )
    }

    /// Broadcast `block` from node `from` to all other nodes.
    ///
    /// Messages blocked by an active partition are silently dropped.
    pub fn broadcast(&self, from: usize, block: VerifiedBlock) {
        debug_assert!(from < self.n, "sender index {} out of range (n={})", from, self.n);

        for to in 0..self.n {
            if to == from {
                continue;
            }
            if self.is_partitioned(from, to) {
                continue;
            }
            // UnboundedSender::send only fails when the receiver is dropped,
            // which should not happen during a test run.
            let _ = self.senders[to].send(block.clone());
        }
    }

    /// Block all messages between nodes `a` and `b` (bidirectional).
    pub fn partition(&mut self, a: usize, b: usize) {
        debug_assert!(a < self.n && b < self.n, "node index out of range");
        debug_assert_ne!(a, b, "cannot partition a node from itself");
        self.partitioned.insert((a, b));
        self.partitioned.insert((b, a));
    }

    /// Restore connectivity between `a` and `b`.
    pub fn heal(&mut self, a: usize, b: usize) {
        self.partitioned.remove(&(a, b));
        self.partitioned.remove(&(b, a));
    }

    fn is_partitioned(&self, from: usize, to: usize) -> bool {
        self.partitioned.contains(&(from, to))
    }

    /// Drain all pending blocks from `rx` into a `Vec`.
    ///
    /// Provided as a static helper so test code can flush node inboxes without
    /// holding a mutable reference to the network.
    pub fn drain_inbox(rx: &mut mpsc::UnboundedReceiver<VerifiedBlock>) -> Vec<VerifiedBlock> {
        let mut msgs = Vec::new();
        while let Ok(block) = rx.try_recv() {
            msgs.push(block);
        }
        msgs
    }
}
