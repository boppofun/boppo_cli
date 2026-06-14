use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

pub const TARGET: &str = "wasm32-wasip1";
pub const DEVICE_ROOT: &str = "/sd/activities/user/wasm";

#[derive(serde::Deserialize)]
struct CargoManifest {
    // Optional so workspace roots (which have no [package]) are silently skipped.
    package: Option<CargoPackage>,
}

#[derive(serde::Deserialize)]
struct CargoPackage {
    name: String,
}

/// Walks up from cwd until it finds a Cargo.toml that contains a \[package\] section.
/// Returns (project_root, package_name).
fn find_project() -> anyhow::Result<(PathBuf, String)> {
    let mut dir = std::env::current_dir().context("could not get current directory")?;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.exists() {
            let content = fs::read_to_string(&manifest)
                .with_context(|| format!("could not read {}", manifest.display()))?;
            let parsed: CargoManifest = toml::from_str(&content)
                .with_context(|| format!("could not parse {}", manifest.display()))?;
            if let Some(pkg) = parsed.package {
                return Ok((dir, pkg.name));
            }
            // Workspace root with no [package] — keep walking up.
        }
        anyhow::ensure!(
            dir.pop(),
            "could not find a Cargo.toml with a [package] section\n\
             Run this command from within a Wasm activity project."
        );
    }
}

pub fn package_name() -> anyhow::Result<String> {
    find_project().map(|(_, name)| name)
}

/// Compiles the activity for wasm32-wasip1 and optionally runs wasm-opt.
/// Returns the package name (used to locate the output binary).
pub fn build(optimize: bool) -> anyhow::Result<String> {
    let (project_dir, name) = find_project()?;
    std::env::set_current_dir(&project_dir)
        .with_context(|| format!("could not change to {}", project_dir.display()))?;

    let status = Command::new("cargo")
        .args(["build", "--package", &name, "--target", TARGET, "--release"])
        .status()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "cargo not found — install Rust from https://rustup.rs\n\
                     Also run: rustup target add {}",
                    TARGET
                )
            } else {
                anyhow::Error::from(e)
            }
        })?;
    anyhow::ensure!(status.success(), "cargo build failed");

    if optimize {
        let wasm = release_path(&name);
        eprintln!("Optimizing {} with wasm-opt...", wasm.display());
        let status = Command::new("wasm-opt")
            .args(["-Oz", "--output"])
            .arg(&wasm)
            .arg(&wasm)
            .status()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    anyhow::anyhow!(
                        "wasm-opt not found\n  \
                         macOS:    brew install binaryen\n  \
                         Ubuntu:   apt install binaryen\n  \
                         Windows:  scoop install binaryen\n  \
                         Releases: https://github.com/WebAssembly/binaryen/releases"
                    )
                } else {
                    anyhow::Error::from(e)
                }
            })?;
        anyhow::ensure!(status.success(), "wasm-opt failed");
    }

    Ok(name)
}

/// Assembles a staging directory at target/deploy/ containing the compiled
/// wasm and all files from assets/. Returns the staging directory path.
pub fn stage(package_name: &str) -> anyhow::Result<PathBuf> {
    // Ensure we operate from the project root even when called without build()
    // (e.g. boppo dev deploy --no-build).
    let (project_dir, _) = find_project()?;
    std::env::set_current_dir(&project_dir)
        .with_context(|| format!("could not change to {}", project_dir.display()))?;

    let staging = PathBuf::from("target").join("deploy");
    if staging.exists() {
        fs::remove_dir_all(&staging).context("failed to clear target/deploy")?;
    }
    fs::create_dir_all(&staging).context("failed to create target/deploy")?;

    let wasm_src = release_path(package_name);
    let wasm_dst = staging.join(format!("{}.wasm", package_name));
    fs::copy(&wasm_src, &wasm_dst)
        .with_context(|| format!("failed to copy {}", wasm_src.display()))?;

    let assets = Path::new("assets");
    if assets.exists() {
        copy_dir(assets, &staging).context("failed to copy assets")?;
    }

    Ok(staging)
}

/// Returns the shell command to launch the activity on the device.
pub fn start_command(package_name: &str) -> String {
    format!("start wasm user/wasm/{pkg}/{pkg}.wasm", pkg = package_name)
}

/// Path to the compiled wasm binary in the local target directory.
/// Cargo normalises hyphens to underscores in binary output names.
pub fn release_path(package_name: &str) -> PathBuf {
    let binary_name = package_name.replace('-', "_");
    PathBuf::from("target")
        .join(TARGET)
        .join("release")
        .join(format!("{}.wasm", binary_name))
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}
