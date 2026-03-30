use tokio::sync::mpsc::UnboundedSender;

use shared::BackpressureSignal;

/// Monitors pending-queue depth and emits [`BackpressureSignal`]s via an
/// unbounded channel.
///
/// A hysteresis band between `threshold/2` and `threshold` prevents rapid
/// oscillation between SlowDown and Resume:
/// - `SlowDown` fires only when `depth > threshold` and not yet paused.
/// - `Resume`   fires only when `depth < threshold / 2` and currently paused.
pub struct BackpressureController {
    threshold: usize,
    paused: bool,
    signal_tx: UnboundedSender<BackpressureSignal>,
}

impl BackpressureController {
    /// Create a controller with `threshold` and an output `signal_tx`.
    pub fn new(threshold: usize, signal_tx: UnboundedSender<BackpressureSignal>) -> Self {
        debug_assert!(threshold >= 2, "threshold must be at least 2 to allow hysteresis");
        Self { threshold, paused: false, signal_tx }
    }

    /// Evaluate `depth` against the threshold and emit a signal if a
    /// state transition is needed.
    ///
    /// Idempotent within a state: calling `check(100)` twice only emits
    /// `SlowDown` once.
    pub fn check(&mut self, depth: usize) {
        if depth > self.threshold && !self.paused {
            self.paused = true;
            let _ = self.signal_tx.send(BackpressureSignal::SlowDown);
        } else if depth < self.threshold / 2 && self.paused {
            self.paused = false;
            let _ = self.signal_tx.send(BackpressureSignal::Resume);
        }
    }

    /// Apply an incoming [`BackpressureSignal`] (received from the executor)
    /// to the internal paused state.
    pub fn update(&mut self, signal: BackpressureSignal) {
        match signal {
            BackpressureSignal::SlowDown => self.paused = true,
            BackpressureSignal::Resume => self.paused = false,
        }
    }

    /// Returns `true` when dispatch is currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    #[test]
    fn test_backpressure_triggered() {
        let (tx, mut rx) = unbounded_channel::<BackpressureSignal>();
        let mut bp = BackpressureController::new(2, tx);

        // depth=3 exceeds threshold=2 → should emit SlowDown
        bp.check(3);

        assert_eq!(
            rx.try_recv().unwrap(),
            BackpressureSignal::SlowDown,
            "SlowDown must be emitted when depth exceeds threshold"
        );
        assert!(bp.is_paused(), "controller must be paused after SlowDown");

        // Calling check again while already paused must NOT emit a second signal
        bp.check(5);
        assert!(rx.try_recv().is_err(), "no duplicate SlowDown should be emitted");
    }

    #[test]
    fn test_backpressure_release() {
        let (tx, mut rx) = unbounded_channel::<BackpressureSignal>();
        let mut bp = BackpressureController::new(4, tx);

        // depth=5 > threshold=4 → SlowDown
        bp.check(5);
        rx.try_recv().unwrap(); // consume SlowDown

        // depth=1 < threshold/2=2 → Resume
        bp.check(1);
        assert_eq!(
            rx.try_recv().unwrap(),
            BackpressureSignal::Resume,
            "Resume must be emitted when depth drops below hysteresis floor"
        );
        assert!(!bp.is_paused(), "controller must be unpaused after Resume");
    }
}
