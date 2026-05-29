//! Channel-implementation root.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::bus::BusSender;

pub mod telegram;
pub mod webhook;

#[async_trait]
pub trait Publisher: Send + Sync {
    /// Run until cancelled. Pushes InboundEvents into `bus`.
    async fn run(self: Arc<Self>, bus: BusSender, cancel: CancellationToken) -> Result<()>;
}
