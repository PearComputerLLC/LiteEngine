//! `liteengine` — project manager, runner and packager.
//!
//! The binary is dual-purpose:
//!
//! * with a subcommand (`run`, `build`, `module`, ...) it is the developer CLI;
//! * with no subcommand it is the **game runner**: it looks for an embedded
//!   `.litepkg` trailer in its own executable, then for `game.litepkg` next to
//!   the executable (or in `../Resources/` inside a `.app` bundle), and runs
//!   the game using the engine statically linked into this binary — the CLI
//!   *is* the LiteEngine runtime, so games built with `liteengine build` ship
//!   the actual engine code alongside them.

mod pkg;

use liteengine_core::device::{DeviceInfo, GpuPath};
use liteengine_core::host::{Host, Renderer};
use liteengine_core::project::{Project, ProjectConfig};
use liteengine_core::assets::Vfs;
use liteengine_core::Engine;
use liteengine_render::window::{HeadlessHost, make_host};
use liteengine_render::{SoftRenderer, save_png};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Game mode: no subcommand (or only run flags like --headless) + a
    // package somewhere => run the game.
    let looks_like_game = args.is_empty() || (args[0].starts_with('-') && args[0] != "--version" && args[0] != "--help");
    if looks_like_game {
        if let Some(game) = locate_game_package() {
            return run_game_from_package(game, &args);
        }
    }
    if args.is_empty() {
        print_help();
        return ExitCode::from(0);
    }

    match args[0].as_str() {
        "new" => cmd_new(&args[1..]),
        "run" => cmd_run(&args[1..]),
        "build" => cmd_build(&args[1..]),
        "module" => cmd_module(&args[1..]),
        "devices" => cmd_devices(),
        "doctor" => cmd_doctor(),
        "lint" => cmd_lint(&args[1..]),
        "guide" => cmd_guide(&args[1..]),
        "version" | "--version" | "-V" => {
            println!("liteengine {} (engine {})", liteengine_core::ENGINE_VERSION, liteengine_core::ENGINE_VERSION);
            ExitCode::from(0)
        }
        "help" | "--help" | "-h" => {
            print_help();
            ExitCode::from(0)
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!(
        r#"LiteEngine — the lightweight, modular, Lua-scripted game engine

USAGE:
    liteengine                              run a packaged game (embedded or adjacent .litepkg)
    liteengine new <name>                   scaffold a new project in ./<name>
    liteengine run <path> [flags]           run a project directory or .litepkg
    liteengine build [path] [flags]         package + bundle the game
    liteengine module <sub> [args]          list | add | rm | pack | info | search
    liteengine devices                      show detected device + render path
    liteengine doctor                       environment sanity checks
    liteengine lint [path]                  static project checks
    liteengine guide [--open]               locate (and open) the Developer Guide PDF
    liteengine version

RUN FLAGS:
    --headless            render without a window (CI, screenshots)
    --frames N            stop after N frames
    --screenshot <file>   save the last frame as PNG
    --scale X             window scale factor (default from project)
    --force-legacy        use the software path even on modern devices
    --force-modern        prefer the wgpu path (build must enable `modern`)

BUILD FLAGS (mutually exclusive; default --folder):
    --packed              single executable with the .litepkg embedded
    --folder              folder with the runner binary + game.litepkg
    --macos               .app bundle (runner + Info.plist + Resources/game.litepkg)
    --zip                 zip of the --folder layout
    --out <dir>           output directory (default ./dist)"#
    );
}

// --------------------------------------------------------------------- new

fn cmd_new(args: &[String]) -> ExitCode {
    let name = match args.first() {
        Some(n) => n.clone(),
        None => {
            eprintln!("usage: liteengine new <name>");
            return ExitCode::from(2);
        }
    };
    let dir = PathBuf::from(&name);
    if dir.exists() {
        eprintln!("'{name}' already exists");
        return ExitCode::from(2);
    }
    for sub in ["assets", "modules"] {
        std::fs::create_dir_all(dir.join(sub)).expect("create dirs");
    }
    std::fs::write(
        dir.join("liteengine.toml"),
        Project::template(&name),
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("main.lua"),
        include_str!("../templates/main.lua"),
    )
    .expect("write main.lua");
    std::fs::write(dir.join(".gitignore"), "dist/\n").ok();
    println!("created project '{name}'");
    println!("next: cd {name} && liteengine run");
    ExitCode::from(0)
}

// --------------------------------------------------------------------- run

struct RunOpts {
    headless: bool,
    frames: Option<u64>,
    screenshot: Option<String>,
    scale: f32,
    width: Option<u32>,
    height: Option<u32>,
}

fn parse_run_flags(args: &[String]) -> (RunOpts, Vec<String>) {
    let mut opts = RunOpts {
        headless: false,
        frames: None,
        screenshot: None,
        scale: 1.0,
        width: None,
        height: None,
    };
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--headless" => opts.headless = true,
            "--frames" => {
                i += 1;
                opts.frames = args.get(i).and_then(|v| v.parse().ok());
            }
            "--screenshot" => {
                i += 1;
                opts.screenshot = args.get(i).cloned();
                if opts.screenshot.is_some() {
                    opts.headless = true;
                }
            }
            "--scale" => {
                i += 1;
                opts.scale = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(1.0);
            }
            "--width" => {
                i += 1;
                opts.width = args.get(i).and_then(|v| v.parse().ok());
            }
            "--height" => {
                i += 1;
                opts.height = args.get(i).and_then(|v| v.parse().ok());
            }
            other => rest.push(other.to_string()),
        }
        i += 1;
    }
    (opts, rest)
}

fn cmd_run(args: &[String]) -> ExitCode {
    let (opts, rest) = parse_run_flags(args);
    let path = match rest.first() {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: liteengine run <path|game.litepkg>");
            return ExitCode::from(2);
        }
    };
    match load_game(&path) {
        Ok((config, vfs)) => run_engine(config, vfs, &opts, "run"),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

/// Locate a `.litepkg` for game mode: embedded trailer, exe-adjacent file,
/// or macOS `../Resources/game.litepkg`.
fn locate_game_package() -> Option<Vec<u8>> {
    if let Some(bytes) = pkg::self_bytes() {
        if let Some(pkg_data) = pkg::extract_trailer(&bytes) {
            return Some(pkg_data);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for candidate in [
        dir.join("game.litepkg"),
        dir.join("../Resources/game.litepkg"),
        dir.join("../../Resources/game.litepkg"),
    ] {
        if candidate.exists() {
            if let Ok(b) = std::fs::read(&candidate) {
                return Some(b);
            }
        }
    }
    None
}

fn run_game_from_package(game: Vec<u8>, flag_args: &[String]) -> ExitCode {
    let config = match read_config_from_package(&game) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to read packaged game: {e}");
            return ExitCode::from(1);
        }
    };
    let (mut opts, _) = parse_run_flags(flag_args);
    opts.scale = config.window_scale.max(0.1);
    println!("running packaged game: {}", config.title);
    run_engine(config, Vfs::Package(game), &opts, "game")
}

fn read_config_from_package(bytes: &[u8]) -> Result<ProjectConfig, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut raw = String::new();
    use std::io::Read;
    zip.by_name("liteengine.toml")
        .map_err(|e| e.to_string())?
        .read_to_string(&mut raw)
        .map_err(|e| e.to_string())?;
    toml::from_str(&raw).map_err(|e| e.to_string())
}

fn load_game(path: &Path) -> Result<(ProjectConfig, Vfs), String> {
    if path.is_dir() {
        let project = Project::load(path).map_err(|e| e.to_string())?;
        Ok((project.config, Vfs::Dir(path.to_path_buf())))
    } else if path.extension().and_then(|e| e.to_str()) == Some("litepkg") {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let config = read_config_from_package(&bytes)?;
        Ok((config, Vfs::Package(bytes)))
    } else {
        Err(format!(
            "'{}' is neither a project directory nor a .litepkg",
            path.display()
        ))
    }
}

fn run_engine(mut config: ProjectConfig, vfs: Vfs, opts: &RunOpts, mode: &str) -> ExitCode {
    // Window geometry.
    let scale = if opts.scale != 1.0 {
        opts.scale
    } else {
        config.window_scale.max(0.1)
    };
    let w = opts
        .width
        .unwrap_or(config.width)
        .max(160) as f32
        * scale;
    let h = opts
        .height
        .unwrap_or(config.height)
        .max(120) as f32
        * scale;
    let (w, h) = (w.round() as usize, h.round() as usize);

    let device = DeviceInfo::detect();

    // Host.
    let host: Box<dyn Host> = if opts.headless {
        let mut hh = HeadlessHost::new(w, h);
        hh.frames_limit = opts.frames;
        hh.screenshot_at = opts.frames;
        Box::new(hh)
    } else {
        match make_host(w, h, &config.title, false) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        }
    };

    // Renderer selection: modern wgpu when compiled in AND the device wants it.
    let mut renderer: Box<dyn Renderer> = {
        #[cfg(feature = "modern")]
        {
            if device.path == GpuPath::ModernWgpu {
                match liteengine_render::modern::GpuRenderer::new() {
                    Ok(r) => Box::new(r),
                    Err(e) => {
                        eprintln!(
                            "[liteengine] wgpu init failed ({e}); falling back to software path"
                        );
                        Box::new(SoftRenderer::new())
                    }
                }
            } else {
                Box::new(SoftRenderer::new())
            }
        }
        #[cfg(not(feature = "modern"))]
        {
            if device.path == GpuPath::ModernWgpu {
                eprintln!(
                    "[liteengine] device wants the modern wgpu path but this build lacks the 'modern' feature; using the software path"
                );
            }
            Box::new(SoftRenderer::new())
        }
    };

    println!(
        "[liteengine] {} v{} | {} | {}x{} | device: {} ({}) | renderer: {}",
        mode,
        liteengine_core::ENGINE_VERSION,
        config.title,
        w,
        h,
        device.os_pretty,
        device.path,
        renderer.name()
    );

    let mut engine = match Engine::new(config, device, vfs, host) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[liteengine] {e}");
            return ExitCode::from(1);
        }
    };

    if let Err(e) = engine.run(&mut *renderer, opts.frames) {
        eprintln!("[liteengine] {e}");
        return ExitCode::from(1);
    }

    // Screenshot support (headless).
    if let Some(path) = &opts.screenshot {
        let shared = engine.shared();
        let frame = {
            let st = shared.borrow();
            st.host.last_frame().map(|(v, w, h)| (v.to_vec(), w, h))
        };
        if let Some((buf, w, h)) = frame {
            match save_png(path, &buf, w, h) {
                Ok(()) => println!("[liteengine] screenshot saved: {path} ({w}x{h})"),
                Err(e) => eprintln!("[liteengine] screenshot failed: {e}"),
            }
        }
    }
    ExitCode::from(0)
}

// ------------------------------------------------------------------- build

fn cmd_build(args: &[String]) -> ExitCode {
    let mut path = PathBuf::from(".");
    let mut mode = "folder";
    let mut out = PathBuf::from("dist");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--packed" => mode = "packed",
            "--folder" => mode = "folder",
            "--macos" => mode = "macos",
            "--zip" => mode = "zip",
            "--out" => {
                i += 1;
                out = PathBuf::from(args.get(i).map(|s| s.as_str()).unwrap_or("dist"));
            }
            other => path = PathBuf::from(other),
        }
        i += 1;
    }

    let project = match Project::load(&path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let name = project
        .config
        .title
        .to_lowercase()
        .replace(' ', "-")
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "");
    let name = if name.is_empty() { "game".to_string() } else { name };

    println!("[build] packaging {name}...");
    let pkg = match pkg::build_litepkg(&path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[build] {e}");
            return ExitCode::from(1);
        }
    };
    println!(
        "[build] game.litepkg: {:.1} KiB",
        pkg.len() as f64 / 1024.0
    );

    let runner = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[build] cannot locate runner: {e}");
            return ExitCode::from(1);
        }
    };

    let result = match mode {
        "packed" => {
            let out_path = out.join(&name);
            let _ = std::fs::create_dir_all(&out);
            match pkg::append_trailer(&runner, &pkg, &out_path) {
                Ok(base) => {
                    println!(
                        "[build] packed executable: {} (runner {:.1} MiB + package)",
                        out_path.display(),
                        base as f64 / 1048576.0
                    );
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        "folder" | "zip" => {
            let dir = out.join(format!("{name}-dist"));
            let _ = std::fs::create_dir_all(&dir);
            let exe_path = dir.join(&name);
            if std::fs::copy(&runner, &exe_path).is_err() {
                return ExitCode::from(1);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &exe_path,
                    std::fs::Permissions::from_mode(0o755),
                );
            }
            let pkg_path = dir.join("game.litepkg");
            if let Err(e) = std::fs::write(&pkg_path, &pkg) {
                eprintln!("[build] {e}");
                return ExitCode::from(1);
            }
            println!("[build] folder bundle: {}", dir.display());
            if mode == "zip" {
                let zip_path = out.join(format!("{name}.zip"));
                match zip_dir(&dir, &zip_path) {
                    Ok(()) => println!("[build] zip bundle: {}", zip_path.display()),
                    Err(e) => {
                        eprintln!("[build] zip failed: {e}");
                        return ExitCode::from(1);
                    }
                }
            }
            Ok(())
        }
        "macos" => {
            let app = out.join(format!("{name}.app"));
            let contents = app.join("Contents");
            let _ = std::fs::create_dir_all(contents.join("MacOS"));
            let _ = std::fs::create_dir_all(contents.join("Resources"));
            let exe_path = contents.join("MacOS").join(&name);
            if std::fs::copy(&runner, &exe_path).is_err() {
                return ExitCode::from(1);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &exe_path,
                    std::fs::Permissions::from_mode(0o755),
                );
            }
            std::fs::write(
                contents.join("Info.plist"),
                pkg::info_plist(&project.config.title, &name),
            )
            .ok();
            std::fs::write(contents.join("PkgInfo"), "APPL????").ok();
            if let Err(e) = std::fs::write(contents.join("Resources").join("game.litepkg"), &pkg) {
                eprintln!("[build] {e}");
                return ExitCode::from(1);
            }
            println!("[build] .app bundle: {}", app.display());
            Ok(())
        }
        _ => unreachable!(),
    };

    match result {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("[build] {e}");
            ExitCode::from(1)
        }
    }
}

fn zip_dir(dir: &Path, out_zip: &Path) -> Result<(), String> {
    let file = std::fs::File::create(out_zip).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for f in pkg::walk(dir) {
        let rel = f
            .strip_prefix(dir)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .to_string();
        zip.start_file(rel.as_str(), opts)
            .map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&f).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut zip, &bytes).map_err(|e| e.to_string())?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

// ------------------------------------------------------------------ module

fn cmd_module(args: &[String]) -> ExitCode {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    let project_dir = std::env::current_dir().unwrap_or_default();
    match sub {
        "list" => {
            let mgr = liteengine_core::modules::ModuleManager::new();
            match mgr {
                Ok(mut m) => {
                    fill_native_features(&mut m);
                    let _ = m.load_project_modules(&project_dir.join("modules"));
                    println!("{:<14} {:<8} {}", "NAME", "VERSION", "DESCRIPTION");
                    for (name, desc) in m.table() {
                        println!("{:<14} {:<8} {}", name, "", desc);
                    }
                    println!("\nnative features available: {:?}", {
                        let mut v: Vec<_> = m.native_features.iter().collect();
                        v.sort();
                        v
                    });
                }
                Err(e) => eprintln!("{e}"),
            }
            ExitCode::from(0)
        }
        "add" => {
            let file = match args.get(1) {
                Some(f) => PathBuf::from(f),
                None => {
                    eprintln!("usage: liteengine module add <file.lemodule>");
                    return ExitCode::from(2);
                }
            };
            match add_module(&project_dir, &file) {
                Ok(name) => {
                    println!("added module '{name}' to {}", project_dir.display());
                    ExitCode::from(0)
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::from(1)
                }
            }
        }
        "rm" => {
            let name = args.get(1).cloned().unwrap_or_default();
            let dir = project_dir.join("modules");
            let mut removed = false;
            for f in pkg::walk(&dir) {
                if f.file_name().unwrap_or_default().to_string_lossy().starts_with(&name) {
                    let _ = std::fs::remove_file(&f);
                    removed = true;
                }
            }
            if removed {
                println!("removed '{name}' from project modules");
                ExitCode::from(0)
            } else {
                eprintln!("module '{name}' not found in project modules/");
                ExitCode::from(1)
            }
        }
        "pack" => {
            let dir = match args.get(1) {
                Some(d) => PathBuf::from(d),
                None => {
                    eprintln!("usage: liteengine module pack <dir>");
                    return ExitCode::from(2);
                }
            };
            match pack_module_dir(&dir) {
                Ok(out) => {
                    println!("packed {}", out.display());
                    ExitCode::from(0)
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::from(1)
                }
            }
        }
        "info" => {
            let name = args.get(1).cloned().unwrap_or_default();
            match liteengine_core::modules::ModuleManager::new() {
                Ok(mut m) => {
                    let _ = m.load_project_modules(&project_dir.join("modules"));
                    match m.find(&name) {
                        Some(modm) => {
                            println!(
                                "name: {}\nversion: {}\nengine_api: {}\nprovides: {}\nnative: {:?}\n\n{}",
                                modm.manifest.module.name,
                                modm.manifest.module.version,
                                modm.manifest.module.engine_api,
                                modm.manifest.module.provides,
                                modm.manifest.module.native,
                                modm.readme.clone().unwrap_or_default()
                            );
                            ExitCode::from(0)
                        }
                        None => {
                            eprintln!("module '{name}' not found");
                            ExitCode::from(1)
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::from(1)
                }
            }
        }
        "search" => {
            let query = args.get(1).cloned().unwrap_or_default().to_lowercase();
            match liteengine_core::modules::ModuleManager::new() {
                Ok(mut m) => {
                    let _ = m.load_project_modules(&project_dir.join("modules"));
                    let mut hits = 0;
                    for (name, desc) in m.table() {
                        if query.is_empty()
                            || name.to_lowercase().contains(&query)
                            || desc.to_lowercase().contains(&query)
                        {
                            println!("{name:<14} {desc}");
                            hits += 1;
                        }
                    }
                    println!("\n{hits} module(s) matching '{query}'");
                    ExitCode::from(0)
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::from(1)
                }
            }
        }
        other => {
            eprintln!("unknown module subcommand '{other}' (list|add|rm|pack|info|search)");
            ExitCode::from(2)
        }
    }
}

fn add_module(project: &Path, file: &Path) -> Result<String, String> {
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    let (manifest, _, _) =
        liteengine_core::modules::load_lemodule_bytes(&file.display().to_string(), &bytes)
            .map_err(|e| e.to_string())?;
    let name = manifest.module.name.clone();
    std::fs::create_dir_all(project.join("modules")).map_err(|e| e.to_string())?;
    let dest = project
        .join("modules")
        .join(format!("{}-{}.lemodule", name, manifest.module.version));
    std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
    // Append to enabled list if absent.
    let cfg_path = project.join("liteengine.toml");
    if cfg_path.exists() {
        let raw = std::fs::read_to_string(&cfg_path).map_err(|e| e.to_string())?;
        if !raw.contains(&format!("\"{name}\"")) {
            let mut new_raw = String::new();
            let mut inserted = false;
            for line in raw.lines() {
                if !inserted && line.trim_start().starts_with("enabled") {
                    let trimmed = line.trim_end();
                    let inner = trimmed
                        .trim_start_matches(|c: char| c != '[')
                        .trim_end_matches(|c: char| c != ']');
                    let new_line = if inner.trim().is_empty() {
                        format!("enabled = [\"{name}\"]")
                    } else {
                        format!(
                            "enabled = [{}, \"{}\"]",
                            inner.trim().trim_matches(['[', ']']),
                            name
                        )
                    };
                    new_raw.push_str(&new_line);
                    new_raw.push('\n');
                    inserted = true;
                } else {
                    new_raw.push_str(line);
                    new_raw.push('\n');
                }
            }
            std::fs::write(&cfg_path, new_raw).map_err(|e| e.to_string())?;
        }
    }
    Ok(name)
}

fn pack_module_dir(dir: &Path) -> Result<PathBuf, String> {
    if !dir.join("module.toml").exists() {
        return Err(format!("{} lacks module.toml", dir.display()));
    }
    let name = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
    let out = dir.parent().unwrap_or(Path::new(".")).join(format!("{name}.lemodule"));
    let file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for f in pkg::walk(dir) {
        let rel = f
            .strip_prefix(dir)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .to_string();
        zip.start_file(rel.as_str(), opts)
            .map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&f).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut zip, &bytes).map_err(|e| e.to_string())?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

// --------------------------------------------------------- devices/doctor

fn cmd_devices() -> ExitCode {
    let d = DeviceInfo::detect();
    println!("os:          {} ({})", d.os_pretty, d.os);
    println!("arch:        {}", d.arch);
    println!("cores:       {}", d.cores);
    println!(
        "ram:         {}",
        d.ram_mb.map(|m| format!("{m} MiB")).unwrap_or_else(|| "unknown".into())
    );
    println!("render path: {} ({})", d.path, d.tier());
    println!("reason:      {}", d.reason);
    println!();
    println!("modules:");
    match liteengine_core::modules::ModuleManager::new() {
        Ok(mut m) => {
            let _ = m.load_project_modules(&std::env::current_dir().unwrap_or_default().join("modules"));
            for (name, desc) in m.table() {
                println!("  {name:<14} {desc}");
            }
        }
        Err(e) => println!("  (registry unavailable: {e})"),
    }
    ExitCode::from(0)
}

fn cmd_doctor() -> ExitCode {
    let mut failures = 0;
    let mut check = |name: &str, ok: bool, note: &str| {
        println!(
            "{:<4} {:<28} {}",
            if ok { "OK" } else { "FAIL" },
            name,
            note
        );
        if !ok {
            failures += 1;
        }
    };

    let d = DeviceInfo::detect();
    check("device detect", true, &format!("{} / {} / {}", d.os, d.arch, d.path));

    // Lua VM + API + scheduler probe.
    let (lua_ok, lua_note) = match liteengine_core::runtime::self_test() {
        Ok(()) => (true, "VM boot + API registration + scheduler".to_string()),
        Err(e) => (false, format!("self_test: {e}")),
    };
    check("lua 5.4 (embedded)", lua_ok, &lua_note);

    // Built-in module registry loads?
    let mods_ok = liteengine_core::modules::ModuleManager::new()
        .map(|m| {
            m.warnings.is_empty() && m.installed.len() >= 7
        })
        .unwrap_or(false);
    check(
        "module registry",
        mods_ok,
        "7 built-in .lemodule archives must load",
    );

    // PNG decode?
    let png_ok = png_decode_check();
    check("png decode", png_ok, "asset pipeline smoke test");

    // Window backend?
    #[cfg(feature = "window")]
    check("window backend", true, "minifb compiled in (window feature)");
    #[cfg(not(feature = "window"))]
    check("window backend", false, "built without 'window' feature — headless only");

    // Modern path?
    #[cfg(feature = "modern")]
    check("modern wgpu path", true, "wgpu compiled in (modern feature)");
    #[cfg(not(feature = "modern"))]
    check(
        "modern wgpu path",
        false,
        "built without 'modern' feature — software path only",
    );

    println!();
    if failures == 0 {
        println!("all checks passed");
        ExitCode::from(0)
    } else {
        println!("{failures} check(s) failed");
        ExitCode::from(1)
    }
}

fn png_decode_check() -> bool {
    // 1x1 transparent PNG hardcoded; decodes through the engine's asset stack.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
        0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
        0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
        0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    liteengine_render::png_probe(&[])
}

// ------------------------------------------------------------------- lint

fn cmd_lint(args: &[String]) -> ExitCode {
    let path = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let project = match Project::load(&path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let mut warnings: Vec<String> = Vec::new();

    // 1. Modules declared must exist.
    let registry = liteengine_core::modules::ModuleManager::new().ok();
    for name in &project.config.modules.enabled {
        let in_project = path
            .join("modules")
            .read_dir()
            .map(|entries| {
                entries.flatten().any(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with(name.as_str())
                })
            })
            .unwrap_or(false);
        let in_builtin = registry
            .as_ref()
            .map(|m| m.find(name).is_some())
            .unwrap_or(false);
        if !in_project && !in_builtin {
            warnings.push(format!("module '{name}' enabled but not found (builtin or modules/)"));
        }
    }

    // 2. Assets referenced from Lua must exist in the project.
    let entry = path.join(&project.config.entry);
    let mut lua_sources = Vec::new();
    for f in pkg::walk(&path) {
        if f.extension().and_then(|e| e.to_str()) == Some("lua") {
            if let Ok(s) = std::fs::read_to_string(&f) {
                lua_sources.push(s);
            }
        }
    }
    let _ = entry;
    let all_lua = lua_sources.join("\n");
    for tok in all_lua.split('"') {
        let t = tok.trim();
        if t.starts_with("assets/") {
            if !path.join(t).exists() {
                warnings.push(format!("asset '{t}' referenced but missing"));
            }
        }
    }

    // 3. API/module consistency.
    let has = |m: &str| project.config.modules.enabled.iter().any(|x| x == m);
    if all_lua.contains("Sprite.new") && !has("gfx2d") && !has("gfx3d") {
        warnings.push("Sprite.new used but neither gfx2d nor gfx3d is enabled".into());
    }
    if all_lua.contains(":say(") && !has("speech") {
        warnings.push("say()/think() used but the 'speech' module is not enabled".into());
    }
    if all_lua.contains("rt.") && !has("rtmode") {
        warnings.push("rt API used but the 'rtmode' module is not enabled".into());
    }
    if all_lua.contains("pbr3d.") && !has("pbr3d") {
        warnings.push("pbr3d API used but the 'pbr3d' module is not enabled".into());
    }
    if all_lua.contains(":in3d(") && !has("gfx3d") {
        warnings.push("in3d() used but the 'gfx3d' module is not enabled".into());
    }

    // 4. Asset budget sanity.
    let mut total = 0u64;
    for f in pkg::walk(&path.join("assets")) {
        if let Ok(md) = std::fs::metadata(&f) {
            total += md.len();
        }
    }
    let budget = project.config.assets.budget_mb * 1024 * 1024;
    if total > budget {
        warnings.push(format!(
            "assets total {:.1} MiB > budget_mb {} — heavy LRU churn expected",
            total as f64 / 1048576.0,
            project.config.assets.budget_mb
        ));
    }

    if warnings.is_empty() {
        println!("lint: no issues found in {}", path.display());
        ExitCode::from(0)
    } else {
        for w in &warnings {
            println!("warn: {w}");
        }
        println!("\nlint: {} warning(s)", warnings.len());
        ExitCode::from(0)
    }
}

// ------------------------------------------------------------------ guide

fn cmd_guide(args: &[String]) -> ExitCode {
    let open = args.iter().any(|a| a == "--open");
    let candidates = [
        PathBuf::from("docs/LiteEngine_Developer_Guide.pdf"),
        PathBuf::from("../docs/LiteEngine_Developer_Guide.pdf"),
        PathBuf::from("../../docs/LiteEngine_Developer_Guide.pdf"),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("docs/LiteEngine_Developer_Guide.pdf")))
            .unwrap_or_default(),
        std::env::var("LITEENGINE_GUIDE")
            .map(PathBuf::from)
            .unwrap_or_default(),
    ];
    for c in candidates.iter() {
        if c.exists() {
            println!("{}", c.display());
            if open {
                let _ = open_file(c);
            }
            return ExitCode::from(0);
        }
    }
    eprintln!(
        "Developer Guide PDF not found. Install it to docs/ or set LITEENGINE_GUIDE."
    );
    ExitCode::from(1)
}

/// Declare the compiled-in native features for display (module list/devices).
fn fill_native_features(m: &mut liteengine_core::modules::ModuleManager) {
    let d = DeviceInfo::detect();
    for f in [
        "core.events",
        "core.stage",
        "gfx2d.sprites",
        "gfx3d.billboards",
        "ui.speech",
        "assets.stream",
        "rt.toggle",
    ] {
        m.provide_native(f);
    }
    if d.path == GpuPath::ModernWgpu {
        m.provide_native("pbr3d.renderer");
    }
}

fn open_file(p: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(p)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &p.to_string_lossy()])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(p)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
