//! Updates only the complete, self-contained eoncn/codex package.
//! Releases are downloaded from the fixed fork, verified, staged on the
//! installation filesystem, and installed with rollback on failure.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const LATEST_RELEASE: &str = "https://api.github.com/repos/eoncn/codex/releases/latest";
const ASSET_PREFIX: &str = "https://github.com/eoncn/codex/releases/download/";
const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct UpdateManifest {
    schema: u64,
    version: String,
    build: u64,
    assets: HashMap<String, UpdateAsset>,
}

#[derive(Deserialize)]
struct UpdateAsset {
    name: String,
    sha256: String,
}

#[derive(Deserialize)]
struct PackageManifest {
    version: String,
    target: String,
    entrypoint: String,
    #[serde(rename = "forkBuildNumber")]
    fork_build_number: u64,
}

fn release_asset<'a>(release: &'a GithubRelease, name: &str) -> Result<&'a str> {
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .context(format!("GitHub Release does not include {name}"))?;
    ensure!(
        asset.browser_download_url.starts_with(ASSET_PREFIX),
        "Refusing non-fork release download URL"
    );
    Ok(&asset.browser_download_url)
}

fn parse_release_tag(tag: &str) -> Result<(semver::Version, u64)> {
    let value = tag
        .strip_prefix("fork-v")
        .context("Expected fork-v<major>.<minor>.<patch>.<build> release tag")?;
    let (version, build) = value.rsplit_once('.').context("Missing fork build number")?;
    ensure!(
        !build.is_empty() && build.chars().all(|ch| ch.is_ascii_digit()),
        "Invalid fork build number"
    );
    Ok((semver::Version::parse(version)?, build.parse()?))
}

fn current_build() -> Result<u64> {
    Ok(option_env!("CODEX_FORK_BUILD_NUMBER")
        .context("This Codex binary was not built by the fork release workflow")?
        .parse()?)
}

fn target() -> Result<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("x86_64-pc-windows-msvc")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok("x86_64-apple-darwin")
    } else {
        bail!("Fork updates are supported only on Windows x64 and Intel macOS")
    }
}

fn native_curl() -> PathBuf {
    #[cfg(windows)]
    {
        let windows = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(windows).join("System32").join("curl.exe")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/usr/bin/curl")
    }
}

fn download(url: &str, destination: &Path, max_bytes: u64) -> Result<()> {
    ensure!(
        url == LATEST_RELEASE || url.starts_with(ASSET_PREFIX),
        "Refusing an unexpected update source"
    );
    let status = Command::new(native_curl())
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "20",
            "--max-time",
            "600",
            "--max-filesize",
            &max_bytes.to_string(),
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "User-Agent: eoncn-codex-updater",
            "--output",
        ])
        .arg(destination)
        .arg(url)
        .status()
        .context("Could not start the system curl downloader")?;
    ensure!(status.success(), "Downloading {url} failed ({status})");
    ensure!(
        destination.metadata()?.len() <= max_bytes,
        "Downloaded update exceeds size limit"
    );
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let len = file.read(&mut buffer)?;
        if len == 0 {
            break;
        }
        hash.update(&buffer[..len]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn safe_member_path(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "Unsafe update archive member: {}",
        path.display()
    );
    Ok(())
}

#[cfg(windows)]
fn unpack(archive: &Path, destination: &Path) -> Result<()> {
    let reader = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(reader)?;
    for i in 0..zip.len() {
        let mut member = zip.by_index(i)?;
        let path = member
            .enclosed_name()
            .context("Unsafe ZIP archive entry")?
            .to_path_buf();
        safe_member_path(&path)?;
        // Do not follow or unpack symlinks from the release.
        if let Some(mode) = member.unix_mode() {
            let file_type = mode & 0o170000;
            ensure!(
                file_type == 0 || file_type == 0o100000 || file_type == 0o040000,
                "Unsupported ZIP archive entry"
            );
        }
        let dest = destination.join(path);
        if member.is_dir() {
            fs::create_dir_all(&dest)?;
        } else {
            fs::create_dir_all(dest.parent().context("ZIP path missing parent")?)?;
            let mut output = File::create(dest)?;
            std::io::copy(&mut member, &mut output)?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn unpack(archive: &Path, destination: &Path) -> Result<()> {
    let reader = File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(reader);
    let mut tar = tar::Archive::new(decoder);
    for item in tar.entries()? {
        let mut entry = item?;
        let kind = entry.header().entry_type();
        ensure!(kind.is_file() || kind.is_dir(), "Unsafe TAR entry type");
        safe_member_path(&entry.path()?)?;
        ensure!(entry.unpack_in(destination)?, "TAR member escaped package directory");
    }
    Ok(())
}

fn package_root() -> Result<PathBuf> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let bin_dir = executable.parent().context("Executable missing parent")?;
    ensure!(
        bin_dir.file_name().is_some_and(|name| name == "bin"),
        "Updates require the complete Codex package (bin/codex), not a copied binary"
    );
    let package = bin_dir.parent().context("Package missing parent")?;
    let manifest: PackageManifest = serde_json::from_slice(
        &fs::read(package.join("codex-package.json"))
            .context("Missing codex-package.json beside bin/")?,
    )?;
    ensure!(
        manifest.target == target()?,
        "The running package architecture does not match the update target"
    );
    ensure!(
        manifest.entrypoint
            == if cfg!(windows) { "bin/codex.exe" } else { "bin/codex" },
        "Unexpected Codex package entrypoint"
    );
    Ok(package.to_path_buf())
}

fn verify_package(path: &Path, version: &str, build: u64) -> Result<()> {
    let manifest: PackageManifest =
        serde_json::from_slice(&fs::read(path.join("codex-package.json"))?)?;
    ensure!(
        manifest.version == version
            && manifest.fork_build_number == build
            && manifest.target == target()?,
        "Downloaded package metadata does not match the verified release"
    );
    let entry = if cfg!(windows) {
        "bin/codex.exe"
    } else {
        "bin/codex"
    };
    ensure!(manifest.entrypoint == entry, "Unexpected package entrypoint");
    let binary = path.join(entry);
    ensure!(binary.is_file(), "Codex executable is missing");
    for resource in [
        if cfg!(windows) {
            "bin/codex-code-mode-host.exe"
        } else {
            "bin/codex-code-mode-host"
        },
        if cfg!(windows) {
            "bin/codex-exec-server.exe"
        } else {
            "bin/codex-exec-server"
        },
        if cfg!(windows) { "codex-path/rg.exe" } else { "codex-path/rg" },
    ] {
        ensure!(path.join(resource).is_file(), "Package resource missing: {resource}");
    }
    let output = Command::new(binary).arg("--version").output()?;
    ensure!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains(version),
        "New Codex binary failed version smoke test"
    );
    Ok(())
}

fn install_unix(live: &Path, incoming: &Path, stage: &Path, version: &str, build: u64) -> Result<()> {
    let backup = stage.join("backup");
    fs::rename(live, &backup).context("Could not back up installed Codex")?;
    if let Err(error) = fs::rename(incoming, live) {
        fs::rename(&backup, live).context("Update failed and rollback also failed")?;
        return Err(error).context("Could not install staged Codex");
    }
    if let Err(error) = verify_package(live, version, build) {
        let failed = stage.join("failed");
        fs::rename(live, &failed).context("Could not move invalid updated package")?;
        fs::rename(&backup, live).context("Could not restore previous Codex")?;
        return Err(error).context("New Codex failed validation, prior package restored");
    }
    println!("Updated Codex to {version} (fork build {build}); restart Codex.");
    Ok(())
}

#[cfg(windows)]
fn install_windows(live: &Path, incoming: &Path, stage: tempfile::TempDir, version: &str) -> Result<()> {
    // Windows cannot replace its own running EXE. A trusted local helper
    // waits for this process to exit before atomically swapping directories.
    let script_path = stage.path().join("install-update.ps1");
    let log = stage.path().join("updater.log");
    let backup = stage.path().join("backup");
    let script = r#"
param(
  [int]$ParentProcessId,
  [string]$Live,
  [string]$Incoming,
  [string]$Backup,
  [string]$ExpectedVersion,
  [string]$Log
)
$ErrorActionPreference = 'Stop'
try {
  for ($i = 0; $i -lt 240; $i++) {
    if (-not (Get-Process -Id $ParentProcessId -ErrorAction SilentlyContinue)) { break }
    Start-Sleep -Milliseconds 500
  }
  if (Get-Process -Id $ParentProcessId -ErrorAction SilentlyContinue) {
    throw 'Codex did not exit; update aborted.'
  }
  $backedUp = $false
  try {
    Move-Item -LiteralPath $Live -Destination $Backup -ErrorAction Stop
    $backedUp = $true
    Move-Item -LiteralPath $Incoming -Destination $Live -ErrorAction Stop
    $exe = Join-Path $Live 'bin\codex.exe'
    $output = & $exe --version
    if ($LASTEXITCODE -ne 0 -or "$output" -notlike "*$ExpectedVersion*") {
      throw 'Updated Codex failed its version smoke test.'
    }
    [System.IO.File]::WriteAllText($Log, "Updated successfully: $output")
    Remove-Item -LiteralPath $Backup -Recurse -Force -ErrorAction SilentlyContinue
  } catch {
    if ($backedUp) {
      if (Test-Path -LiteralPath $Live) {
        Move-Item -LiteralPath $Live -Destination ($Live + '.failed-update') -ErrorAction Stop
      }
      Move-Item -LiteralPath $Backup -Destination $Live -ErrorAction Stop
    }
    throw
  }
} catch {
  [System.IO.File]::WriteAllText($Log, "Update failed: $($_.ToString())")
  exit 1
}
"#;
    fs::write(&script_path, script)?;
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let powershell = PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    Command::new(powershell)
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script_path)
        .arg("-ParentProcessId")
        .arg(std::process::id().to_string())
        .arg("-Live")
        .arg(live)
        .arg("-Incoming")
        .arg(incoming)
        .arg("-Backup")
        .arg(backup)
        .arg("-ExpectedVersion")
        .arg(version)
        .arg("-Log")
        .arg(&log)
        .spawn()
        .context("Failed to start detached Windows updater")?;
    let path = stage.keep();
    println!(
        "Verified update staged. Exit this Codex process to finish installation. Updater log: {}",
        path.join("updater.log").display()
    );
    Ok(())
}

pub fn run() -> Result<()> {
    let build = current_build()?;
    let platform = target()?;
    let live = package_root()?;
    let parent = live.parent().context("Package has no parent directory")?;
    let stage = tempfile::Builder::new()
        .prefix(".codex-fork-update-")
        .tempdir_in(parent)
        .context("Cannot stage update beside the installed package")?;

    let release_file = stage.path().join("release.json");
    download(LATEST_RELEASE, &release_file, 1024 * 1024)
        .context("Cannot query the latest eoncn/codex GitHub Release")?;
    let release: GithubRelease = serde_json::from_slice(&fs::read(&release_file)?)?;
    let (release_version, release_build) = parse_release_tag(&release.tag_name)?;
    let manifest_file = stage.path().join("codex-update.json");
    download(
        release_asset(&release, "codex-update.json")?,
        &manifest_file,
        1024 * 1024,
    )?;
    let manifest: UpdateManifest = serde_json::from_slice(&fs::read(&manifest_file)?)?;
    ensure!(
        manifest.schema == 1
            && manifest.version == release_version.to_string()
            && manifest.build == release_build,
        "GitHub Release tag and update manifest disagree"
    );

    let current_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    if (release_version.clone(), release_build) <= (current_version, build) {
        println!("Your forked Codex is already up to date.");
        return Ok(());
    }

    let asset = manifest
        .assets
        .get(platform)
        .context("No update package exists for this platform")?;
    let expected_name = if cfg!(windows) {
        "codex-windows-x64.zip"
    } else {
        "codex-macos-intel.tar.gz"
    };
    ensure!(asset.name == expected_name, "Unexpected update asset filename");
    ensure!(
        asset.sha256.len() == 64
            && asset.sha256.chars().all(|ch| ch.is_ascii_hexdigit()),
        "Invalid release SHA-256"
    );

    let archive = stage.path().join(expected_name);
    println!("Downloading Codex {} (fork build {})...", manifest.version, manifest.build);
    download(
        release_asset(&release, expected_name)?,
        &archive,
        MAX_ARCHIVE_BYTES,
    )?;
    ensure!(
        sha256_file(&archive)?.eq_ignore_ascii_case(&asset.sha256),
        "SHA-256 verification failed; update was not installed"
    );

    let incoming = stage.path().join("incoming");
    fs::create_dir(&incoming)?;
    unpack(&archive, &incoming)?;
    verify_package(&incoming, &manifest.version, manifest.build)?;

    #[cfg(windows)]
    return install_windows(&live, &incoming, stage, &manifest.version);

    #[cfg(not(windows))]
    return install_unix(&live, &incoming, stage.path(), &manifest.version, manifest.build);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_tag_must_include_numeric_build() {
        let (version, build) = parse_release_tag("fork-v0.167.0.3").unwrap();
        assert_eq!(version.to_string(), "0.167.0");
        assert_eq!(build, 3);
        assert!(parse_release_tag("rust-v0.167.0").is_err());
        assert!(parse_release_tag("fork-v0.167.0.xyz").is_err());
    }

    #[test]
    fn archive_paths_cannot_escape_installation() {
        assert!(safe_member_path(Path::new("bin/codex")).is_ok());
        assert!(safe_member_path(Path::new("../escape")).is_err());
        assert!(safe_member_path(Path::new("/etc/passwd")).is_err());
    }
}
