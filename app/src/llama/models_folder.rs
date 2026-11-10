//! Where the GGUF models are kept: `models` in the llama folder, unless the user picked another
//! folder on the Local model screen (a bigger disk, say). The pick is kept in a small file in the
//! llama folder, models-folder.txt, holding the folder's path; no file, or an empty one, is the
//! default.
//!
//! Picking a folder moves the models there: each .gguf file is renamed when the folder is on the
//! same disk, else copied (through name.part, so a broken copy never looks like a model) and the
//! original removed once every file is across. A failure puts back what was moved.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use super::llama_server::MODELS_FOLDER;

/// The file in the llama folder that names the models folder.
pub const SETTING_FILE: &str = "models-folder.txt";

/// A model file's extension.
const GGUF_EXTENSION: &str = "gguf";

/// What a copy is written as until it is whole.
const PART_SUFFIX: &str = ".part";

/// The models folder for llama_folder: the one models-folder.txt names, else `models` in it.
pub fn read(llama_folder: &Path) -> PathBuf {
    let named = std::fs::read_to_string(llama_folder.join(SETTING_FILE)).unwrap_or_default();
    let named = named.trim();
    if named.is_empty() {
        default_for(llama_folder)
    } else {
        PathBuf::from(named)
    }
}

/// `models` in the llama folder.
pub fn default_for(llama_folder: &Path) -> PathBuf {
    llama_folder.join(MODELS_FOLDER)
}

/// Keeps to as the models folder in models-folder.txt; the default removes the file.
fn write(llama_folder: &Path, to: &Path) -> io::Result<()> {
    let setting = llama_folder.join(SETTING_FILE);
    if to == default_for(llama_folder) {
        return match std::fs::remove_file(&setting) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    }
    std::fs::create_dir_all(llama_folder)?;
    std::fs::write(setting, to.to_string_lossy().as_bytes())
}

/// Moves every model in from to the folder to, then keeps to as the models folder; says how many
/// models moved. Reports the file name, one-based position and total before each file is moved.
/// Blocking: run it on a blocking thread.
pub fn move_to(
    llama_folder: &Path,
    from: &Path,
    to: &Path,
    mut report: impl FnMut(&OsStr, usize, usize),
) -> Result<usize, String> {
    std::fs::create_dir_all(to).map_err(|e| format!("Couldn't make {}: {e}", to.display()))?;
    let files = models_in(from).map_err(|e| format!("Couldn't read {}: {e}", from.display()))?;
    let mut moved = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let name = file.file_name().unwrap_or_default();
        let target = to.join(name);
        if target.exists() {
            put_back(&moved);
            return Err(format!(
                "{} already has {}. Move or delete that file first.",
                to.display(),
                target.display()
            ));
        }
        report(name, index + 1, files.len());
        match move_file(file, &target) {
            Ok(how) => moved.push((file.clone(), target, how)),
            Err(e) => {
                put_back(&moved);
                return Err(format!("Couldn't move {} to {}: {e}", file.display(), to.display()));
            }
        }
    }
    if let Err(e) = write(llama_folder, to) {
        put_back(&moved);
        return Err(format!("Couldn't remember the models folder: {e}"));
    }
    remove_copied_originals(&moved);
    Ok(moved.len())
}

/// How a file got to its new folder.
#[derive(Clone, Copy, PartialEq)]
enum Moved {
    Renamed,
    /// Copied: the original is still there until every file is across.
    Copied,
}

/// The .gguf files directly in folder; none when the folder isn't there.
fn models_in(folder: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let is_model = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case(GGUF_EXTENSION));
        if is_model && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Renames from to to, or copies it there when they are on different disks.
fn move_file(from: &Path, to: &Path) -> io::Result<Moved> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(Moved::Renamed);
    }
    let mut part = to.as_os_str().to_os_string();
    part.push(PART_SUFFIX);
    let part = PathBuf::from(part);
    let copied = std::fs::copy(from, &part).and_then(|_| std::fs::rename(&part, to));
    if copied.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    copied.map(|()| Moved::Copied)
}

/// Undoes moved: renames back, and removes copies (their originals are still there).
fn put_back(moved: &[(PathBuf, PathBuf, Moved)]) {
    for (original, target, how) in moved {
        let undone = match how {
            Moved::Renamed => std::fs::rename(target, original),
            Moved::Copied => std::fs::remove_file(target),
        };
        if let Err(e) = undone {
            crate::log_line!(
                "llama: couldn't put back {} after a failed move: {e}",
                original.display()
            );
        }
    }
}

/// Removes the originals of the copied files, now that the new folder is the one in use.
fn remove_copied_originals(moved: &[(PathBuf, PathBuf, Moved)]) {
    for (original, _, how) in moved {
        if *how == Moved::Copied
            && let Err(e) = std::fs::remove_file(original)
        {
            crate::log_line!("llama: {} was copied but couldn't be removed: {e}", original.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("jobfinder-models-folder-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn the_models_move_and_the_folder_is_remembered() {
        let llama = scratch("move");
        let from = default_for(&llama);
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("a.gguf"), b"a").unwrap();
        std::fs::write(from.join("notes.txt"), b"not a model").unwrap();
        assert_eq!(read(&llama), from, "the default without a setting");

        let to = llama.join("big disk");
        let mut progress = Vec::new();
        assert_eq!(
            move_to(&llama, &from, &to, |name, index, total| {
                progress.push((name.to_os_string(), index, total));
            }),
            Ok(1)
        );
        assert_eq!(progress, vec![("a.gguf".into(), 1, 1)]);
        assert_eq!(std::fs::read(to.join("a.gguf")).unwrap(), b"a");
        assert!(!from.join("a.gguf").exists());
        assert!(from.join("notes.txt").exists(), "only models move");
        assert_eq!(read(&llama), to);

        assert_eq!(move_to(&llama, &to, &from, |_, _, _| {}), Ok(1));
        assert!(!llama.join(SETTING_FILE).exists(), "back to the default");
        assert_eq!(read(&llama), from);
    }

    #[test]
    fn a_file_already_there_stops_the_move_and_puts_back_what_moved() {
        let llama = scratch("clash");
        let from = default_for(&llama);
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("a.gguf"), b"a").unwrap();
        std::fs::write(from.join("b.gguf"), b"b").unwrap();
        let to = llama.join("other");
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("b.gguf"), b"theirs").unwrap();

        assert!(move_to(&llama, &from, &to, |_, _, _| {}).is_err());
        assert!(from.join("a.gguf").exists() && from.join("b.gguf").exists());
        assert!(!to.join("a.gguf").exists());
        assert_eq!(std::fs::read(to.join("b.gguf")).unwrap(), b"theirs");
        assert_eq!(read(&llama), from);
    }
}
