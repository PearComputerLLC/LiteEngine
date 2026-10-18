//! Project configuration (`liteengine.toml`) and project discovery.
//!
//! A LiteEngine project is a directory (or a `.litepkg` bundle) containing:
//!
//! ```text
//! liteengine.toml     project manifest (this module)
//! main.lua            entry script
//! assets/             streamed textures and data
//! modules/            optional custom .lemodule files
//! ```

use crate::{EngineError, EngineResult};
use std::path::{Path, PathBuf};

/// `[graphics.rt]` section.
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
pub struct RtConfig {
    pub quality: String,
    pub scale: f32,
    pub bounces: u32,
    pub denoise: bool,
}

/// `[assets]` section.
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
pub struct AssetConfig {
    /// Resident texture budget in MiB (LRU eviction above this).
    pub budget_mb: u64,
    /// Paths to prefetch at startup.
    pub preload: Vec<String>,
}

/// `[modules]` section.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct ModuleList {
    pub enabled: Vec<String>,
}

/// The parsed `liteengine.toml`, flattened for convenient access.
///
/// The on-disk layout has sections:
///
/// ```toml
/// [game]      title, entry, width, height, fps_cap, background
/// [graphics]  raytracing, window_scale, [graphics.rt] quality/scale/bounces/denoise
/// [modules]   enabled = [...]
/// [assets]    budget_mb, preload = [...]
/// ```
#[derive(Debug, Clone)]
pub struct ProjectConfig {
    pub title: String,
    pub entry: String,
    pub width: u32,
    pub height: u32,
    pub fps_cap: u32,
    pub background: String,
    pub raytracing: bool,
    pub window_scale: f32,
    pub rt: RtConfig,
    pub modules: ModuleList,
    pub assets: AssetConfig,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        ProjectConfig {
            title: "LiteEngine Project".into(),
            entry: "main.lua".into(),
            width: 960,
            height: 540,
            fps_cap: 60,
            background: "#1b2a41".into(),
            raytracing: false,
            window_scale: 1.0,
            rt: RtConfig {
                quality: "medium".into(),
                scale: 0.0,
                bounces: 2,
                denoise: true,
            },
            modules: ModuleList {
                enabled: vec![
                    "engine-core".into(),
                    "gfx2d".into(),
                    "speech".into(),
                    "stream".into(),
                ],
            },
            assets: AssetConfig {
                budget_mb: 256,
                preload: Vec::new(),
            },
        }
    }
}

/// Raw on-disk sections.
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
struct RawProject {
    game: RawGame,
    graphics: RawGraphics,
    modules: ModuleList,
    assets: AssetConfig,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
struct RawGame {
    title: String,
    entry: String,
    width: u32,
    height: u32,
    fps_cap: u32,
    background: String,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
struct RawGraphics {
    raytracing: bool,
    window_scale: f32,
    rt: RtConfig,
}

impl<'de> serde::Deserialize<'de> for ProjectConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawProject::deserialize(deserializer)?;
        let d = ProjectConfig::default();
        Ok(ProjectConfig {
            title: if raw.game.title.is_empty() {
                d.title
            } else {
                raw.game.title
            },
            entry: if raw.game.entry.is_empty() {
                d.entry
            } else {
                raw.game.entry
            },
            width: if raw.game.width == 0 {
                d.width
            } else {
                raw.game.width
            },
            height: if raw.game.height == 0 {
                d.height
            } else {
                raw.game.height
            },
            fps_cap: if raw.game.fps_cap == 0 {
                d.fps_cap
            } else {
                raw.game.fps_cap
            },
            background: if raw.game.background.is_empty() {
                d.background
            } else {
                raw.game.background
            },
            raytracing: raw.graphics.raytracing,
            window_scale: if raw.graphics.window_scale > 0.0 {
                raw.graphics.window_scale
            } else {
                d.window_scale
            },
            rt: RtConfig {
                quality: if raw.graphics.rt.quality.is_empty() {
                    d.rt.quality
                } else {
                    raw.graphics.rt.quality
                },
                scale: raw.graphics.rt.scale,
                bounces: raw.graphics.rt.bounces,
                denoise: raw.graphics.rt.denoise,
            },
            modules: ModuleList {
                enabled: if raw.modules.enabled.is_empty() {
                    d.modules.enabled
                } else {
                    raw.modules.enabled
                },
            },
            assets: AssetConfig {
                budget_mb: if raw.assets.budget_mb == 0 {
                    d.assets.budget_mb
                } else {
                    raw.assets.budget_mb
                },
                preload: raw.assets.preload,
            },
        })
    }
}

/// A located project: config + the directory it lives in.
pub struct Project {
    pub config: ProjectConfig,
    pub root: PathBuf,
}

impl Project {
    /// Load a project from a directory containing `liteengine.toml`.
    pub fn load(path: &Path) -> EngineResult<Project> {
        let cfg_path = path.join("liteengine.toml");
        if !cfg_path.exists() {
            return Err(EngineError::Config(format!(
                "no liteengine.toml in {}",
                path.display()
            )));
        }
        let raw = std::fs::read_to_string(&cfg_path)?;
        let config: ProjectConfig = toml::from_str(&raw)
            .map_err(|e| EngineError::Config(format!("bad liteengine.toml: {e}")))?;
        let entry = path.join(&config.entry);
        if !entry.exists() {
            return Err(EngineError::Config(format!(
                "entry script '{}' missing in {}",
                config.entry,
                path.display()
            )));
        }
        Ok(Project {
            config,
            root: path.to_path_buf(),
        })
    }

    /// Default project template written by `liteengine new`.
    pub fn template(title: &str) -> String {
        format!(
            r##"# LiteEngine project manifest
[game]
title = "{title}"
entry = "main.lua"
width = 960
height = 540
fps_cap = 60
background = "#87ceeb"

[graphics]
raytracing = false
window_scale = 1.0

[graphics.rt]
quality = "medium"   # low | medium | high
scale = 0.0          # 0 = derive from quality
bounces = 2
denoise = true

[modules]
enabled = ["engine-core", "gfx2d", "speech", "stream"]

[assets]
budget_mb = 256      # resident texture budget (LRU eviction above this)
preload = []
"##
        )
    }
}
