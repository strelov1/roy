mod state;

#[allow(unused_imports)]
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Real entrypoint is added in Task 9; this stub just keeps the binary
    // buildable for the intermediate tasks.
    println!("roy-management: stub entrypoint (see Task 9)");
    Ok(())
}
