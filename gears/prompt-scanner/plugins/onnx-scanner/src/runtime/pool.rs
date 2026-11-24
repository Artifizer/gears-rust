//! Dispatch to any free replica.
//!
//! A caller waits for *a* replica, not for a particular one. The previous
//! round-robin selector picked the next index in turn and then waited on that
//! replica's mutex even when other replicas were idle, so a short input could
//! wait seconds behind a long one while a core sat unused. Measured with four
//! replicas and a mixed payload set, that left ~40% of the CPU idle.
//!
//! The in-flight semaphore in `session.rs` cannot stand in for this: a timed-out
//! inference releases its in-flight permit while its replica is still running,
//! so a permit does not imply a free replica. This pool counts free replicas
//! themselves, and a lease returns its replica only when the work on it ends.
//!
//! Waiters are served in FIFO order (`tokio::sync::Semaphore` is fair), as they
//! were on the per-replica `tokio::sync::Mutex`.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The indices of the replicas nobody is using, and a count of them to wait on.
///
/// Invariant: the semaphore's available permits never exceed `free.len()`. A
/// permit is taken before an index is popped, and an index is pushed back
/// before its permit is released, so a permit holder always finds an index.
#[derive(Debug)]
pub(super) struct ReplicaPool {
    free: Mutex<Vec<usize>>,
    available: Arc<Semaphore>,
}

/// Exclusive use of one replica. Returning it to the pool is `Drop`, so it
/// travels with the work: moved into a blocking task, it is released when that
/// task ends, whether or not anyone still awaits the result.
#[derive(Debug)]
pub(super) struct ReplicaLease {
    index: usize,
    pool: Arc<ReplicaPool>,
    // Dropped after `Drop::drop` has pushed the index back, which is the order
    // the invariant above needs. `None` once the lease is retired.
    permit: Option<OwnedSemaphorePermit>,
}

impl ReplicaPool {
    /// A pool of `replicas` replicas, all free.
    pub(super) fn new(replicas: NonZeroUsize) -> Arc<Self> {
        // Reversed so the first leases are 0, 1, 2… (`pop` takes from the end).
        let free: Vec<usize> = (0..replicas.get()).rev().collect();
        Arc::new(Self {
            free: Mutex::new(free),
            available: Arc::new(Semaphore::new(replicas.get())),
        })
    }

    /// Waits until a replica is free and leases it.
    ///
    /// Cancel-safe: dropping the future while it waits takes nothing. `None`
    /// means the pool was [closed](Self::close) — every replica is gone — or
    /// its invariant is broken; either way no replica is coming.
    pub(super) async fn acquire(self: &Arc<Self>) -> Option<ReplicaLease> {
        let permit = Arc::clone(&self.available).acquire_owned().await.ok()?;
        let index = self.lock_free().pop()?;
        Some(ReplicaLease {
            index,
            pool: Arc::clone(self),
            permit: Some(permit),
        })
    }

    /// Wakes every waiter with `None` and refuses future leases. For when the
    /// last replica has been retired and nobody should wait for one.
    pub(super) fn close(&self) {
        self.available.close();
    }

    /// How many replicas are free right now. A snapshot, for tests and gauges.
    #[cfg(test)]
    fn free_count(&self) -> usize {
        self.lock_free().len()
    }

    /// The free list. Poisoning cannot leave it inconsistent — every critical
    /// section is a single `push` or `pop` — so a poisoned lock is used as is.
    fn lock_free(&self) -> std::sync::MutexGuard<'_, Vec<usize>> {
        self.free.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ReplicaLease {
    /// The leased replica's index, in `0..replicas`.
    pub(super) const fn index(&self) -> usize {
        self.index
    }

    /// Removes this replica from the pool for good: its index is not returned
    /// and its permit is forgotten, so nobody waits on a replica that will
    /// never serve again.
    pub(super) fn retire(mut self) {
        if let Some(permit) = self.permit.take() {
            permit.forget();
        }
    }
}

impl Drop for ReplicaLease {
    fn drop(&mut self) {
        // A retired lease has no permit and must not return its index: the
        // invariant is "permits <= free indices", and both go down together.
        if self.permit.is_some() {
            self.pool.lock_free().push(self.index);
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use super::*;

    fn pool(n: usize) -> Arc<ReplicaPool> {
        ReplicaPool::new(NonZeroUsize::new(n).unwrap_or(NonZeroUsize::MIN))
    }

    async fn lease(pool: &Arc<ReplicaPool>) -> ReplicaLease {
        match pool.acquire().await {
            Some(lease) => lease,
            None => unreachable!("a consistent pool always leases"),
        }
    }

    #[tokio::test]
    async fn leases_distinct_replicas_until_none_is_free() {
        let pool = pool(3);
        let a = lease(&pool).await;
        let b = lease(&pool).await;
        let c = lease(&pool).await;
        let indices: BTreeSet<usize> = [a.index(), b.index(), c.index()].into();
        assert_eq!(indices, BTreeSet::from([0, 1, 2]));
        assert_eq!(pool.free_count(), 0);

        let waiting = tokio::time::timeout(Duration::from_millis(20), pool.acquire()).await;
        assert!(waiting.is_err(), "a fourth caller waits while all are busy");
    }

    /// The regression: a caller must get whichever replica frees up first,
    /// not wait on the one round robin would have picked.
    #[tokio::test]
    async fn a_waiter_gets_whichever_replica_frees_first() {
        let pool = pool(3);
        let a = lease(&pool).await;
        let b = lease(&pool).await;
        let c = lease(&pool).await;
        let freed = b.index();

        let waiter = {
            let pool = Arc::clone(&pool);
            tokio::spawn(async move { lease(&pool).await.index() })
        };
        tokio::task::yield_now().await;
        drop(b);

        let got = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .map(Result::ok);
        assert_eq!(got, Ok(Some(freed)));
        drop((a, c));
    }

    #[tokio::test]
    async fn waiters_are_served_in_arrival_order() {
        let pool = pool(1);
        let held = lease(&pool).await;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut waiters = Vec::new();
        for id in 0..3 {
            let pool = Arc::clone(&pool);
            let tx = tx.clone();
            waiters.push(tokio::spawn(async move {
                let lease = lease(&pool).await;
                assert!(tx.send(id).is_ok(), "the receiver outlives every waiter");
                drop(lease);
            }));
            // Let each waiter enqueue before the next one is spawned.
            tokio::task::yield_now().await;
        }
        drop(tx);
        drop(held);

        let mut order = Vec::new();
        while let Some(id) = rx.recv().await {
            order.push(id);
        }
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn a_dropped_lease_returns_its_replica() {
        let pool = pool(2);
        drop(lease(&pool).await);
        drop(lease(&pool).await);
        assert_eq!(pool.free_count(), 2);
    }

    #[tokio::test]
    async fn a_retired_replica_never_comes_back() {
        let pool = pool(2);
        let a = lease(&pool).await;
        let retired = a.index();
        a.retire();
        assert_eq!(pool.free_count(), 1);

        // Only the other replica is leased from now on, and a second waiter
        // waits rather than receiving the retired index.
        let b = lease(&pool).await;
        assert_ne!(b.index(), retired);
        let waiting = tokio::time::timeout(Duration::from_millis(20), pool.acquire()).await;
        assert!(waiting.is_err(), "no replica is free");
        drop(b);
        let c = lease(&pool).await;
        assert_ne!(c.index(), retired);
    }

    #[tokio::test]
    async fn closing_wakes_waiters_with_none() {
        let pool = pool(1);
        let held = lease(&pool).await;
        let waiter = {
            let pool = Arc::clone(&pool);
            tokio::spawn(async move { pool.acquire().await.is_none() })
        };
        tokio::task::yield_now().await;
        pool.close();
        let woke_empty = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .map(Result::ok);
        assert_eq!(woke_empty, Ok(Some(true)));
        assert!(pool.acquire().await.is_none(), "closed stays closed");
        drop(held);
    }

    #[tokio::test]
    async fn a_cancelled_wait_takes_nothing() {
        let pool = pool(1);
        let held = lease(&pool).await;
        let cancelled = tokio::time::timeout(Duration::from_millis(10), pool.acquire()).await;
        assert!(cancelled.is_err());
        drop(held);
        assert_eq!(pool.free_count(), 1);
        drop(lease(&pool).await);
    }
}
