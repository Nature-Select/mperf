//! Shared monotonic clock for a session. See docs/abstractions.md §3.

use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
pub struct Clock {
    inner: Arc<Inner>,
}

struct Inner {
    origin: Instant,
}

impl Clock {
    pub fn session_origin() -> Self {
        Self {
            inner: Arc::new(Inner {
                origin: Instant::now(),
            }),
        }
    }

    pub fn now_us(&self) -> i64 {
        self.inner.origin.elapsed().as_micros() as i64
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::session_origin()
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clock")
            .field("elapsed_us", &self.now_us())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn clock_progresses_monotonically() {
        let c = Clock::session_origin();
        let t0 = c.now_us();
        sleep(Duration::from_millis(5));
        let t1 = c.now_us();
        assert!(t1 > t0);
        assert!((t1 - t0) >= 4_000);
    }
}
