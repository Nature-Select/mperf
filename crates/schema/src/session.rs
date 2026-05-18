//! Session-scoped identity types. See docs/abstractions.md §4.

use crate::device::Platform;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

impl SessionId {
    pub fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DeviceRef {
    pub platform: Platform,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppTarget {
    pub device: DeviceRef,
    pub bundle_id: String,
    pub pid: Option<i32>,
}
