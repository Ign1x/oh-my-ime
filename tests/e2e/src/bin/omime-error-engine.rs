//! Test fixture returning synthetic key data in an engine error description.
use omime_sdk::KeyEvent;
use std::collections::HashMap;

type Dict = HashMap<String, zbus::OwnedVariant>;
type Actions = Vec<(u32, zbus::OwnedVariant)>;

struct Errors;
#[zbus::interface(name = "io.github.ign1x.OhMyIme.Engine1")]
impl Errors {
    fn activate(&self, _context: u64, _info: Dict) -> Actions {
        Vec::new()
    }
    fn deactivate(&self, _context: u64) {}
    fn update_context(&self, _context: u64, _changes: Dict) -> Actions {
        Vec::new()
    }
    fn reset(&self, _context: u64) -> Actions {
        Vec::new()
    }
    fn process_key(
        &self,
        _context: u64,
        key: KeyEvent,
        _extras: Dict,
    ) -> zbus::fdo::Result<(bool, Actions)> {
        Err(zbus::fdo::Error::Failed(format!(
            "PRIVATE_KEY_ARGUMENT:{}",
            key.text
        )))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let address = std::env::var("OMIME_ENGINE_ADDRESS")?;
    let id = std::env::var("OMIME_ENGINE_ID")?;
    let conn = zbus::connection::Builder::address(address.as_str())
        .p2p()
        .serve_at("/io/github/ign1x/OhMyIme/Engine", Errors)
        .build()
        .await?;
    let host = zbus::Proxy::new(
        &conn,
        "io.github.ign1x.OhMyIme",
        "/io/github/ign1x/OhMyIme",
        "io.github.ign1x.OhMyIme.EngineHost1",
    )
    .await?;
    let _: Dict = host
        .call("Register", &(id, 1u16, 0u16, Dict::new()))
        .await?;
    std::future::pending::<()>().await;
    Ok(())
}
