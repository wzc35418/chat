fn main() -> anyhow::Result<()> {
    println!("{}", chat_dependency_check::run_smoke()?);
    Ok(())
}
