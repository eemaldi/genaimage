//! Regenerate the checked-in OpenAPI contract.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let value = serde_json::to_value(genaimage_server::openapi())?;
    std::fs::write(
        root.join("docs/api/openapi.json"),
        format!("{}\n", serde_json::to_string_pretty(&value)?),
    )?;
    Ok(())
}
