//! In-memory token bucket per IP. 5 attempts / 5 minutes. Wraps the login
//! handler only; no other endpoint pays for it. Resets on process restart.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_ATTEMPTS: u32 = 5;
const WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy)]
struct Bucket {
    tokens: u32,
    refilled_at: Instant,
}

#[derive(Default)]
pub struct LoginLimiter {
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl LoginLimiter {
    /// Returns true if the request is allowed; false if the bucket is empty.
    /// Each call consumes one token (no separate "consume" step).
    pub fn check(&self, ip: IpAddr) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: MAX_ATTEMPTS,
            refilled_at: Instant::now(),
        });
        if bucket.refilled_at.elapsed() >= WINDOW {
            bucket.tokens = MAX_ATTEMPTS;
            bucket.refilled_at = Instant::now();
        }
        if bucket.tokens == 0 {
            return false;
        }
        bucket.tokens -= 1;
        true
    }
}
