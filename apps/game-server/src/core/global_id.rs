use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use global_id::{GlobalIdGenerator, WorkerLease};

type ItemUidResult = std::io::Result<u64>;

#[derive(Clone)]
pub struct ItemUidGenerator {
    source: Arc<dyn ItemUidSource>,
}

trait ItemUidSource: Send + Sync {
    fn next(&self) -> ItemUidResult;
}

struct GlobalItemUidSource {
    generator: GlobalIdGenerator,
}

impl ItemUidGenerator {
    pub fn from_worker_lease(worker_lease: &WorkerLease) -> std::io::Result<Self> {
        Ok(Self {
            source: Arc::new(GlobalItemUidSource {
                generator: worker_lease
                    .generator()
                    .map_err(|error| std::io::Error::other(error.to_string()))?,
            }),
        })
    }

    pub fn next(&self) -> ItemUidResult {
        self.source.next()
    }
}

impl ItemUidSource for GlobalItemUidSource {
    fn next(&self) -> ItemUidResult {
        self.generator
            .generate()
            .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[cfg(test)]
impl ItemUidGenerator {
    pub fn new_for_test(start: u64) -> Self {
        Self {
            source: Arc::new(TestItemUidSource {
                next_uid: AtomicU64::new(start),
            }),
        }
    }
}

#[cfg(test)]
struct TestItemUidSource {
    next_uid: AtomicU64,
}

#[cfg(test)]
impl ItemUidSource for TestItemUidSource {
    fn next(&self) -> ItemUidResult {
        Ok(self.next_uid.fetch_add(1, Ordering::Relaxed))
    }
}
