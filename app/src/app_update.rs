//! Updates the portable Windows executable or Linux AppImage from GitHub Releases.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

const RELEASES: &str = "https://api.github.com/repos/sasin91/jobfinder/releases/latest";
const DOWNLOADS: &str = "https://github.com/sasin91/jobfinder/releases/download";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const STAGE_PREFIX: &str = ".jobfinder-update-";

fn asset_name() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("jobfinder.exe")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("JobFinder-x86_64.AppImage")
    } else {
        None
    }
}

pub fn supported() -> bool {
    asset_name().is_some() && (cfg!(windows) || std::env::var_os("APPIMAGE").is_some_and(|path| !path.is_empty()))
}

#[derive(Debug, Clone)]
pub struct Update {
    pub version: Version,
    name: &'static str,
    url: String,
    digest: String,
    size: u64,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    digest: Option<String>,
    size: u64,
}

fn newer_release(release: Release, current: &Version, name: &'static str) -> Result<Option<Update>, String> {
    let tag = release.tag_name;
    let version = Version::parse(tag.strip_prefix('v').unwrap_or(&tag))
        .map_err(|_| format!("GitHub returned an invalid JobFinder version ({tag})."))?;
    if !version.pre.is_empty() || !version.cmp_precedence(current).is_gt() {
        return Ok(None);
    }
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == name && asset.size > 0)
        .ok_or_else(|| format!("JobFinder {version} has no download for this platform."))?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| format!("JobFinder {version} has no valid SHA-256 digest."))?;
    Ok(Some(Update {
        version,
        name,
        url: format!("{DOWNLOADS}/{tag}/{name}"),
        digest: digest.to_ascii_lowercase(),
        size: asset.size,
    }))
}

fn client(timeout: Duration) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .user_agent(concat!("JobFinder/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .build()
}

pub async fn check() -> Result<Option<Update>, String> {
    if !supported() {
        return Err("In-app updates are available for the Windows release and the Linux AppImage.".into());
    }
    async {
        client(Duration::from_secs(30))?
            .get(RELEASES)
            .send()
            .await?
            .error_for_status()?
            .json::<Release>()
            .await
    }
    .await
    .map_err(|e| format!("Couldn't check GitHub for updates. {e}"))
    .and_then(|release| {
        newer_release(
            release,
            &Version::parse(VERSION).expect("Cargo package version"),
            asset_name().unwrap(),
        )
    })
}

pub struct Staged {
    folder: tempfile::TempDir,
    file: PathBuf,
    program: PathBuf,
}

pub async fn download(update: Update) -> Result<Staged, String> {
    let program = crate::scan_schedule::running_program()
        .and_then(|path| path.canonicalize())
        .map_err(|e| format!("Couldn't locate the JobFinder program. {e}"))?;
    let parent = program.parent().ok_or("Couldn't locate the JobFinder folder.")?;
    let folder = tempfile::Builder::new()
        .prefix(STAGE_PREFIX)
        .tempdir_in(parent)
        .map_err(|e| format!("Couldn't prepare an update in {}. {e}", parent.display()))?;
    let file = folder.path().join(update.name);
    let mut output = tokio::fs::File::create_new(&file).await.map_err(write_error)?;
    let mut response = async {
        client(Duration::from_secs(15 * 60))?
            .get(&update.url)
            .send()
            .await?
            .error_for_status()
    }
    .await
    .map_err(download_error)?;
    let mut digest = Sha256::new();
    let mut size = 0;
    while let Some(chunk) = response.chunk().await.map_err(download_error)? {
        size += chunk.len() as u64;
        if size > update.size {
            break;
        }
        digest.update(&chunk);
        output.write_all(&chunk).await.map_err(write_error)?;
    }
    if size != update.size || format!("{:x}", digest.finalize()) != update.digest {
        return Err("The download didn't match the published size and checksum. Try downloading it again.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::metadata(&program).map_err(write_error)?.permissions();
        output
            .set_permissions(std::fs::Permissions::from_mode(permissions.mode() & 0o777))
            .await
            .map_err(write_error)?;
    }
    output.sync_all().await.map_err(write_error)?;
    drop(output);
    Ok(Staged { folder, file, program })
}

fn download_error(e: reqwest::Error) -> String {
    format!("Couldn't download the JobFinder update. {}", e.without_url())
}

fn write_error(e: std::io::Error) -> String {
    format!("Couldn't save the JobFinder update. {e}")
}

fn restart(program: &Path, args: impl IntoIterator<Item = std::ffi::OsString>) -> std::io::Result<()> {
    let mut command = Command::new(program);
    command.args(args);
    #[cfg(target_os = "linux")]
    {
        // The old AppImage may unmount or remove its extraction directory when it exits.
        command
            .env_remove("APPIMAGE")
            .env_remove("APPDIR")
            .env_remove("ARGV0")
            .env_remove("OWD");
        if let Some(cwd) = std::env::var_os("OWD") {
            command.current_dir(cwd);
        }
        if crate::scan_schedule::extracted_appimage() {
            command.env("APPIMAGE_EXTRACT_AND_RUN", "1");
        }
    }
    crate::no_window::hide_std(&mut command).spawn()?;
    Ok(())
}

impl Staged {
    /// Called after the window, services and database locks have shut down.
    pub fn apply(self) -> Result<(), String> {
        let args = std::env::args_os().skip(1);
        #[cfg(windows)]
        {
            // Windows locks the running program. The verified new copy waits for this process
            // to exit, renames itself over the old copy, and restarts from the original path.
            let mut helper = Command::new(&self.file);
            helper
                .arg(FINISH_UPDATE)
                .arg(std::process::id().to_string())
                .arg(&self.program)
                .args(args);
            crate::no_window::hide_std(&mut helper)
                .spawn()
                .map_err(|e| format!("Couldn't start the JobFinder updater. {e}"))?;
            let _ = self.folder.keep();
            Ok(())
        }
        #[cfg(not(windows))]
        {
            std::fs::rename(&self.file, &self.program).map_err(install_error)?;
            drop(self.folder);
            restart(&self.program, args).map_err(restart_error)
        }
    }
}

fn install_error(e: std::io::Error) -> String {
    format!("Couldn't replace the JobFinder program. {e}")
}

fn restart_error(e: std::io::Error) -> String {
    format!("Couldn't reopen JobFinder. Open the program again to continue. {e}")
}

/// Keep this handoff compatible with older releases, which run the new Windows EXE to install it.
#[cfg(windows)]
pub const FINISH_UPDATE: &str = "--finish-update";

#[cfg(windows)]
pub fn finish_windows_update() -> Result<(), String> {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

    let mut args = std::env::args_os().skip(2);
    let pid = args
        .next()
        .and_then(|pid| pid.to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != 0 && *pid != std::process::id())
        .ok_or("The update is missing the original process ID.")?;
    let program = PathBuf::from(args.next().ok_or("The update is missing the program path.")?);
    let file = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(install_error)?;
    let folder = file
        .parent()
        .filter(|folder| {
            folder.parent() == program.parent()
                && folder
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(STAGE_PREFIX))
        })
        .ok_or("The update is outside its staging folder.")?;

    // SAFETY: the handle is opened with synchronization access and closed after waiting.
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(process) => {
                let waited = WaitForSingleObject(process, 30_000);
                let _ = CloseHandle(process);
                if waited != WAIT_OBJECT_0 {
                    return Err(
                        "JobFinder didn't close in time for the update. Open the original program and try again."
                            .into(),
                    );
                }
            }
            // A process that exited before the helper opened its handle is already gone.
            Err(e) if e.code() == windows::Win32::Foundation::ERROR_INVALID_PARAMETER.to_hresult() => {}
            Err(e) => return Err(format!("Couldn't wait for JobFinder to close. {e}")),
        }
    }
    std::fs::rename(&file, &program).map_err(install_error)?;
    let _ = std::fs::remove_dir(folder);
    restart(&program, args).map_err(restart_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS: &str = "jobfinder.exe";

    fn release(tag: &str, name: &str, digest: &str) -> Release {
        Release {
            tag_name: tag.into(),
            assets: vec![Asset {
                name: name.into(),
                digest: Some(digest.into()),
                size: 42,
            }],
        }
    }

    #[test]
    fn only_offers_a_newer_stable_version() {
        let current = Version::parse("0.1.9").unwrap();
        for tag in ["v0.1.8", "v0.1.9", "v0.1.9+rebuilt", "v0.1.10-rc.1"] {
            assert!(
                newer_release(release(tag, WINDOWS, ""), &current, WINDOWS)
                    .unwrap()
                    .is_none()
            );
        }
        let digest = format!("sha256:{}", "A".repeat(64));
        let update = newer_release(release("v0.1.10", WINDOWS, &digest), &current, WINDOWS)
            .unwrap()
            .unwrap();
        assert_eq!(update.version.to_string(), "0.1.10");
        assert_eq!(update.url, format!("{DOWNLOADS}/v0.1.10/jobfinder.exe"));
        assert_eq!(update.digest, "a".repeat(64));
    }

    #[test]
    fn requires_the_platform_asset_and_a_sha256_digest() {
        let current = Version::parse("0.1.9").unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(
            newer_release(
                release("v0.1.10", WINDOWS, &digest),
                &current,
                "JobFinder-x86_64.AppImage"
            )
            .is_err()
        );
        assert!(newer_release(release("v0.1.10", WINDOWS, "sha256:123"), &current, WINDOWS).is_err());
        assert!(newer_release(release("v0.1.10/path", WINDOWS, &digest), &current, WINDOWS).is_err());
    }
}
