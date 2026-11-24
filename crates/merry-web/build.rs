use std::{env, fs, path::PathBuf};

const ASSETS: &[&str] = &[
    "index.html",
    "app.js",
    "trajectory-contract.js",
    "trajectory-contract.generated.js",
    "trajectory-message-model.js",
    "trajectory-timeline.js",
    "trajectory-format.js",
    "trajectory-view.js",
    "app.css",
];

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo provides CARGO_MANIFEST_DIR"),
    );
    let bundled_dist_dir = manifest_dir.join("dist");
    let web_dir = manifest_dir.join("../../web");
    let workspace_dist_dir = web_dir.join("dist");
    let dist_dir = if bundled_dist_dir.is_dir() {
        bundled_dist_dir
    } else {
        workspace_dist_dir
    };
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR"));

    if web_dir.is_dir() {
        for path in [
            "index.html",
            "package.json",
            "package-lock.json",
            "src",
            "scripts",
        ] {
            println!("cargo:rerun-if-changed={}", web_dir.join(path).display());
        }
    }
    for asset in ASSETS {
        let source = dist_dir.join(asset);
        println!("cargo:rerun-if-changed={}", source.display());
        if !source.is_file() {
            panic!(
                "missing Web asset {}. Build the Web app first with `(cd web && npm ci && npm run build)`",
                source.display()
            );
        }
        fs::copy(&source, out_dir.join(asset)).unwrap_or_else(|error| {
            panic!(
                "could not copy Web asset {} to {}: {error}",
                source.display(),
                out_dir.join(asset).display()
            )
        });
    }
}
