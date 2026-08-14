use std::collections::{HashSet, VecDeque};

use pseudomux_protocol::v1::{SessionGenerationId, SessionId};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ClosedGeneration {
    session_id: SessionId,
    generation_id: SessionGenerationId,
}

/// Recent successfully closed sessions retained only to make Close idempotent.
///
/// This is deliberately bounded: result/event idempotency lives in a live
/// session actor, while a daemon-lifetime close tombstone is only a convenience
/// and must not turn RunOnce traffic into unbounded resident memory.
pub(crate) struct ClosedSessionTombstones {
    capacity: usize,
    order: VecDeque<ClosedGeneration>,
    ids: HashSet<ClosedGeneration>,
}

impl ClosedSessionTombstones {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: VecDeque::with_capacity(capacity.max(1)),
            ids: HashSet::with_capacity(capacity.max(1)),
        }
    }

    pub(crate) fn contains(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
    ) -> bool {
        self.ids.contains(&ClosedGeneration {
            session_id,
            generation_id,
        })
    }

    pub(crate) fn insert(&mut self, session_id: SessionId, generation_id: SessionGenerationId) {
        let generation = ClosedGeneration {
            session_id,
            generation_id,
        };
        if !self.ids.insert(generation) {
            return;
        }
        self.order.push_back(generation);
        while self.order.len() > self.capacity {
            if let Some(expired) = self.order.pop_front() {
                self.ids.remove(&expired);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_is_idempotent_and_evicts_oldest() {
        let first = SessionId::from_u128(1);
        let second = SessionId::from_u128(2);
        let third = SessionId::from_u128(3);
        let generation = SessionGenerationId::from_u128(10);
        let mut tombstones = ClosedSessionTombstones::new(2);
        tombstones.insert(first, generation);
        tombstones.insert(first, generation);
        tombstones.insert(second, generation);
        tombstones.insert(third, generation);
        assert!(!tombstones.contains(first, generation));
        assert!(tombstones.contains(second, generation));
        assert!(tombstones.contains(third, generation));
    }

    #[test]
    fn generations_of_the_same_claude_session_are_distinct() {
        let session_id = SessionId::from_u128(1);
        let generation_a = SessionGenerationId::from_u128(10);
        let generation_b = SessionGenerationId::from_u128(11);
        let mut tombstones = ClosedSessionTombstones::new(2);
        tombstones.insert(session_id, generation_a);
        assert!(tombstones.contains(session_id, generation_a));
        assert!(!tombstones.contains(session_id, generation_b));
        tombstones.insert(session_id, generation_b);
        assert!(tombstones.contains(session_id, generation_a));
        assert!(tombstones.contains(session_id, generation_b));
    }
}
