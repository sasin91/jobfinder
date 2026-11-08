//! Recordings: a call, a voice message or a spoken note about a saved match, turned into text on
//! this computer by Whisper (whisper), and kept with its timed lines (recording_model). The audio
//! file is kept in the database's own folder under recordings, next to it (folder_for), so it can be played again; Delete
//! removes both.
//!
//! A recording comes in as a file picked on the match page (recording_service::add_file). The
//! phone's call recorder's files reach the computer by Taildrop; picking them up from there is
//! the next step.

pub mod audio;
pub mod recording_model;
pub mod recording_row;
pub mod recording_service;

use std::path::{Path, PathBuf};

/// The recordings folder's name next to the database.
pub const FOLDER: &str = "recordings";

/// The single local database keeps its existing recordings/jobfinder folder. Development
/// databases use their complete filename under recordings/databases, so an override cannot
/// collide with the normal database or with another override's extension.
pub fn folder_for(database: &Path) -> PathBuf {
    let data_folder = database.parent().unwrap_or(Path::new("."));
    let name = database.file_name().unwrap_or(database.as_os_str());
    if name == crate::db::FILE {
        data_folder.join(FOLDER).join("jobfinder")
    } else {
        data_folder.join(FOLDER).join("databases").join(name)
    }
}

/// Copies a development database's existing audio out of the old stem-based folder. Copy
/// only files referenced by this database and leave the source alone: it may have been shared
/// with the normal database. Existing destinations are never overwritten, including on retry.
pub async fn prepare_folder(db: &sqlx::SqlitePool, database: &Path) -> Result<PathBuf, String> {
    let folder = folder_for(database);
    let old = database
        .parent()
        .unwrap_or(Path::new("."))
        .join(FOLDER)
        .join(database.file_stem().unwrap_or(database.as_os_str()));
    if old == folder || !old.is_dir() {
        return Ok(folder);
    }
    let names: Vec<String> = sqlx::query_scalar("SELECT content_url FROM recordings")
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    for name in names {
        if Path::new(&name).file_name() != Some(std::ffi::OsStr::new(&name)) {
            return Err("A recording has an invalid filename.".to_string());
        }
        let from = old.join(&name);
        if !from.is_file() {
            continue;
        }
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let to = folder.join(&name);
        let mut output = match std::fs::OpenOptions::new().write(true).create_new(true).open(&to) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        };
        let copied = std::fs::File::open(&from).and_then(|mut input| std::io::copy(&mut input, &mut output));
        if let Err(e) = copied {
            drop(output);
            let _ = std::fs::remove_file(&to);
            return Err(e.to_string());
        }
    }
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn development_audio_is_copied_without_changing_shared_originals() {
        let root = std::env::temp_dir().join(format!("jobfinder-recording-migration-{}", std::process::id()));
        let database = root.join("jobfinder.db");
        let old = root.join(FOLDER).join("jobfinder");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("mine.wav"), b"mine").unwrap();
        std::fs::write(old.join("other.wav"), b"other").unwrap();
        let db = crate::db::open_in_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO recordings (kind, source, content_url, upload_date) VALUES ('call', 'file', 'mine.wav', 1)",
        )
        .execute(&db)
        .await
        .unwrap();
        let folder = prepare_folder(&db, &database).await.unwrap();
        assert_eq!(std::fs::read(folder.join("mine.wav")).unwrap(), b"mine");
        assert!(!folder.join("other.wav").exists());
        std::fs::write(folder.join("mine.wav"), b"new").unwrap();
        prepare_folder(&db, &database).await.unwrap();
        assert_eq!(std::fs::read(folder.join("mine.wav")).unwrap(), b"new");
        assert_eq!(std::fs::read(old.join("mine.wav")).unwrap(), b"mine");
        assert_eq!(std::fs::read(old.join("other.wav")).unwrap(), b"other");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn each_database_keeps_its_recordings_apart() {
        let main = folder_for(Path::new("/home/me/jobfinder.sqlite"));
        let other = folder_for(Path::new("/home/me/test.sqlite"));
        assert_eq!(main, Path::new("/home/me/recordings/jobfinder"));
        assert_eq!(other, Path::new("/home/me/recordings/databases/test.sqlite"));
        assert_ne!(main, folder_for(Path::new("/home/me/jobfinder.db")));
        assert_ne!(main, folder_for(Path::new("/home/me/jobfinder")));
        assert_ne!(other, folder_for(Path::new("/home/me/test.db")));
    }
}
