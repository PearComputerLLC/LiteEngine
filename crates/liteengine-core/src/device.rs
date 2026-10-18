//! Device capability detection and render-path selection.
//!
//! LiteEngine has two rendering paths:
//!
//! * [`GpuPath::LegacySoftware`] — the fully custom, driver-free software path
//!   (rasterizer + hybrid path tracer). Selected on hardware where GPU driver
//!   support for modern APIs cannot be assumed: pre-macOS-11 or non-ARM64 Macs
//!   (minimum target: a 2010 MacBook Air on OS X Mountain Lion), Windows 7 and
//!   below, and 32-bit x86 or non-x86/non-ARM64 Linux.
//! * [`GpuPath::ModernWgpu`] — the `wgpu` path (PBR 3D, shadow maps, screen-space
//!   RT effects) for modern devices: Apple Silicon on macOS 11+, Windows 8/10/11,
//!   and 64-bit x86_64/ARM64 Linux.
//!
//! The choice can be overridden for testing with `LITEENGINE_FORCE_LEGACY=1` or
//! `LITEENGINE_FORCE_MODERN=1`.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuPath {
    /// Custom optimized software rendering (raster + hybrid path tracing).
    LegacySoftware,
    /// wgpu (Vulkan/Metal/DX12/GL) with the PBR pipeline.
    ModernWgpu,
}

impl fmt::Display for GpuPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuPath::LegacySoftware => write!(f, "legacy-software"),
            GpuPath::ModernWgpu => write!(f, "modern-wgpu"),
        }
    }
}

/// Everything the engine knows about the machine it is running on.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// `macos`, `windows`, `linux`, ...
    pub os: String,
    /// `aarch64`, `x86_64`, `x86`, `riscv64`, ...
    pub arch: String,
    /// Parsed OS version `(major, minor, patch)` when detectable.
    pub os_version: Option<(u32, u32, u32)>,
    /// Human-readable OS name/version string.
    pub os_pretty: String,
    /// Logical CPU cores.
    pub cores: usize,
    /// Total RAM in MiB, when detectable.
    pub ram_mb: Option<u64>,
    /// Selected rendering path.
    pub path: GpuPath,
    /// Why this path was selected (shown by `liteengine devices`).
    pub reason: String,
}

impl DeviceInfo {
    /// Detect the device and choose the rendering path per the portability rules.
    pub fn detect() -> DeviceInfo {
        let os = std::env::consts::OS.to_string();
        let arch = std::env::consts::ARCH.to_string();
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let (os_version, os_pretty) = probe_os(&os);
        let ram_mb = probe_ram(&os);

        let is64 = cfg!(target_pointer_width = "64");
        let modern_arch = arch == "x86_64" || arch == "aarch64";

        // Environment overrides are useful for CI and for forcing the
        // software path on machines with flaky GPU drivers.
        let force_legacy = std::env::var("LITEENGINE_FORCE_LEGACY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let force_modern = std::env::var("LITEENGINE_FORCE_MODERN")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let (path, reason) = if force_legacy {
            (GpuPath::LegacySoftware, "LITEENGINE_FORCE_LEGACY=1 override".into())
        } else if force_modern {
            (GpuPath::ModernWgpu, "LITEENGINE_FORCE_MODERN=1 override".into())
        } else if os == "macos" {
            let ver = os_version.unwrap_or((10, 7, 0));
            if arch == "aarch64" && ver >= (11, 0, 0) {
                (
                    GpuPath::ModernWgpu,
                    format!(
                        "Apple Silicon + macOS {}.{} >= 11: wgpu (Metal) available",
                        ver.0, ver.1
                    ),
                )
            } else {
                (
                    GpuPath::LegacySoftware,
                    format!(
                        "macOS {}.{} on {arch}: pre-11 or non-ARM64 Mac uses the custom optimized software path",
                        ver.0, ver.1
                    ),
                )
            }
        } else if os == "windows" {
            let ver = os_version.unwrap_or((10, 0, 0));
            if ver <= (6, 2, 0) && ver >= (6, 0, 0) {
                // 6.0/6.1 = Vista/7 (treat 6.2+ loosely as 8+; Win7 => legacy)
                (
                    GpuPath::LegacySoftware,
                    format!(
                        "Windows {}.{} (<= Windows 7): no modern GPU API assumed; software path",
                        ver.0, ver.1
                    ),
                )
            } else if ver < (6, 0, 0) {
                (
                    GpuPath::LegacySoftware,
                    format!("Windows {}.{} (XP-era): software path", ver.0, ver.1),
                )
            } else {
                (
                    GpuPath::ModernWgpu,
                    format!("Windows {}.{}: wgpu (DX12) available", ver.0, ver.1),
                )
            }
        } else {
            // linux & friends
            if !is64 {
                (
                    GpuPath::LegacySoftware,
                    format!("32-bit {arch}: software path (no wgpu on 32-bit x86)"),
                )
            } else if !modern_arch {
                (
                    GpuPath::LegacySoftware,
                    format!("{arch} is neither x86 nor ARM64: software path"),
                )
            } else {
                (
                    GpuPath::ModernWgpu,
                    format!("64-bit {arch} Linux: wgpu (Vulkan) available"),
                )
            }
        };

        DeviceInfo {
            os,
            arch,
            os_version,
            os_pretty,
            cores,
            ram_mb,
            path,
            reason,
        }
    }

    /// Short tier label used by docs and `doctor`.
    pub fn tier(&self) -> &'static str {
        match self.path {
            GpuPath::ModernWgpu => "modern (wgpu / PBR)",
            GpuPath::LegacySoftware => "legacy (custom software renderer + hybrid RT)",
        }
    }
}

fn probe_os(os: &str) -> (Option<(u32, u32, u32)>, String) {
    fn parse_ver(s: &str) -> Option<(u32, u32, u32)> {
        let mut it = s
            .trim()
            .split('.')
            .map(|p| p.trim().to_string());
        let major = it.next()?.parse().ok()?;
        let minor = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let patch = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        Some((major, minor, patch))
    }

    match os {
        "macos" => {
            if let Ok(out) = std::process::Command::new("sw_vers")
                .args(["-productVersion"])
                .output()
            {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if let Ok(pretty) = std::process::Command::new("sw_vers")
                    .args(["-productName", "-productVersion"])
                    .output()
                {
                    let p = String::from_utf8_lossy(&pretty.stdout).replace('\n', " ");
                    return (parse_ver(&s), p.trim().to_string());
                }
                return (parse_ver(&s), format!("macOS {s}"));
            }
            (None, "macOS (version unknown)".into())
        }
        "windows" => {
            // `cmd /c ver` prints e.g. "Microsoft Windows [Version 10.0.19045.3803]"
            if let Ok(out) = std::process::Command::new("cmd").args(["/c", "ver"]).output() {
                let s = String::from_utf8_lossy(&out.stdout);
                for tok in s.split_whitespace() {
                    if tok.starts_with("Version") {
                        continue;
                    }
                    let clean = tok.trim_start_matches('[').trim_end_matches(']');
                    if let Some(v) = parse_ver(clean) {
                        return (Some(v), format!("Windows {}.{}", v.0, v.1));
                    }
                }
            }
            (None, "Windows (version unknown)".into())
        }
        _ => {
            // Linux: read /etc/os-release for a pretty name.
            let mut pretty = "Linux".to_string();
            if let Ok(txt) = std::fs::read_to_string("/etc/os-release") {
                for line in txt.lines() {
                    if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
                        pretty = v.trim_matches('"').to_string();
                        break;
                    }
                }
            }
            if let Ok(uname) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
                pretty.push_str(&format!(" (kernel {})", uname.trim()));
            }
            (None, pretty)
        }
    }
}

fn probe_ram(os: &str) -> Option<u64> {
    match os {
        "linux" => {
            let txt = std::fs::read_to_string("/proc/meminfo").ok()?;
            for line in txt.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    let kb: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
                    return Some(kb / 1024);
                }
            }
            None
        }
        "macos" => {
            let out = std::process::Command::new("sysctl").args(["-n", "hw.memsize"]).output().ok()?;
            let bytes: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
            Some(bytes / (1024 * 1024))
        }
        "windows" => {
            let out = std::process::Command::new("wmic")
                .args(["computersystem", "get", "TotalPhysicalMemory"])
                .output()
                .ok()?;
            let s = String::from_utf8_lossy(&out.stdout);
            s.split_whitespace()
                .find_map(|t| t.parse::<u64>().ok())
                .map(|b| b / (1024 * 1024))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_does_not_crash() {
        let d = DeviceInfo::detect();
        assert!(!d.os.is_empty());
        assert!(!d.reason.is_empty());
    }
}
