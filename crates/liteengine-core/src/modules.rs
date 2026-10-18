//! The `.lemodule` module system.
//!
//! Every LiteEngine component beyond the core is a **module**: a
//! zip-compressed `.lemodule` archive containing
//!
//! ```text
//! module.toml    manifest (name, version, engine_api, provides, native features)
//! README.md      human documentation
//! init.lua       optional Lua-side glue loaded when the module enables
//! ```
//!
//! The engine embeds its built-in modules at compile time, so a freshly built
//! `liteengine` binary can run games with no external files at all. Projects
//! can also drop custom `.lemodule` files into `modules/` or install them
//! with `liteengine module add`.
//!
//! Native features (e.g. `"gfx2d.sprites"`, `"rtmode.toggle"`) declare which
//! compiled-in capabilities a module needs; the runtime disables a module with
//! a warning when a feature is unavailable (e.g. `pbr3d` on the legacy path).

use crate::{EngineError, EngineResult, ENGINE_API};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// Parsed `module.toml`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModuleManifest {
    pub module: ModuleMeta,
}

/// `[module]` section of the manifest.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModuleMeta {
    pub name: String,
    pub version: String,
    /// Expected engine API surface (`"1"`).
    #[serde(default = "default_api")]
    pub engine_api: String,
    pub title: String,
    pub description: String,
    /// Lua global this module exposes (informational).
    #[serde(default)]
    pub provides: String,
    /// Compiled-in features this module requires, e.g. `["gfx2d.sprites"]`.
    #[serde(default)]
    pub native: Vec<String>,
}

fn default_api() -> String {
    "1".to_string()
}

/// A module available to the runtime.
#[derive(Debug, Clone)]
pub struct InstalledModule {
    pub manifest: ModuleManifest,
    /// Optional Lua glue from `init.lua` inside the archive.
    pub init_lua: Option<String>,
    pub readme: Option<String>,
    /// Where it came from.
    pub source: ModuleSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleSource {
    /// Embedded in the engine binary at compile time.
    Builtin,
    /// A `.lemodule` file on disk.
    File(PathBuf),
}

/// Loads, enables and disables modules; tracks native feature availability.
pub struct ModuleManager {
    pub installed: Vec<InstalledModule>,
    pub enabled: HashSet<String>,
    /// Native feature keys the runtime supports (e.g. `gfx2d.sprites`).
    pub native_features: HashSet<String>,
    /// Warnings collected during load (missing features, bad manifests...).
    pub warnings: Vec<String>,
}

impl ModuleManager {
    /// Registry of compiled-in modules.
    pub fn builtin_registry() -> Vec<(&'static str, &'static [u8])> {
        vec![
            (
                "engine-core",
                include_bytes!("../builtin/engine-core-1.0.0.lemodule") as &'static [u8],
            ),
            (
                "gfx2d",
                include_bytes!("../builtin/gfx2d-1.0.0.lemodule") as &'static [u8],
            ),
            (
                "gfx3d",
                include_bytes!("../builtin/gfx3d-1.0.0.lemodule") as &'static [u8],
            ),
            (
                "speech",
                include_bytes!("../builtin/speech-1.0.0.lemodule") as &'static [u8],
            ),
            (
                "rtmode",
                include_bytes!("../builtin/rtmode-1.0.0.lemodule") as &'static [u8],
            ),
            (
                "stream",
                include_bytes!("../builtin/stream-1.0.0.lemodule") as &'static [u8],
            ),
            (
                "pbr3d",
                include_bytes!("../builtin/pbr3d-1.0.0.lemodule") as &'static [u8],
            ),
        ]
    }

    pub fn new() -> ModuleResult<ModuleManager> {
        let mut installed = Vec::new();
        let mut warnings = Vec::new();
        for (name, bytes) in Self::builtin_registry() {
            match load_lemodule_bytes(name, bytes) {
                Ok(m) => installed.push(InstalledModule {
                    manifest: m.0,
                    init_lua: m.1,
                    readme: m.2,
                    source: ModuleSource::Builtin,
                }),
                Err(e) => warnings.push(format!("builtin module {name}: {e}")),
            }
        }
        Ok(ModuleManager {
            installed,
            enabled: HashSet::new(),
            native_features: HashSet::new(),
            warnings,
        })
    }

    /// Load every `*.lemodule` found in a project's `modules/` directory.
    pub fn load_project_modules(&mut self, dir: &std::path::Path) -> EngineResult<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()), // no modules dir is fine
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("lemodule") {
                let bytes = std::fs::read(&path)?;
                match load_lemodule_bytes(
                    &path.file_name().unwrap_or_default().to_string_lossy(),
                    &bytes,
                ) {
                    Ok((manifest, init_lua, readme)) => {
                        self.installed.push(InstalledModule {
                            manifest,
                            init_lua,
                            readme,
                            source: ModuleSource::File(path.clone()),
                        });
                    }
                    Err(e) => self
                        .warnings
                        .push(format!("{}: {e}", path.display())),
                }
            }
        }
        Ok(())
    }

    /// Declare a native feature as available (called by the runtime setup).
    pub fn provide_native(&mut self, feature: &str) {
        self.native_features.insert(feature.to_string());
    }

    pub fn find(&self, name: &str) -> Option<&InstalledModule> {
        self.installed.iter().find(|m| m.manifest.module.name == name)
    }

    /// Enable a module, validating its manifest and native features.
    pub fn enable(&mut self, name: &str) -> EngineResult<()> {
        let m = self
            .find(name)
            .ok_or_else(|| {
                EngineError::Config(format!(
                    "module '{name}' not found; try `liteengine module list`"
                ))
            })?
            .clone();
        if m.manifest.module.engine_api != ENGINE_API.to_string() {
            self.warnings.push(format!(
                "module '{}' expects engine_api {} but engine provides {}",
                name, m.manifest.module.engine_api, ENGINE_API
            ));
        }
        for feat in &m.manifest.module.native {
            if !self.native_features.contains(feat) {
                self.warnings.push(format!(
                    "module '{name}' disabled: native feature '{feat}' unavailable on this device/path"
                ));
                return Ok(()); // soft-disable
            }
        }
        self.enabled.insert(name.to_string());
        Ok(())
    }

    pub fn disable(&mut self, name: &str) {
        self.enabled.remove(name);
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    pub fn enabled_sorted(&self) -> Vec<String> {
        let mut v: Vec<String> = self.enabled.iter().cloned().collect();
        v.sort();
        v
    }

    /// Table of all installed modules for `liteengine module list`.
    pub fn table(&self) -> BTreeMap<String, String> {
        self.installed
            .iter()
            .map(|m| {
                (
                    m.manifest.module.name.clone(),
                    format!(
                        "v{} — {} (provides: {})",
                        m.manifest.module.version,
                        m.manifest.module.description,
                        if m.manifest.module.provides.is_empty() {
                            "-"
                        } else {
                            &m.manifest.module.provides
                        }
                    ),
                )
            })
            .collect()
    }
}

pub type ModuleResult<T> = Result<T, EngineError>;

/// Parse a `.lemodule` archive from raw bytes.
pub fn load_lemodule_bytes(
    label: &str,
    bytes: &[u8],
) -> EngineResult<(ModuleManifest, Option<String>, Option<String>)> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor)
        .map_err(|e| EngineError::Config(format!("{label}: not a valid zip module: {e}")))?;

    let manifest_bytes = read_zip(&mut zip, "module.toml")
        .ok_or_else(|| EngineError::Config(format!("{label}: missing module.toml")))?;
    let manifest: ModuleManifest = toml::from_str(&manifest_bytes)
        .map_err(|e| EngineError::Config(format!("{label}: bad module.toml: {e}")))?;

    let init_lua = read_zip(&mut zip, "init.lua");
    let readme = read_zip(&mut zip, "README.md");

    Ok((manifest, init_lua, readme))
}

fn read_zip(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    use std::io::Read;
    f.read_to_string(&mut s).ok()?;
    Some(s)
}
