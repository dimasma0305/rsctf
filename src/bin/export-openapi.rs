//! Export without database access or a running server.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "{}",
        serde_json::to_string_pretty(&rsctf::controllers::api_contract::document())?
    );
    Ok(())
}
