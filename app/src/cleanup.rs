//! `jobfinder cleanup`: what the app leaves outside its program file, removed. The program itself
//! is never installed, so removing JobFinder is deleting its file; this takes away the rest, after
//! saying what it found:
//!
//! - the job scan's schedule (Task Scheduler's task, or the systemd user timer), which would
//!   otherwise keep running a program that is gone;
//! - what the app kept in Windows Credential Manager (or the keyring) before its secrets table,
//!   when any is left there (account::former_keys);
//! - JobFinder's name for Windows' notifications, in the user's registry (notifications);
//! - what the app wrote or downloaded next to the database: the Jobnet extension's files,
//!   llama.cpp and its models, whisper.cpp and its model, Laya's model;
//! - and only when asked again, the database itself, with its -wal and -shm files, its key file
//!   (account::data_key), the recordings' audio files (recordings) and the app's log (app_log)
//!   beside it. The API keys and the paired
//!   devices' tokens are in the database, so they go with it.
//!
//! The data folder itself goes too when it is the default one (%APPDATA%\JobFinder) and nothing
//! else is left in it; a folder that --database names is never removed, only the app's own
//! files in it.

use std::path::{Path, PathBuf};

use crate::account::former_keys::{self, Credential, FormerKeys};
use crate::account::key_vault::Secret;
use crate::devices::device_model::{self, Device};
use crate::scan_schedule::ScanSchedule;
use crate::{app_log, extension, laya, llama};

/// What the cleanup found.
pub struct Found {
    pub database: PathBuf,
    /// The job scan's interval, when it is scheduled.
    pub schedule: Option<u32>,
    /// The schedule couldn't be read.
    pub schedule_error: Option<String>,
    /// What is left in Credential Manager (the keyring) from before the secrets table.
    pub left_in_credentials: Vec<Credential>,
    /// Credential Manager (the keyring) couldn't be read.
    pub credentials_error: Option<String>,
    /// The paired devices, whose tokens may be left there too.
    pub devices: Vec<Device>,
    pub devices_error: Option<String>,
    /// The folders the app wrote next to the database that are there.
    pub folders: Vec<PathBuf>,
    /// The database's files that are there.
    pub database_files: Vec<PathBuf>,
    /// JobFinder's name for Windows notifications is in the registry (notifications).
    pub notification_name: bool,
}

impl Found {
    /// Whether anything but the database is there to remove.
    pub fn anything_besides_the_database(&self) -> bool {
        self.schedule.is_some()
            || !self.left_in_credentials.is_empty()
            || !self.folders.is_empty()
            || self.notification_name
    }
}

/// The folders the app writes next to database.
pub fn app_folders(database: &Path) -> Vec<PathBuf> {
    let data_folder = database.parent().unwrap_or(Path::new("."));
    [
        extension::FOLDER,
        llama::FOLDER,
        crate::whisper::FOLDER,
        laya::laya_client::FOLDER,
        crate::job_sites::signed_in::FOLDER,
    ]
    .iter()
    .map(|name| data_folder.join(name))
    .collect()
}

/// Another JobFinder holds the database's lock: it is running on it.
#[derive(Debug, PartialEq)]
pub struct InUse;

/// The files whose locks a running JobFinder holds for database:
/// - the database's own, its name and -lock beside the file itself, so a link to the database
///   shares it;
/// - its folder's, `jobfinder-folder.lock`, since every database in a folder shares the
///   llama, laya and jobnet-joblog folders there.
pub fn lock_files(database: &Path) -> [PathBuf; 2] {
    let file = std::fs::canonicalize(database).unwrap_or_else(|_| database.to_path_buf());
    let mut name = file.into_os_string();
    name.push("-lock");
    let folder = database
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    [PathBuf::from(name), folder.join(FOLDER_LOCK)]
}

/// The folder lock's file name.
const FOLDER_LOCK: &str = "jobfinder-folder.lock";

/// How a lock on the database is held.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hold {
    /// The app while it runs: any number of apps share it (a dev build beside the release one).
    Running,
    /// The cleanup: alone, so it starts only when no app runs on the database or another one in
    /// its folder, and no app starts there while it runs.
    Cleanup,
}

/// The locks hold() took, held until this is dropped.
pub struct Held {
    _files: Vec<std::fs::File>,
}

/// Takes database's locks (lock_files). A lock file that can't be made or locked at all (a
/// read-only folder, say) is skipped, which doesn't stop either.
pub fn hold(database: &Path, how: Hold) -> Result<Held, InUse> {
    if let Some(folder) = database.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let mut files = Vec::new();
    for path in lock_files(database) {
        let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
        else {
            continue;
        };
        let locked = match how {
            Hold::Running => file.try_lock_shared(),
            Hold::Cleanup => file.try_lock(),
        };
        match locked {
            Ok(()) => files.push(file),
            Err(std::fs::TryLockError::WouldBlock) => return Err(InUse),
            Err(std::fs::TryLockError::Error(_)) => {}
        }
    }
    Ok(Held { _files: files })
}

/// The database's own files: the file, SQLite's -wal, -shm and -journal beside it, its key file,
/// and the app's log in its folder (the lock file goes after the cleanup lets go of it).
pub fn database_files(database: &Path) -> Vec<PathBuf> {
    let mut files = vec![database.to_path_buf()];
    for suffix in [
        "-wal",
        "-shm",
        "-journal",
        crate::job_scan::scan_lock::LOCK_SUFFIX,
        crate::job_scan::scan_lock::STOP_SUFFIX,
        crate::job_scan::scan_lock::PAUSE_SUFFIX,
    ] {
        let mut name = database.as_os_str().to_owned();
        name.push(suffix);
        files.push(PathBuf::from(name));
    }
    files.push(crate::account::data_key::key_file(database));
    let data_folder = database.parent().unwrap_or(Path::new("."));
    files.extend([app_log::FILE, app_log::OLD_FILE, app_log::LOCK_FILE].map(|name| data_folder.join(name)));
    files
}

/// Whether the cleanup looks in Credential Manager (the keyring) for what is left there.
pub enum Credentials {
    /// It looks; former_user_id is the local user's id while the database had trongate_users,
    /// which named its keys' credentials then.
    LookIn { former_user_id: Option<i64> },
    /// It doesn't (tests, which mustn't touch the system's store).
    Skip,
}

/// Looks for what the app left for database.
pub async fn find(database: &Path, schedule: &ScanSchedule, credentials: Credentials) -> Found {
    let (schedule, schedule_error) = match schedule.interval_hours().await {
        Ok(hours) => (hours, None),
        Err(why) => (None, Some(why)),
    };
    let (devices, devices_error) = match device_model::in_file(database).await {
        Ok(devices) => (devices, None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let (left_in_credentials, credentials_error) = match credentials {
        Credentials::LookIn { former_user_id } => {
            let device_ids = devices.iter().map(|device| device.id).collect();
            match FormerKeys::of(database, former_user_id, device_ids).left() {
                Ok(left) => (left, None),
                Err(why) => (Vec::new(), Some(why)),
            }
        }
        Credentials::Skip => (Vec::new(), None),
    };
    Found {
        database: database.to_path_buf(),
        schedule,
        schedule_error,
        left_in_credentials,
        credentials_error,
        devices,
        devices_error,
        folders: app_folders(database)
            .into_iter()
            .filter(|folder| folder.exists())
            .collect(),
        notification_name: crate::notifications::system_name_kept(),
        database_files: database_files(database)
            .into_iter()
            .filter(|file| file.exists())
            .collect(),
    }
}

/// Removes the schedule, what is left in the credentials and the app's folders; what couldn't be
/// removed, said why.
pub async fn remove_besides_the_database(found: &Found, schedule: &ScanSchedule) -> Vec<String> {
    let mut failed = Vec::new();
    if found.schedule.is_some()
        && let Err(why) = schedule.remove().await
    {
        failed.push(format!("The job scan's schedule: {why}"));
    }
    for credential in &found.left_in_credentials {
        if let Err(why) = former_keys::remove(credential) {
            failed.push(format!("{}: {why}", left_name(&credential.secret, &found.devices)));
        }
    }
    for folder in &found.folders {
        if let Err(e) = std::fs::remove_dir_all(folder) {
            failed.push(format!("{}: {e}", folder.display()));
        }
    }
    if found.notification_name
        && let Err(why) = crate::notifications::forget_system_name()
    {
        failed.push(format!("JobFinder's name for Windows notifications: {why}"));
    }
    failed
}

/// Removes the database's files, and its own recordings folder (recordings::folder_for; their
/// audio is the user's, like the database), with the recordings folder itself once no other
/// database's are left in it; what couldn't be removed, said why.
pub fn remove_database(found: &Found) -> Vec<String> {
    let mut failed: Vec<String> = found
        .database_files
        .iter()
        .filter_map(|file| {
            std::fs::remove_file(file)
                .err()
                .map(|e| format!("{}: {e}", file.display()))
        })
        .collect();
    let recordings = crate::recordings::folder_for(&found.database);
    if recordings.is_dir() {
        if let Err(e) = std::fs::remove_dir_all(&recordings) {
            failed.push(format!("{}: {e}", recordings.display()));
        }
        // remove_dir only removes an empty folder: another database's recordings keep it.
        if let Some(shared) = recordings.parent() {
            let _ = std::fs::remove_dir(shared);
        }
    }
    failed
}

/// Removes the data folder when it is default_folder and empty; whether it did.
pub fn remove_default_folder(database: &Path, default_folder: &Path) -> bool {
    let Some(folder) = database.parent() else {
        return false;
    };
    // remove_dir only removes an empty folder.
    same_folder(folder, default_folder) && std::fs::remove_dir(folder).is_ok()
}

fn same_folder(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// What the cleanup calls a secret left in a credential: "the OpenAI key", "the token of the
/// paired device \"Pixel 8\"".
pub fn left_name(secret: &Secret<'_>, devices: &[Device]) -> String {
    match secret {
        Secret::LlmKey(provider) => format!("the {} key", crate::account::account_model::provider_label(provider)),
        Secret::KeyFromBeforeProviders => "the earlier key".to_string(),
        Secret::DeviceTokenHash(id) => match devices.iter().find(|device| device.id == *id) {
            Some(device) => format!("the token of the paired device \"{}\"", device.name),
            None => format!("the token of paired device {id}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_database_files_are_the_file_sqlites_own_and_the_log_beside_it() {
        let files = database_files(Path::new("/data/jobfinder.sqlite"));
        assert_eq!(
            files,
            [
                "/data/jobfinder.sqlite",
                "/data/jobfinder.sqlite-wal",
                "/data/jobfinder.sqlite-shm",
                "/data/jobfinder.sqlite-journal",
                "/data/jobfinder.sqlite.scan.lock",
                "/data/jobfinder.sqlite.scan.stop",
                "/data/jobfinder.sqlite.scan.pause",
                "/data/jobfinder.sqlite-key",
                "/data/jobfinder.log",
                "/data/jobfinder.old.log",
                "/data/jobfinder.log.lock"
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn running_apps_share_the_lock_and_the_cleanup_needs_it_alone() {
        let folder = std::env::temp_dir().join(format!("jobfinder-lock-{}", std::process::id()));
        let database = folder.join("jobfinder.sqlite");
        let app = hold(&database, Hold::Running).unwrap();

        let dev_build = hold(&database, Hold::Running).expect("a second app on the same database starts");

        assert_eq!(hold(&database, Hold::Cleanup).err(), Some(InUse));
        drop(app);
        assert_eq!(
            hold(&database, Hold::Cleanup).err(),
            Some(InUse),
            "the dev build still runs"
        );
        drop(dev_build);
        let cleanup = hold(&database, Hold::Cleanup).unwrap();
        assert_eq!(
            hold(&database, Hold::Running).err(),
            Some(InUse),
            "no app starts during the cleanup"
        );
        drop(cleanup);

        // A link to the database shares its lock.
        #[cfg(unix)]
        {
            std::fs::write(&database, "db").unwrap();
            let alias = folder.join("alias.sqlite");
            std::os::unix::fs::symlink(&database, &alias).unwrap();
            let app = hold(&database, Hold::Running).unwrap();

            assert_eq!(hold(&alias, Hold::Cleanup).err(), Some(InUse));
            // Another database in the same folder shares its folders, so its cleanup waits too.
            assert_eq!(hold(&folder.join("test.sqlite"), Hold::Cleanup).err(), Some(InUse));
            drop(app);
            assert!(hold(&folder.join("test.sqlite"), Hold::Cleanup).is_ok());
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn only_the_apps_own_folders_are_removed_from_a_named_folder() {
        assert_eq!(
            app_folders(Path::new("/home/me/jobfinder.sqlite")),
            [
                "/home/me/jobnet-joblog",
                "/home/me/llama",
                "/home/me/whisper",
                "/home/me/laya",
                "/home/me/signed-in-browser"
            ]
            .map(PathBuf::from)
        );
    }

    #[tokio::test]
    async fn the_cleanup_removes_the_schedule_and_the_folders_and_keeps_the_database() {
        let folder = std::env::temp_dir().join(format!("jobfinder-cleanup-{}", std::process::id()));
        std::fs::create_dir_all(folder.join("llama/models")).unwrap();
        std::fs::write(folder.join("llama/models/a.gguf"), "x").unwrap();
        let database = folder.join("jobfinder.sqlite");
        std::fs::write(&database, "db").unwrap();
        std::fs::create_dir_all(folder.join("mine")).unwrap();
        let recordings = crate::recordings::folder_for(&database);
        let others_recordings = crate::recordings::folder_for(&folder.join("test.sqlite"));
        for (recordings, file) in [(&recordings, "call.m4a"), (&others_recordings, "other.m4a")] {
            std::fs::create_dir_all(recordings).unwrap();
            std::fs::write(recordings.join(file), "sound").unwrap();
        }

        let schedule = ScanSchedule::in_memory();
        schedule.register(4).await.unwrap();
        let key_file = crate::account::data_key::key_file(&database);
        std::fs::write(&key_file, "key").unwrap();

        let found = find(&database, &schedule, Credentials::Skip).await;
        assert_eq!(found.schedule, Some(4));
        assert_eq!(found.folders, [folder.join("llama")]);
        assert_eq!(found.database_files, [database.clone(), key_file.clone()]);
        assert!(found.anything_besides_the_database());

        assert!(remove_besides_the_database(&found, &schedule).await.is_empty());
        assert_eq!(schedule.interval_hours().await.unwrap(), None);
        assert!(!folder.join("llama").exists());
        assert!(database.exists(), "the database stays unless asked again");
        assert!(key_file.exists(), "its key file stays with it");
        assert!(folder.join("mine").exists(), "nothing else in the folder is touched");

        assert!(remove_database(&found).is_empty());
        assert!(!database.exists());
        assert!(!key_file.exists(), "the key file goes with the database");
        assert!(!recordings.exists(), "its recordings go with the database");
        assert!(
            others_recordings.join("other.m4a").exists(),
            "another database's recordings in the same folder stay"
        );
        assert!(!remove_default_folder(&database, Path::new("/elsewhere")));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[tokio::test]
    async fn the_paired_devices_are_named_by_their_names() {
        let folder = std::env::temp_dir().join(format!("jobfinder-cleanup-devices-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let database = folder.join("jobfinder.sqlite");
        let db = crate::db::open(&database).await.unwrap();
        let id = device_model::add(&db, "Pixel 8", 1).await.unwrap();
        db.close().await;

        let found = find(&database, &ScanSchedule::in_memory(), Credentials::Skip).await;
        assert_eq!(found.devices_error, None);
        assert_eq!(
            left_name(&Secret::DeviceTokenHash(id), &found.devices),
            "the token of the paired device \"Pixel 8\""
        );
        assert_eq!(left_name(&Secret::LlmKey("openai"), &[]), "the OpenAI key");
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
