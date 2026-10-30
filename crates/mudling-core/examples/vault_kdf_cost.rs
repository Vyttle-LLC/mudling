//! Prints what one master-password unlock costs on this machine. Run it in
//! release: a debug build of Argon2 is many times slower.
//!
//!   cargo run --release -p mudling-core --example vault_kdf_cost

use mudling_core::vault::{AutoLock, FILE_NAME, MemoryKeyStore, Vault};
use std::sync::Arc;
use std::time::Instant;

const MASTER: &str = "violet anchor mosaic drifting under quiet lanterns";
const RUNS: u32 = 10;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let folder = tempfile::tempdir()?;
    let path = folder.path().join(FILE_NAME);
    let vault = Vault::open(path, Arc::new(MemoryKeyStore::default()), AutoLock::NEVER);
    let _ = vault.create(MASTER.into()).await?;

    let start = Instant::now();
    for _ in 0..RUNS {
        let _ = vault.unlock_with_password(MASTER.into()).await?;
    }
    println!("unlock with password: {:?} each", start.elapsed() / RUNS);

    let start = Instant::now();
    for _ in 0..RUNS {
        vault.unlock_from_device().await?;
    }
    println!("unlock from device:   {:?} each", start.elapsed() / RUNS);
    Ok(())
}
