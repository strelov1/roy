//! Registry of started upstreams. C4 fills this in — currently a stub so
//! the C1 dispatcher compiles and returns an empty tools list.

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::spec::Bundle;

pub struct Registry;

impl Registry {
    pub async fn start(_bundle: Bundle) -> Result<Self> {
        Ok(Self)
    }

    pub fn tools_list(&self) -> Vec<Value> {
        Vec::new()
    }

    pub async fn call_tool(&self, _params: Value) -> Result<Value> {
        Err(anyhow!("no upstream registered yet"))
    }

    pub async fn shutdown(self) {}
}
