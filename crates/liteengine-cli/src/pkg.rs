//! `.litepkg` packaging and the self-executable trailer format.
//!
//! A `.litepkg` is a zip archive containing the whole game:
//!
//! ```text
//! liteengine.toml        project manifest
//! main.lua               entry script (as configured by `entry`)
//! assets/...             streamed assets
//! modules/...            custom .lemodule files used by the project
//! ```
//!
//! `liteengine build --packed` appends the package to a copy of the runner
//! binary with an 8-byte magic + 8-byte little-endian length trailer:
//!
//! ```text
//! [runner ELF/MachV/PE bytes][.litepkg bytes]["LEPKG01\0"][len: u64 LE]
//! ```
//!
//! On startup with no subcommand, the runner checks its own file for the
//! trailer and, if present, runs the embedded game entirely from memory.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const TRAILER_MAGIC: &[u8; 8] = b"LEPKG01\x00";

/// Build the `.litepkg` zip in memory from a project directory.
pub fn build_litepkg(project: &Path) -> Result<Vec<u8>, String> {
    let cfg_path = project.join("liteengine.toml");
    if !cfg_path.exists() {
        return Err(format!("no liteengine.toml in {}", project.display()));
    }
    let raw = std::fs::read_to_string(&cfg_path).map_err(|e| e.to_string())?;
    let entry: String = raw
        .lines()
        .find_map(|l| l.trim().strip_prefix("entry"))
        .map(|rest| {
            rest.trim_start_matches(['=', ' ', '"'])
                .trim_end_matches('"')
                .trim()
                .to_string()
        })
        .unwrap_or_else(|| "main.lua".to_string());

    let buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("liteengine.toml", opts)
        .map_err(|e| e.to_string())?;
    zip.write_all(raw.as_bytes()).map_err(|e| e.to_string())?;

    // Entry script.
    let entry_bytes = std::fs::read(project.join(&entry))
        .map_err(|e| format!("entry script: {e}"))?;
    zip.start_file(&entry, opts).map_err(|e| e.to_string())?;
    zip.write_all(&entry_bytes).map_err(|e| e.to_string())?;

    // Additional .lua files (modules of the game).
    for lua in list_files(project, "lua") {
        let rel = lua
            .strip_prefix(project)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .to_string();
        if rel == entry || rel.starts_with("assets/") {
            continue;
        }
        let bytes = std::fs::read(&lua).map_err(|e| e.to_string())?;
        zip.start_file(rel.as_str(), opts).map_err(|e| e.to_string())?;
        zip.write_all(&bytes).map_err(|e| e.to_string())?;
    }

    // assets/ directory tree.
    let assets_dir = project.join("assets");
    if assets_dir.exists() {
        for f in walk(&assets_dir) {
            let rel = f
                .strip_prefix(project)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .to_string();
            let bytes = std::fs::read(&f).map_err(|e| e.to_string())?;
            zip.start_file(rel.as_str(), opts).map_err(|e| e.to_string())?;
            zip.write_all(&bytes).map_err(|e| e.to_string())?;
        }
    }

    // modules/ directory tree (custom .lemodule files).
    let modules_dir = project.join("modules");
    if modules_dir.exists() {
        for f in walk(&modules_dir) {
            let rel = f
                .strip_prefix(project)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .to_string();
            let bytes = std::fs::read(&f).map_err(|e| e.to_string())?;
            zip.start_file(rel.as_str(), opts).map_err(|e| e.to_string())?;
            zip.write_all(&bytes).map_err(|e| e.to_string())?;
        }
    }

    let cursor = zip.finish().map_err(|e| e.to_string())?;
    Ok(cursor.into_inner())
}

/// Recursively list files under `dir`.
pub fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn list_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    walk(dir)
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(ext))
        .collect()
}

/// Append a `.litepkg` to a copy of `runner`, producing a self-executable.
pub fn append_trailer(
    runner: &Path,
    pkg: &[u8],
    out: &Path,
) -> Result<u64, String> {
    let mut exe = std::fs::read(runner).map_err(|e| e.to_string())?;
    let exe_len = exe.len() as u64;
    // Trailer layout (see module docs): [pkg][len: u64 LE][magic: 8 bytes]
    exe.extend_from_slice(pkg);
    exe.extend_from_slice(&(pkg.len() as u64).to_le_bytes());
    exe.extend_from_slice(TRAILER_MAGIC);
    std::fs::write(out, &exe).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755));
    }
    Ok(exe_len)
}

/// Extract an embedded `.litepkg` from a binary's own bytes, if present.
pub fn extract_trailer(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 16 {
        return None;
    }
    let end = bytes.len();
    let magic = &bytes[end - 8..end];
    if magic != TRAILER_MAGIC {
        return None;
    }
    let len = u64::from_le_bytes(bytes[end - 16..end - 8].try_into().ok()?) as usize;
    let start = end - 16 - len;
    if start < 0 || start >= end {
        return None;
    }
    Some(bytes[start..end - 16].to_vec())
}

/// Read the current executable's bytes (self, for trailer detection).
pub fn self_bytes() -> Option<Vec<u8>> {
    let p = std::env::current_exe().ok()?;
    std::fs::read(p).ok()
}

/// Info.plist template for `--macos` bundles.
pub fn info_plist(title: &str, exe_name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>{title}</string>
    <key>CFBundleDisplayName</key><string>{title}</string>
    <key>CFBundleExecutable</key><string>{exe_name}</string>
    <key>CFBundleIdentifier</key><string>io.liteengine.game.{exe_name}</string>
    <key>CFBundleVersion</key><string>1.0.0</string>
    <key>CFBundleShortVersionString</key><string>1.0.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>LSMinimumSystemVersion</key><string>10.8</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#
    )
}
