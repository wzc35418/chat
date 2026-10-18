uniffi::setup_scaffolding!();

#[uniffi::export]
pub fn dependency_status() -> String {
    "Rust/Kotlin FFI dependencies ready".to_owned()
}

pub fn run_smoke() -> anyhow::Result<String> {
    let mut connection = rusqlite::Connection::open_in_memory()?;
    let transaction = connection.transaction()?;
    transaction.execute_batch("CREATE TABLE dependency_probe (status TEXT NOT NULL);")?;
    transaction.execute(
        "INSERT INTO dependency_probe VALUES (?1)",
        [dependency_status()],
    )?;
    transaction.commit()?;
    let status: String =
        connection.query_row("SELECT status FROM dependency_probe", [], |row| row.get(0))?;
    let _provider = openmls_rust_crypto::OpenMlsRustCrypto::default();
    Ok(serde_json::json!({
        "status": status,
        "sqlite": rusqlite::version(),
        "transport_type": std::any::type_name::<iroh::Endpoint>(),
        "note": "Toolchain smoke check only; no chat protocol implemented"
    })
    .to_string())
}
