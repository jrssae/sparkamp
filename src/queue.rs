//! Manual play queue (Winamp JTFE-style). A session-only ordered list of
//! playlist-entry ids that the controller drains before normal/shuffle
//! advance. Keyed on `model::Track.id` (stable per session) so it survives
//! reorder and distinguishes duplicate paths. Never persisted.

use std::collections::HashSet;

// Wired into the controller advance seam + frontends in phase-5 tasks 3/5/7/8.
#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub struct Queue {
    /// Queued entry ids in play order. Front is next.
    order: Vec<u64>,
}

#[allow(dead_code)]
impl Queue {
    pub fn new() -> Self {
        Self { order: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Queued ids in play order (Manager render order).
    pub fn ids(&self) -> &[u64] {
        &self.order
    }

    pub fn contains(&self, id: u64) -> bool {
        self.order.contains(&id)
    }

    /// 0-based position of `id` in the queue, or `None`. The user-facing badge
    /// is this value + 1.
    pub fn position_of(&self, id: u64) -> Option<usize> {
        self.order.iter().position(|&x| x == id)
    }

    /// Enqueue if absent, dequeue if present — the `q`-key / context toggle.
    pub fn toggle(&mut self, id: u64) {
        if self.contains(id) {
            self.dequeue(id);
        } else {
            self.enqueue(id);
        }
    }

    /// Append `id` if not already queued (no duplicates — an entry is queued
    /// at most once).
    pub fn enqueue(&mut self, id: u64) {
        if !self.contains(id) {
            self.order.push(id);
        }
    }

    /// Remove `id` from the queue if present.
    pub fn dequeue(&mut self, id: u64) {
        self.order.retain(|&x| x != id);
    }

    /// Remove and return the front id (the entry that plays next), or `None`.
    pub fn pop_next(&mut self) -> Option<u64> {
        if self.order.is_empty() {
            None
        } else {
            Some(self.order.remove(0))
        }
    }

    /// Drop every queued id not present in `live` (playlist removal/clear),
    /// preserving the order of survivors.
    pub fn retain_ids(&mut self, live: &HashSet<u64>) {
        self.order.retain(|id| live.contains(id));
    }

    pub fn clear(&mut self) {
        self.order.clear();
    }

    /// Randomize the queue order (Manager "Randomize"). Membership unchanged.
    pub fn shuffle(&mut self) {
        use rand::seq::SliceRandom;
        self.order.shuffle(&mut rand::thread_rng());
    }

    /// Swap the entry at `idx` with the one above it (no-op at the top or out
    /// of bounds).
    pub fn move_up(&mut self, idx: usize) {
        if idx > 0 && idx < self.order.len() {
            self.order.swap(idx, idx - 1);
        }
    }

    /// Swap the entry at `idx` with the one below it (no-op at the bottom or
    /// out of bounds).
    pub fn move_down(&mut self, idx: usize) {
        if idx + 1 < self.order.len() {
            self.order.swap(idx, idx + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn toggle_enqueues_then_dequeues() {
        let mut q = Queue::new();
        q.toggle(7);
        assert!(q.contains(7));
        assert_eq!(q.position_of(7), Some(0));
        q.toggle(7);
        assert!(!q.contains(7));
        assert!(q.is_empty());
    }

    #[test]
    fn enqueue_is_idempotent_and_ordered() {
        let mut q = Queue::new();
        q.enqueue(3);
        q.enqueue(9);
        q.enqueue(3); // dup ignored
        assert_eq!(q.ids(), &[3, 9]);
        assert_eq!(q.position_of(9), Some(1));
    }

    #[test]
    fn pop_next_drains_front_in_order() {
        let mut q = Queue::new();
        q.enqueue(1);
        q.enqueue(2);
        q.enqueue(3);
        assert_eq!(q.pop_next(), Some(1));
        assert_eq!(q.pop_next(), Some(2));
        assert_eq!(q.ids(), &[3]);
        assert_eq!(q.pop_next(), Some(3));
        assert_eq!(q.pop_next(), None);
    }

    #[test]
    fn retain_ids_drops_dead_entries_keeps_order() {
        let mut q = Queue::new();
        for id in [1, 2, 3, 4] {
            q.enqueue(id);
        }
        let live: HashSet<u64> = [1, 3].into_iter().collect();
        q.retain_ids(&live);
        assert_eq!(q.ids(), &[1, 3]);
    }

    #[test]
    fn move_up_down_bounds_are_noops() {
        let mut q = Queue::new();
        for id in [1, 2, 3] {
            q.enqueue(id);
        }
        q.move_up(0); // no-op
        assert_eq!(q.ids(), &[1, 2, 3]);
        q.move_down(2); // no-op
        assert_eq!(q.ids(), &[1, 2, 3]);
        q.move_up(2); // 3 rises
        assert_eq!(q.ids(), &[1, 3, 2]);
        q.move_down(0); // 1 sinks
        assert_eq!(q.ids(), &[3, 1, 2]);
    }

    #[test]
    fn shuffle_preserves_membership() {
        let mut q = Queue::new();
        for id in 1..=20 {
            q.enqueue(id);
        }
        let before: HashSet<u64> = q.ids().iter().copied().collect();
        q.shuffle();
        let after: HashSet<u64> = q.ids().iter().copied().collect();
        assert_eq!(before, after);
        assert_eq!(q.len(), 20);
    }
}
