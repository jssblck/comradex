//! Allocation diagnostics are compiled only with `memory-diagnostics`.
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub rust_live_bytes: usize,
    pub rust_peak_bytes: usize,
    pub bridge_continuation_bytes: usize,
    pub bridge_continuation_peak_bytes: usize,
    pub bridge_turn_copy_bytes: usize,
    pub bridge_turn_copy_peak_bytes: usize,
}

#[cfg(feature = "memory-diagnostics")]
mod enabled;
#[cfg(feature = "memory-diagnostics")]
pub use enabled::{Reservation, measure};

pub fn snapshot() -> Option<MemorySnapshot> {
    #[cfg(feature = "memory-diagnostics")]
    {
        Some(enabled::snapshot())
    }
    #[cfg(not(feature = "memory-diagnostics"))]
    {
        None
    }
}

#[cfg(not(feature = "memory-diagnostics"))]
#[derive(Debug)]
pub struct Reservation;

#[cfg(not(feature = "memory-diagnostics"))]
impl Reservation {
    pub fn turn_bytes(_bytes: usize) -> Self {
        Self
    }
    pub fn retain_as_continuation(&self) {}
    pub fn is_tracked(&self) -> bool {
        false
    }
}

#[cfg(not(feature = "memory-diagnostics"))]
pub fn measure<T>(_continuation: bool, operation: impl FnOnce() -> T) -> (T, Reservation) {
    (operation(), Reservation)
}

#[cfg(all(test, not(feature = "memory-diagnostics")))]
mod tests {
    use super::*;

    #[test]
    fn normal_build_omits_diagnostics_and_preserves_operation() {
        assert!(snapshot().is_none());
        let (value, reservation) = measure(false, || String::from("value"));
        assert_eq!(value, "value");
        assert!(!reservation.is_tracked());
        assert_eq!(std::mem::size_of::<Reservation>(), 0);
    }
}
