//! Locking that survives a poisoned mutex.

use std::sync::{Mutex, MutexGuard};

/// Lock `m`, taking the data back even if a previous holder panicked.
///
/// `Mutex::lock().unwrap()` turns one panic into every later caller panicking,
/// which is the wrong trade in two places this codebase has both of.
///
/// Across the C boundary it is worse than a wrong trade. A panic in an
/// `extern "C"` function aborts the process, so a rip worker that fails while
/// holding a status lock would take the whole macOS app down at the next poll
/// rather than reporting the failure it was holding the lock to record.
///
/// Poisoning means "someone panicked", not "this data is unusable". The state
/// behind these locks is progress counters and cached entries, where carrying
/// on with what is there beats refusing to answer.
pub(crate) fn lock_or_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A panic while the lock is held does not spread to the next caller, and
    /// the value written before the panic is still there.
    #[test]
    fn a_poisoned_lock_still_hands_back_its_value() {
        let m = Arc::new(Mutex::new(0u32));
        let m2 = m.clone();
        let panicked = std::thread::spawn(move || {
            let mut g = m2.lock().unwrap();
            *g = 7;
            panic!("the holder fails");
        })
        .join();
        assert!(panicked.is_err(), "the thread was supposed to panic");
        assert!(m.lock().is_err(), "and that was supposed to poison the lock");

        assert_eq!(*lock_or_recover(&m), 7, "the write before the panic survives");
        *lock_or_recover(&m) += 1;
        assert_eq!(*lock_or_recover(&m), 8, "and it stays usable");
    }
}
