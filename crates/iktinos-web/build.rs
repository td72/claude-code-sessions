//! Embeds the page in the binary.
//!
//! The page is built with npm into `web/dist` (`vp build` in `web/`): this
//! script does not run npm, it takes what is there. A checkout without a
//! built page gets the placeholder under `assets/`, so the crate always
//! builds; the package published to crates.io holds the built page.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let dist = manifest.join("web").join("dist");
    let placeholder = manifest.join("assets");
    println!("cargo::rerun-if-changed={}", dist.display());
    println!("cargo::rerun-if-changed={}", placeholder.display());

    // The built page if there is one with an index, else the placeholder.
    let root = if dist.join("index.html").is_file() {
        dist
    } else {
        placeholder
    };
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();

    let mut source = String::from(
        "/// The page's files: the address each is served at, its type, its bytes.\n\
         static ASSETS: &[(&str, &str, &[u8])] = &[\n",
    );
    for (address, path) in &files {
        writeln!(
            source,
            "    ({address:?}, {:?}, include_bytes!({:?})),",
            content_type(path),
            path.display().to_string()
        )
        .expect("writing to a string");
    }
    source.push_str("];\n");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo")).join("assets.rs");
    fs::write(out, source).expect("the build directory can be written");
}

/// Every file under `dir`, with the address it is served at: its path
/// below `root`, with `/` between its parts.
fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
        } else if let Ok(below) = path.strip_prefix(root) {
            let parts: Vec<String> = below
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            files.push((format!("/{}", parts.join("/")), path));
        }
    }
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
