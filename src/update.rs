//! Self-update from GitHub releases.
//!
//! The plugin looks for a newer release, downloads the zip for this platform, checks it against
//! the release's SHA256SUMS.txt and copies the new VST3/CLAP files over the installed ones. A
//! binary that's currently loaded can't be overwritten on Windows, but it can be renamed, so the
//! old file is moved aside (`*.persona-old`) and deleted the next time the plugin loads. The new
//! version is used after the host is restarted.

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const REPO: &str = "GPTmadeit/persona";
const OLD_SUFFIX: &str = "persona-old";

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Available { version: String, notes: String },
    Installing { version: String },
    Installed { version: String, detail: String },
    Error(String),
}

pub type SharedStatus = Arc<Mutex<Status>>;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct Settings {
    pub auto_check: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { auto_check: true }
    }
}

fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Persona").join("settings.json"))
}

pub fn load_settings() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) {
    if let Some(p) = settings_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string_pretty(s).unwrap_or_default());
    }
}

/// Asset suffix for this build, e.g. `windows-x64`.
pub fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "aarch64") {
            "windows-arm64"
        } else {
            "windows-x64"
        }
    } else if cfg!(target_os = "macos") {
        "macos-universal"
    } else {
        "linux-x64"
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(60))
        .user_agent(concat!("Persona/", env!("CARGO_PKG_VERSION")))
        .build()
}

#[derive(serde::Deserialize)]
struct GhRelease {
    tag_name: String,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<GhAsset>,
}

#[derive(serde::Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

struct Found {
    version: semver::Version,
    notes: String,
    zip_url: String,
    zip_name: String,
    sums_url: Option<String>,
}

fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
}

/// The newest published release, if it's newer than `current`.
fn latest(current: &semver::Version) -> Result<Option<Found>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let rel: GhRelease = agent()
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("could not reach GitHub: {e}"))?
        .into_json()
        .map_err(|e| format!("bad response from GitHub: {e}"))?;
    if rel.draft || rel.prerelease {
        return Ok(None);
    }
    let version = semver::Version::parse(rel.tag_name.trim_start_matches('v'))
        .map_err(|e| format!("bad release tag {}: {e}", rel.tag_name))?;
    if version <= *current {
        return Ok(None);
    }
    let suffix = format!("{}.zip", platform());
    let zip = rel
        .assets
        .iter()
        .find(|a| a.name.ends_with(&suffix))
        .ok_or_else(|| format!("release {version} has no download for {}", platform()))?;
    let sums = rel.assets.iter().find(|a| a.name == "SHA256SUMS.txt");
    Ok(Some(Found {
        version,
        notes: rel.body.unwrap_or_default(),
        zip_url: zip.browser_download_url.clone(),
        zip_name: zip.name.clone(),
        sums_url: sums.map(|s| s.browser_download_url.clone()),
    }))
}

pub fn spawn_check(status: SharedStatus) {
    *status.lock() = Status::Checking;
    std::thread::spawn(move || {
        let s = match latest(&current_version()) {
            Ok(Some(f)) => Status::Available { version: f.version.to_string(), notes: f.notes },
            Ok(None) => Status::UpToDate,
            Err(e) => Status::Error(e),
        };
        *status.lock() = s;
    });
}

pub fn spawn_install(status: SharedStatus) {
    let version = match &*status.lock() {
        Status::Available { version, .. } => version.clone(),
        _ => return,
    };
    *status.lock() = Status::Installing { version: version.clone() };
    std::thread::spawn(move || {
        let s = match install(&current_version()) {
            Ok((v, detail)) => Status::Installed { version: v, detail },
            Err(e) => Status::Error(e),
        };
        *status.lock() = s;
    });
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let resp = agent().get(url).call().map_err(|e| format!("download failed: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(200 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| format!("download failed: {e}"))?;
    Ok(buf)
}

fn install(current: &semver::Version) -> Result<(String, String), String> {
    let f = latest(current)?.ok_or("already up to date")?;
    let data = download(&f.zip_url)?;
    let hash = hex(&Sha256::digest(&data));
    match &f.sums_url {
        Some(u) => {
            let sums = String::from_utf8_lossy(&download(u)?).to_string();
            let ok = sums.lines().any(|l| {
                let mut it = l.split_whitespace();
                it.next() == Some(hash.as_str())
                    && it.next().map(|n| n.trim_start_matches('*')) == Some(f.zip_name.as_str())
            });
            if !ok {
                return Err("checksum mismatch, update aborted".into());
            }
        }
        None => return Err("release has no SHA256SUMS.txt, update aborted".into()),
    }

    let staging = std::env::temp_dir().join(format!("persona-update-{}", f.version));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| e.to_string())?;
    zip.extract(&staging).map_err(|e| format!("unzip failed: {e}"))?;

    let mut updated = Vec::new();
    let mut errors = Vec::new();
    for (name, targets) in [("Persona.vst3", install_targets("vst3")), ("Persona.clap", install_targets("clap"))] {
        let src = staging.join(name);
        if !src.exists() {
            continue;
        }
        for dst in targets {
            match replace(&src, &dst) {
                Ok(()) => updated.push(dst.display().to_string()),
                Err(e) => errors.push(format!("{}: {e}", dst.display())),
            }
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
    if updated.is_empty() {
        return Err(if errors.is_empty() {
            "could not find the installed plugin files".into()
        } else {
            errors.join("; ")
        });
    }
    let mut detail = format!("Updated: {}", updated.join(", "));
    if !errors.is_empty() {
        detail.push_str(&format!(" | Skipped: {}", errors.join("; ")));
    }
    Ok((f.version.to_string(), detail))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The bundle/file the host loaded us from, e.g. `.../Persona.vst3` or `.../Persona.clap`.
fn loaded_bundle() -> Option<(PathBuf, &'static str)> {
    let lib = process_path::get_dylib_path()?;
    for anc in lib.ancestors() {
        let name = anc.file_name()?.to_string_lossy().to_lowercase();
        if name.ends_with(".vst3") {
            // On Windows/Linux the binary inside the bundle is also called *.vst3; take the outermost.
            let outer = anc
                .ancestors()
                .skip(1)
                .find(|a| a.file_name().map(|n| n.to_string_lossy().to_lowercase().ends_with(".vst3")).unwrap_or(false));
            return Some((outer.unwrap_or(anc).to_path_buf(), "vst3"));
        }
        if name.ends_with(".clap") {
            return Some((anc.to_path_buf(), "clap"));
        }
    }
    None
}

fn standard_dirs(kind: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let upper = kind.to_uppercase();
    if cfg!(target_os = "windows") {
        if let Ok(cf) = std::env::var("CommonProgramFiles") {
            v.push(PathBuf::from(cf).join(&upper));
        }
        if let Some(d) = dirs::data_local_dir() {
            v.push(d.join("Programs").join("Common").join(&upper));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(h) = dirs::home_dir() {
            v.push(h.join("Library/Audio/Plug-Ins").join(&upper));
        }
        v.push(PathBuf::from("/Library/Audio/Plug-Ins").join(&upper));
    } else if let Some(h) = dirs::home_dir() {
        v.push(h.join(format!(".{kind}")));
    }
    v
}

fn install_targets(kind: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some((p, k)) = loaded_bundle() {
        if k == kind {
            out.push(p);
        }
    }
    let fname = format!("Persona.{kind}");
    for d in standard_dirs(kind) {
        let p = d.join(&fname);
        if p.exists() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Copies `src` (file or directory) over `dst`, moving locked files aside first.
fn replace(src: &Path, dst: &Path) -> Result<(), String> {
    if src.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            replace(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if dst.is_dir() {
        return Err("expected a file but found a folder".into());
    }
    if dst.exists() && std::fs::remove_file(dst).is_err() {
        let aside = dst.with_extension(format!(
            "{}.{OLD_SUFFIX}",
            dst.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()
        ));
        let _ = std::fs::remove_file(&aside);
        std::fs::rename(dst, &aside).map_err(|e| format!("in use and can't be moved ({e}); close the DAW and retry"))?;
    }
    std::fs::copy(src, dst).map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

/// Deletes binaries an earlier update moved aside. Runs in the background at plugin load.
pub fn cleanup_old_binaries() {
    std::thread::spawn(|| {
        let mut roots: Vec<PathBuf> = loaded_bundle().map(|(p, _)| p).into_iter().collect();
        for kind in ["vst3", "clap"] {
            roots.extend(standard_dirs(kind).into_iter().map(|d| d.join(format!("Persona.{kind}"))));
        }
        for r in roots {
            sweep(&r, 0);
            if let Some(parent) = r.parent() {
                if let Ok(rd) = std::fs::read_dir(parent) {
                    for e in rd.flatten() {
                        if e.file_name().to_string_lossy().ends_with(OLD_SUFFIX) {
                            let _ = std::fs::remove_file(e.path());
                        }
                    }
                }
            }
        }
    });
}

fn sweep(dir: &Path, depth: u32) {
    if depth > 6 || !dir.is_dir() {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                sweep(&p, depth + 1);
            } else if p.to_string_lossy().ends_with(OLD_SUFFIX) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_moves_files_into_place() {
        let base = std::env::temp_dir().join(format!("persona-test-{}", std::process::id()));
        let src = base.join("src/Persona.vst3/Contents/x86_64-linux");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("Persona.so"), b"new").unwrap();
        let dst = base.join("dst/Persona.vst3/Contents/x86_64-linux");
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("Persona.so"), b"old").unwrap();
        replace(&base.join("src/Persona.vst3"), &base.join("dst/Persona.vst3")).unwrap();
        assert_eq!(std::fs::read(dst.join("Persona.so")).unwrap(), b"new");
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Downloads the real latest release from GitHub and installs it over fake old plugin files
    /// in a throwaway home folder. Needs network: `cargo test --release -- --ignored live_update`.
    #[test]
    #[ignore]
    #[cfg(target_os = "linux")]
    fn live_update_installs_latest_release() {
        let home = std::env::temp_dir().join(format!("persona-home-{}", std::process::id()));
        let vst3 = home.join(".vst3/Persona.vst3/Contents/x86_64-linux");
        std::fs::create_dir_all(&vst3).unwrap();
        std::fs::write(vst3.join("Persona.so"), b"old").unwrap();
        std::fs::create_dir_all(home.join(".clap")).unwrap();
        std::fs::write(home.join(".clap/Persona.clap"), b"old").unwrap();
        std::env::set_var("HOME", &home);

        let (version, detail) = install(&semver::Version::new(0, 0, 0)).expect("update failed");
        println!("installed {version}: {detail}");
        let so = std::fs::read(vst3.join("Persona.so")).unwrap();
        let clap = std::fs::read(home.join(".clap/Persona.clap")).unwrap();
        assert!(so.len() > 100_000 && so.starts_with(b"\x7fELF"), "VST3 binary not replaced");
        assert!(clap.len() > 100_000 && clap.starts_with(b"\x7fELF"), "CLAP binary not replaced");
        std::fs::remove_dir_all(&home).unwrap();
    }
}
