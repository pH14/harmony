//! Regenerate the committed fixture JSON files. Deterministic: rerunning
//! must reproduce the committed bytes exactly (`tests/exact.rs` asserts it).
//!
//! ```sh
//! cargo run --example gen_fixtures
//! ```

use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    std::fs::create_dir_all(&dir).expect("create fixtures dir");
    for (fx, _replay) in [
        differential_lineage::fixtures::tree_lineage(),
        differential_lineage::fixtures::two_pass(),
        differential_lineage::fixtures::retention_properties(),
    ] {
        let path = dir.join(format!("{}.json", fx.name));
        let mut json = serde_json::to_string_pretty(&fx).expect("serialize fixture");
        json.push('\n');
        std::fs::write(&path, json).expect("write fixture");
        println!("wrote {}", path.display());
    }
}
