use std::path::Path;

fn main() {
    if std::env::var_os("CARGO_FEATURE_PORTAL").is_some() {
        let dist = Path::new("../../apps/portal/dist");
        println!("cargo:rerun-if-changed={}", dist.display());
        assert!(
            dist.join("index.html").is_file(),
            "Portal build missing: run `npm ci && npm run build` in apps/portal, or build without the `portal` feature (`--no-default-features`)."
        );
    }
}
