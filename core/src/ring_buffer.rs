use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

const STATE_EMPTY: u8 = 0;
const STATE_WRITING: u8 = 1;
const STATE_READY: u8 = 2;
const STATE_READING: u8 = 3;

/// Runtime statistics for a [`MailboxRingBuffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RingBufferStats {
    pub pushed: u64,
    pub popped: u64,
    pub dropped: u64,
}

struct Slot<T> {
    state: AtomicU8,
    seq: AtomicU64,
    data: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Slot<T> {
    fn empty() -> Self {
        Self {
            state: AtomicU8::new(STATE_EMPTY),
            seq: AtomicU64::new(0),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

/// Lock-free single-producer, single-consumer (SPSC) drop-oldest mailbox ring buffer.
///
/// Allocates `capacity + 1` internal slots so the producer never stalls even while the
/// consumer is simultaneously reading a frame out of its slot, while capping queued
/// unread frames at `capacity`. When full, `push` evicts the oldest unread frame.
pub struct MailboxRingBuffer<T> {
    capacity: usize,
    slots: Box<[Slot<T>]>,
    pushed: AtomicU64,
    published_seq: AtomicU64,
    last_popped_seq: AtomicU64,
    popped: AtomicU64,
    dropped: AtomicU64,
}

// Safety: `T: Send` is sufficient because each initialized slot is exclusively owned
// by either the producer (`STATE_WRITING`) or the consumer (`STATE_READING`), with
// Acquire/Release ordering on `Slot::state`.
unsafe impl<T: Send> Send for MailboxRingBuffer<T> {}
unsafe impl<T: Send> Sync for MailboxRingBuffer<T> {}

impl<T> MailboxRingBuffer<T> {
    /// Creates a new mailbox ring buffer that retains up to `capacity` unread frames.
    ///
    /// # Panics
    /// Panics if `capacity < 1`.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 1, "MailboxRingBuffer capacity must be >= 1");
        let total_slots = capacity + 1;
        let slots = (0..total_slots)
            .map(|_| Slot::empty())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            capacity,
            slots,
            pushed: AtomicU64::new(0),
            published_seq: AtomicU64::new(0),
            last_popped_seq: AtomicU64::new(0),
            popped: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Returns the maximum number of unread frames retained before oldest eviction occurs.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Pushes a frame into the mailbox, overwriting and dropping the oldest unread frame
    /// if `capacity` unread frames are already queued. Returns `true` if an old frame was dropped.
    pub fn push(&self, value: T) -> bool {
        let next_seq = self.pushed.fetch_add(1, Ordering::Relaxed) + 1;
        let mut evicted_old = false;

        loop {
            let mut ready_count = 0usize;
            let mut empty_idx: Option<usize> = None;
            let mut oldest_ready: Option<(usize, u64)> = None;

            for (idx, slot) in self.slots.iter().enumerate() {
                match slot.state.load(Ordering::Acquire) {
                    STATE_EMPTY => {
                        if empty_idx.is_none() {
                            empty_idx = Some(idx);
                        }
                    }
                    STATE_READY => {
                        ready_count += 1;
                        let seq = slot.seq.load(Ordering::Relaxed);
                        if oldest_ready.is_none_or(|(_, min_seq)| seq < min_seq) {
                            oldest_ready = Some((idx, seq));
                        }
                    }
                    _ => {}
                }
            }

            if ready_count < self.capacity {
                if let Some(idx) = empty_idx {
                    if self.slots[idx]
                        .state
                        .compare_exchange(
                            STATE_EMPTY,
                            STATE_WRITING,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        self.write_and_publish(idx, next_seq, value);
                        return evicted_old;
                    }
                    continue;
                }
            }

            if let Some((idx, _)) = oldest_ready {
                if self.slots[idx]
                    .state
                    .compare_exchange(
                        STATE_READY,
                        STATE_WRITING,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    // Drop the evicted unread frame while we hold exclusive WRITING ownership.
                    let old_value = unsafe { (*self.slots[idx].data.get()).assume_init_read() };
                    drop(old_value);
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    evicted_old = true;
                    self.write_and_publish(idx, next_seq, value);
                    return evicted_old;
                }
                continue;
            }

            // Fallback when consumer briefly took the only ready slot; claim any empty slot.
            if let Some(idx) = empty_idx {
                if self.slots[idx]
                    .state
                    .compare_exchange(
                        STATE_EMPTY,
                        STATE_WRITING,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    self.write_and_publish(idx, next_seq, value);
                    return evicted_old;
                }
            }

            std::hint::spin_loop();
        }
    }

    fn write_and_publish(&self, idx: usize, seq: u64, value: T) {
        let slot = &self.slots[idx];
        unsafe {
            (*slot.data.get()).write(value);
        }
        slot.seq.store(seq, Ordering::Relaxed);
        slot.state.store(STATE_READY, Ordering::Release);
        self.published_seq.store(seq, Ordering::Release);
    }

    /// Pops the oldest surviving unread frame in strictly monotonic order, or returns `None` if empty.
    pub fn pop(&self) -> Option<T> {
        loop {
            let mut snapshot_tries = 0u8;
            let oldest_ready = loop {
                let p_before = self.published_seq.load(Ordering::Acquire);
                let mut oldest: Option<(usize, u64)> = None;
                for (idx, slot) in self.slots.iter().enumerate() {
                    if slot.state.load(Ordering::Acquire) == STATE_READY {
                        let seq = slot.seq.load(Ordering::Relaxed);
                        if oldest.is_none_or(|(_, min_seq)| seq < min_seq) {
                            oldest = Some((idx, seq));
                        }
                    }
                }
                let p_after = self.published_seq.load(Ordering::Acquire);
                if p_before == p_after || snapshot_tries >= 4 {
                    break oldest;
                }
                snapshot_tries += 1;
            };

            let (idx, _) = oldest_ready?;
            if self.slots[idx]
                .state
                .compare_exchange(
                    STATE_READY,
                    STATE_READING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                let seq = self.slots[idx].seq.load(Ordering::Relaxed);
                let value = unsafe { (*self.slots[idx].data.get()).assume_init_read() };
                self.slots[idx].state.store(STATE_EMPTY, Ordering::Release);

                let prev = self.last_popped_seq.load(Ordering::Relaxed);
                if seq <= prev {
                    drop(value);
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                self.last_popped_seq.store(seq, Ordering::Relaxed);
                self.popped.fetch_add(1, Ordering::Relaxed);
                return Some(value);
            }
        }
    }

    /// Pops the newest available frame immediately and discards any older queued frames.
    pub fn pop_latest(&self) -> Option<T> {
        let mut latest = self.pop()?;
        while let Some(newer) = self.pop() {
            drop(latest);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            latest = newer;
        }
        Some(latest)
    }

    /// Returns cumulative push, pop, and drop counters.
    pub fn stats(&self) -> RingBufferStats {
        RingBufferStats {
            pushed: self.pushed.load(Ordering::Relaxed),
            popped: self.popped.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

impl<T> Drop for MailboxRingBuffer<T> {
    fn drop(&mut self) {
        for slot in self.slots.iter_mut() {
            if *slot.state.get_mut() == STATE_READY {
                unsafe {
                    (*slot.data.get()).assume_init_drop();
                }
                *slot.state.get_mut() = STATE_EMPTY;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn fifo_order_when_not_full() {
        let ring = MailboxRingBuffer::new(3);
        assert!(!ring.push(10));
        assert!(!ring.push(20));
        assert!(!ring.push(30));

        assert_eq!(ring.pop(), Some(10));
        assert_eq!(ring.pop(), Some(20));
        assert_eq!(ring.pop(), Some(30));
        assert_eq!(ring.pop(), None);
        assert_eq!(
            ring.stats(),
            RingBufferStats {
                pushed: 3,
                popped: 3,
                dropped: 0,
            }
        );
    }

    #[test]
    fn drops_oldest_frames_when_producer_outpaces_consumer() {
        let ring = MailboxRingBuffer::new(2);
        assert!(!ring.push(1));
        assert!(!ring.push(2));
        assert!(ring.push(3)); // evicts 1
        assert!(ring.push(4)); // evicts 2

        assert_eq!(ring.pop(), Some(3));
        assert_eq!(ring.pop(), Some(4));
        assert_eq!(ring.pop(), None);
        assert_eq!(
            ring.stats(),
            RingBufferStats {
                pushed: 4,
                popped: 2,
                dropped: 2,
            }
        );
    }

    #[test]
    fn pop_latest_drains_stale_frames() {
        let ring = MailboxRingBuffer::new(4);
        for i in 1..=4 {
            ring.push(i);
        }
        assert_eq!(ring.pop_latest(), Some(4));
        assert_eq!(ring.pop(), None);
        assert_eq!(ring.stats().dropped, 3);
    }

    #[test]
    fn drops_unconsumed_items_without_leaks() {
        struct DropCounter(Arc<AtomicUsize>);
        impl Drop for DropCounter {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        {
            let ring = MailboxRingBuffer::new(2);
            ring.push(DropCounter(Arc::clone(&drops)));
            ring.push(DropCounter(Arc::clone(&drops)));
            ring.push(DropCounter(Arc::clone(&drops)));
            assert_eq!(drops.load(Ordering::Relaxed), 1);
        }
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn concurrent_spsc_mailbox_stress() {
        let ring = Arc::new(MailboxRingBuffer::new(2));
        let producer_ring = Arc::clone(&ring);

        let producer = thread::spawn(move || {
            for seq in 1u64..=10_000 {
                producer_ring.push(seq);
            }
        });

        let mut last_seen = 0u64;
        while last_seen < 10_000 {
            if let Some(seq) = ring.pop() {
                assert!(
                    seq > last_seen,
                    "frames must remain strictly increasing: {seq} <= {last_seen}"
                );
                last_seen = seq;
            } else {
                thread::yield_now();
            }
        }

        producer.join().expect("producer thread panicked");
        let stats = ring.stats();
        assert_eq!(stats.pushed, 10_000);
        assert_eq!(stats.popped + stats.dropped, 10_000);
    }
}
