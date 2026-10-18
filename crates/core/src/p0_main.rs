use chat_core::p0::{P0Session, Side};
fn main() -> anyhow::Result<()> {
    let session = P0Session::new()?;
    let result = (|| {
        let alice = session.send(Side::Alice, "P0: 你好，Bob 👋")?;
        let bob = session.send(Side::Bob, "P0: 已通过 MLS 解密，Alice")?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "PASS", "scope": "two ephemeral local devices / real MLS over real iroh",
                "session": session.info()?, "deliveries": [alice, bob]
            }))?
        );
        Ok(())
    })();
    session.close()?;
    result
}
