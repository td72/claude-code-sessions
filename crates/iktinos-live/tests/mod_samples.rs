//! The bridge mod's tests cannot read files outside the mod, so the event
//! samples are copied into it as `hooks/samples.ts`. This test keeps the copy
//! equal to `tests/fixtures/events/`.
//!
//! After changing a sample, refresh the copy:
//!
//! ```text
//! UPDATE_SAMPLES=1 cargo test -p iktinos-live --test mod_samples
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

const HEADER: &str = "\
// Generated from tests/fixtures/events/*.json. Do not edit: run
//   UPDATE_SAMPLES=1 cargo test -p iktinos-live --test mod_samples
// The samples are the protocol's contract; register.test.ts checks that the
// mod sends exactly these shapes.
export const samples: Record<string, Record<string, unknown>> = ";

#[test]
fn the_mods_copy_of_the_samples_is_current() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut samples = BTreeMap::new();
    for entry in fs::read_dir(root.join("tests/fixtures/events")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            samples.insert(name, value);
        }
    }
    let expected = format!(
        "{HEADER}{}\n",
        serde_json::to_string_pretty(&samples).unwrap()
    );

    let copy = root.join("mods/iktinos-bridge/hooks/samples.ts");
    if std::env::var_os("UPDATE_SAMPLES").is_some() {
        fs::write(&copy, &expected).unwrap();
    }
    let actual = fs::read_to_string(&copy).unwrap_or_default();
    assert!(
        actual == expected,
        "{} is out of date; see this test's header for how to refresh it",
        copy.display()
    );
}
