//! Size-only diagnostics. Rust allocation sizes exclude allocator fragmentation
//! and allocations made directly by native libraries such as SQLite and TLS.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::MemorySnapshot;

#[derive(Default)]
struct Gauge {
    live: AtomicUsize,
    peak: AtomicUsize,
}

impl Gauge {
    fn add(&self, bytes: usize) {
        let live = self.live.fetch_add(bytes, Ordering::Relaxed) + bytes;
        self.peak.fetch_max(live, Ordering::Relaxed);
    }

    fn remove(&self, bytes: usize) {
        self.live.fetch_sub(bytes, Ordering::Relaxed);
    }
}

static RUST: Gauge = Gauge {
    live: AtomicUsize::new(0),
    peak: AtomicUsize::new(0),
};
static CONTINUATIONS: Gauge = Gauge {
    live: AtomicUsize::new(0),
    peak: AtomicUsize::new(0),
};
static TURN_COPIES: Gauge = Gauge {
    live: AtomicUsize::new(0),
    peak: AtomicUsize::new(0),
};

thread_local! {
    static MEASURED: Cell<Option<isize>> = const { Cell::new(None) };
}

fn record_change(bytes: isize) {
    // Allocation can also happen during thread teardown when TLS is unavailable.
    let _ = MEASURED.try_with(|measured| {
        if let Some(current) = measured.get() {
            measured.set(Some(current + bytes));
        }
    });
}

pub struct TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            RUST.add(layout.size());
            record_change(layout.size() as isize);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            RUST.add(layout.size());
            record_change(layout.size() as isize);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        RUST.remove(layout.size());
        record_change(-(layout.size() as isize));
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, size) };
        if !replacement.is_null() {
            if size >= layout.size() {
                RUST.add(size - layout.size());
            } else {
                RUST.remove(layout.size() - size);
            }
            record_change(size as isize - layout.size() as isize);
        }
        replacement
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

#[derive(Debug)]
pub struct Reservation {
    bytes: usize,
    continuation: AtomicBool,
}

impl Reservation {
    fn new(bytes: usize, continuation: bool) -> Self {
        let reservation = Self {
            bytes,
            continuation: AtomicBool::new(continuation),
        };
        reservation.gauge().add(bytes);
        reservation
    }

    fn gauge(&self) -> &'static Gauge {
        if self.continuation.load(Ordering::Relaxed) {
            &CONTINUATIONS
        } else {
            &TURN_COPIES
        }
    }

    pub fn is_tracked(&self) -> bool {
        true
    }

    pub fn turn_bytes(bytes: usize) -> Self {
        Self::new(bytes, false)
    }

    #[cfg(test)]
    pub(crate) fn bytes_for_test(&self) -> usize {
        self.bytes
    }

    /// Shared request data becomes retained history without allocating a copy.
    pub fn retain_as_continuation(&self) {
        if !self.continuation.swap(true, Ordering::Relaxed) {
            TURN_COPIES.remove(self.bytes);
            CONTINUATIONS.add(self.bytes);
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.gauge().remove(self.bytes);
    }
}

struct Measurement;

impl Drop for Measurement {
    fn drop(&mut self) {
        MEASURED.with(|measured| measured.set(None));
    }
}

/// Measures allocations retained by a synchronous clone/construction operation.
/// The closure must not release pre-existing allocations or start async work.
pub fn measure<T>(continuation: bool, operation: impl FnOnce() -> T) -> (T, Reservation) {
    MEASURED.with(|measured| {
        assert!(measured.get().is_none(), "nested memory measurement");
        measured.set(Some(0));
    });
    let measurement = Measurement;
    let value = operation();
    let bytes = MEASURED.with(|measured| measured.get().unwrap());
    drop(measurement);
    assert!(bytes >= 0, "measurement released pre-existing allocations");
    (value, Reservation::new(bytes as usize, continuation))
}

pub fn snapshot() -> MemorySnapshot {
    MemorySnapshot {
        rust_live_bytes: RUST.live.load(Ordering::Relaxed),
        rust_peak_bytes: RUST.peak.load(Ordering::Relaxed),
        bridge_continuation_bytes: CONTINUATIONS.live.load(Ordering::Relaxed),
        bridge_continuation_peak_bytes: CONTINUATIONS.peak.load(Ordering::Relaxed),
        bridge_turn_copy_bytes: TURN_COPIES.live.load(Ordering::Relaxed),
        bridge_turn_copy_peak_bytes: TURN_COPIES.peak.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_nested_json_clone_allocations() {
        let value = serde_json::json!({"input": [{"text": "x".repeat(4096)}]});
        let (copy, reservation) = measure(false, || value.clone());
        assert_eq!(copy, value);
        assert!(reservation.bytes >= 4096);
        assert!(reservation.bytes < 16 * 1024);
    }

    #[test]
    fn tracks_reallocation_and_releases_category_ownership() {
        let baseline = snapshot().bridge_continuation_bytes;
        let (bytes, reservation) = measure(true, || {
            let mut bytes = Vec::<u8>::with_capacity(16);
            bytes.resize(4096, 1);
            bytes
        });
        assert_eq!(reservation.bytes, bytes.capacity());
        assert_eq!(
            snapshot().bridge_continuation_bytes,
            baseline + bytes.capacity()
        );
        let reservation = std::sync::Arc::new(reservation);
        let shared = reservation.clone();
        drop(reservation);
        assert_eq!(
            snapshot().bridge_continuation_bytes,
            baseline + bytes.capacity()
        );
        drop(shared);
        assert_eq!(snapshot().bridge_continuation_bytes, baseline);
    }

    #[test]
    fn measurement_is_reset_after_unwind() {
        let _ = std::panic::catch_unwind(|| measure(false, || panic!("test")));
        let (_, reservation) = measure(false, || vec![0u8; 1024]);
        assert_eq!(reservation.bytes, 1024);
    }

    #[test]
    fn transfers_shared_input_accounting_only_once() {
        let baseline = snapshot();
        let reservation = Reservation::turn_bytes(4096);
        assert_eq!(
            snapshot().bridge_turn_copy_bytes,
            baseline.bridge_turn_copy_bytes + 4096
        );
        reservation.retain_as_continuation();
        reservation.retain_as_continuation();
        assert_eq!(
            snapshot().bridge_turn_copy_bytes,
            baseline.bridge_turn_copy_bytes
        );
        assert_eq!(
            snapshot().bridge_continuation_bytes,
            baseline.bridge_continuation_bytes + 4096
        );
        drop(reservation);
        assert_eq!(
            snapshot().bridge_continuation_bytes,
            baseline.bridge_continuation_bytes
        );
    }
}
