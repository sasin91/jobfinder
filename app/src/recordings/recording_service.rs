//! The recordings' work, one path for every way a recording comes in: keep the file, transcribe
//! it with Whisper, list a match's recordings, delete one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::OwnedMutexGuard;

use super::audio::{self, WHISPER_SAMPLE_RATE};
use super::recording_model::{self, KIND_CALL, KIND_VOICE_MESSAGE, NewRecording, SOURCE_FILE};
use super::recording_row::RecordingRow;
use crate::context::Context;
use crate::service_error::{ErrorKind, ServiceError};
use crate::task_manager::running_work::{Stopped, WorkKind};
use crate::whisper::{MODEL_FILE, MODEL_NAME, Whisper, whisper_cli, whisper_download};

/// The file types the Add a recording dialog offers, by extension.
pub const AUDIO_EXTENSIONS: &[&str] = &["m4a", "mp4", "aac", "mp3", "wav", "flac", "ogg", "opus", "amr", "3gp"];

/// The suffix of the 16 kHz WAV made for whisper-cli next to a recording, deleted after.
const WHISPER_WAV_SUFFIX: &str = ".whisper.wav";

/// The work's name in the log.
const WHERE: &str = "recordings";

/// Keeps the audio file at picked as a recording about match_id, copied into the recordings
/// folder, and transcribes it; the recording's id. A transcription that fails leaves the
/// recording kept, untranscribed, and says why.
pub async fn add_file(context: &Context, match_id: i64, picked: &Path) -> Result<i64, ServiceError> {
    if !recording_model::match_exists(context.db(), match_id)
        .await
        .map_err(|e| ServiceError::internal(WHERE, format!("The match couldn't be read: {e}")))?
    {
        return Err(ServiceError::not_found());
    }
    let folder = recordings_folder(context)?;
    let now = chrono::Utc::now().timestamp();
    let kept_as = keep_copy(folder, picked, now)?;
    let metadata = std::fs::metadata(&kept_as).ok();
    let new_recording = NewRecording {
        match_id: Some(match_id),
        kind: kind_of(picked),
        source: SOURCE_FILE,
        content_url: file_name(&kept_as),
        encoding_format: encoding_format(picked).map(str::to_string),
        content_size: metadata.as_ref().and_then(|m| i64::try_from(m.len()).ok()),
        date_created: std::fs::metadata(picked).ok().and_then(|m| modified_unix(&m)),
        upload_date: now,
    };
    let id = recording_model::add(context.db(), &new_recording)
        .await
        .map_err(|e| ServiceError::internal(WHERE, format!("The recording couldn't be saved: {e}")))?;
    transcribe(context, id).await?;
    Ok(id)
}

/// Transcribes recording id with Whisper, fetching whisper.cpp and its model first when they
/// aren't here yet, and keeps the transcript.
pub async fn transcribe(context: &Context, id: i64) -> Result<(), ServiceError> {
    let whisper = whisper_of(context)?;
    let recording = recording_model::find(context.db(), id)
        .await
        .map_err(|e| ServiceError::internal(WHERE, format!("The recording couldn't be read: {e}")))?
        .ok_or_else(|| ServiceError::new(ErrorKind::NotFound, "That recording was not found."))?;
    let audio_file = recordings_folder(context)?.join(&recording.content_url);
    let program = ready_program(context, whisper).await?;
    ready_model(context, whisper).await?;
    let turn = Arc::clone(&whisper.transcribing).lock_owned().await;
    let what = format!("Recording {}", recording.content_url);
    let heard = context
        .running
        .run(
            WorkKind::Transcribing,
            what,
            hear(&program, &whisper.model_path(), &audio_file, turn),
        )
        .await
        .unwrap_or_else(|Stopped| Err(Stopped::SAID.to_string()))
        .map_err(|why| ServiceError::failed(WHERE, format!("The recording couldn't be transcribed: {why}")))?;
    let (heard, duration_ms) = heard;
    let now = chrono::Utc::now().timestamp();
    recording_model::transcribed(context.db(), id, &heard, duration_ms, MODEL_NAME, now)
        .await
        .map_err(|e| ServiceError::internal(WHERE, format!("The transcript couldn't be saved: {e}")))
}

/// The recordings about match_id, newest first, as the match page shows them.
pub async fn of_match(context: &Context, match_id: i64) -> Result<Vec<RecordingRow>, ServiceError> {
    let recordings = recording_model::of_match(context.db(), match_id)
        .await
        .map_err(|e| ServiceError::internal(WHERE, format!("The recordings couldn't be read: {e}")))?;
    Ok(recordings.iter().map(RecordingRow::from).collect())
}

/// Deletes recording id: its audio file, its transcript and its timed lines.
pub async fn delete(context: &Context, id: i64) -> Result<(), ServiceError> {
    let recording = recording_model::find(context.db(), id)
        .await
        .map_err(|e| ServiceError::internal(WHERE, format!("The recording couldn't be read: {e}")))?
        .ok_or_else(|| ServiceError::new(ErrorKind::NotFound, "That recording was not found."))?;
    let audio_file = recordings_folder(context)?.join(&recording.content_url);
    // A WAV or transcript a stopped transcription couldn't delete (whisper-cli still had it
    // open) goes too.
    WhisperFiles::beside(&audio_file).delete();
    if let Err(e) = std::fs::remove_file(&audio_file)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(ServiceError::internal(
            WHERE,
            format!("{} couldn't be deleted: {e}", audio_file.display()),
        ));
    }
    recording_model::delete(context.db(), id)
        .await
        .map_err(|e| ServiceError::internal(WHERE, format!("The recording couldn't be deleted: {e}")))?;
    Ok(())
}

fn whisper_of(context: &Context) -> Result<&Whisper, ServiceError> {
    context
        .whisper
        .as_ref()
        .ok_or_else(|| ServiceError::internal(WHERE, "The app was started without its whisper folder."))
}

fn recordings_folder(context: &Context) -> Result<&Path, ServiceError> {
    context
        .recordings_folder
        .as_deref()
        .ok_or_else(|| ServiceError::internal(WHERE, "The app was started without its recordings folder."))
}

/// whisper-cli, downloaded first when it is neither in the whisper folder nor on PATH.
async fn ready_program(context: &Context, whisper: &Whisper) -> Result<PathBuf, ServiceError> {
    let _one_at_a_time = whisper.fetching.lock().await;
    if let Some(program) = whisper.program() {
        return Ok(program);
    }
    let label = whisper_download::program_label(whisper.build());
    fetch(context, whisper, label, whisper_download::download_program(whisper)).await?;
    whisper.program().ok_or_else(|| {
        ServiceError::internal(
            WHERE,
            format!(
                "{label} was downloaded, but there's no {} in {}.",
                whisper_cli::PROGRAM_FILE,
                whisper.folder().display()
            ),
        )
    })
}

/// The model, downloaded first when it isn't in the whisper folder.
async fn ready_model(context: &Context, whisper: &Whisper) -> Result<(), ServiceError> {
    let _one_at_a_time = whisper.fetching.lock().await;
    if whisper.model_path().is_file() {
        return Ok(());
    }
    fetch(
        context,
        whisper,
        MODEL_FILE,
        whisper_download::download_model(whisper.folder(), &whisper.downloads),
    )
    .await
}

/// Runs download as a Download row in the task manager called what.
async fn fetch(
    context: &Context,
    whisper: &Whisper,
    what: &str,
    download: impl Future<Output = Result<(), String>>,
) -> Result<(), ServiceError> {
    crate::log_line!("whisper: {what} isn't here yet; downloading it now");
    // Ended here, or on drop when this call is dropped while it waits.
    let turn = whisper
        .downloads
        .begin_turn(what)
        .map_err(|why| ServiceError::new(ErrorKind::TooMany, why))?;
    let outcome = context
        .running
        .run(WorkKind::Download, what.to_string(), download)
        .await
        .unwrap_or_else(|Stopped| Err(Stopped::SAID.to_string()));
    turn.end(outcome.clone());
    outcome.map_err(|why| ServiceError::failed(WHERE, format!("{what} couldn't be downloaded: {why}")))
}

/// What whisper-cli hears in audio_file, and its length in milliseconds: the file is decoded
/// into a 16 kHz WAV beside it first, deleted after.
/// turn is the transcriptions' turn (Whisper::transcribing), let go once both this and the
/// decoding thread are done.
async fn hear(
    program: &Path,
    model: &Path,
    audio_file: &Path,
    turn: OwnedMutexGuard<()>,
) -> Result<(whisper_cli::Heard, i64), String> {
    let wav = whisper_wav_path(audio_file);
    // Held here and by the decoding thread: the files go, and the next transcription may start,
    // when both have let go, also when the transcription is stopped in the task manager (this
    // future is then dropped) while the thread, which can't be stopped, still writes the WAV.
    let files = Arc::new((WhisperFiles::beside(audio_file), turn));
    let (from, to, held) = (audio_file.to_path_buf(), wav.clone(), Arc::clone(&files));
    let duration_ms = tokio::task::spawn_blocking(move || {
        let _held = held;
        let mono = audio::resample(&audio::decode(&from)?, WHISPER_SAMPLE_RATE);
        audio::write_wav(&mono, &to)?;
        Ok::<i64, String>(mono.duration_ms())
    })
    .await
    .map_err(|e| format!("Decoding the sound stopped: {e}"))??;
    let heard = whisper_cli::transcribe(program, model, &wav).await;
    drop(files);
    Ok((heard?, duration_ms))
}

/// The files made for whisper-cli beside a recording: the 16 kHz WAV, and the JSON transcript
/// whisper-cli writes. They hold the recording's sound and words, so they are deleted when the
/// last holder drops this, however the transcription ended.
#[derive(Debug)]
struct WhisperFiles(Vec<PathBuf>);

impl WhisperFiles {
    fn beside(audio_file: &Path) -> Self {
        let wav = whisper_wav_path(audio_file);
        let json = whisper_cli::json_path(&wav);
        WhisperFiles(vec![wav, json])
    }

    /// Deletes the files that are there.
    fn delete(&self) {
        for file in &self.0 {
            if let Err(e) = std::fs::remove_file(file)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                crate::log_line!("whisper: {} couldn't be deleted: {e}", file.display());
            }
        }
    }
}

impl Drop for WhisperFiles {
    fn drop(&mut self) {
        self.delete();
    }
}

fn whisper_wav_path(audio_file: &Path) -> PathBuf {
    let mut name = audio_file.file_name().unwrap_or_default().to_os_string();
    name.push(WHISPER_WAV_SUFFIX);
    audio_file.with_file_name(name)
}

/// Copies picked into folder as "<now>-<its name>", with a number added when that is taken; the
/// copy's path.
fn keep_copy(folder: &Path, picked: &Path, now: i64) -> Result<PathBuf, ServiceError> {
    std::fs::create_dir_all(folder)
        .map_err(|e| ServiceError::internal(WHERE, format!("Couldn't make {}: {e}", folder.display())))?;
    let name = file_name(picked);
    let copy_error = |e| {
        ServiceError::new(
            ErrorKind::Invalid,
            format!("{} couldn't be copied into {}: {e}", picked.display(), folder.display()),
        )
    };
    let mut source = std::fs::File::open(picked).map_err(copy_error)?;
    for taken in 0_u64.. {
        let kept_as = match taken {
            0 => folder.join(format!("{now}-{name}")),
            _ => folder.join(format!("{now}-{taken}-{name}")),
        };
        let mut destination = match std::fs::OpenOptions::new().write(true).create_new(true).open(&kept_as) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(copy_error(e)),
        };
        if let Err(e) = std::io::copy(&mut source, &mut destination) {
            drop(destination);
            let _ = std::fs::remove_file(&kept_as);
            return Err(copy_error(e));
        }
        return Ok(kept_as);
    }
    unreachable!("all recording filenames exhausted")
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().to_string()
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// A voice message for the formats messaging apps send them in (WhatsApp's opus, ogg), else a
/// call.
fn kind_of(path: &Path) -> &'static str {
    match extension_of(path).as_str() {
        "opus" | "ogg" => KIND_VOICE_MESSAGE,
        _ => KIND_CALL,
    }
}

/// The MIME type of the file at path, by its extension.
fn encoding_format(path: &Path) -> Option<&'static str> {
    Some(match extension_of(path).as_str() {
        "m4a" | "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "amr" => "audio/amr",
        "3gp" => "audio/3gpp",
        _ => return None,
    })
}

/// When the file was last changed, in unix seconds: when a recorder made it.
fn modified_unix(metadata: &std::fs::Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    let since_epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since_epoch.as_secs()).ok()
}

/// Whether whisper can run here at all: false when the app has no whisper folder (tests).
pub fn available(context: &Context) -> bool {
    context.whisper.is_some() && context.recordings_folder.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_imports_keep_every_recording() {
        let folder = std::env::temp_dir().join(format!("jobfinder-recordings-race-{}", std::process::id()));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut tasks = Vec::new();
        for number in 0_u8..8 {
            let picked_folder = folder.join(number.to_string());
            std::fs::create_dir_all(&picked_folder).unwrap();
            let picked = picked_folder.join("call.wav");
            std::fs::write(&picked, vec![number; 65536]).unwrap();
            let kept = folder.join("kept");
            let barrier = Arc::clone(&barrier);
            tasks.push(std::thread::spawn(move || {
                barrier.wait();
                let path = keep_copy(&kept, &picked, 100).unwrap();
                (number, path)
            }));
        }
        let mut paths = std::collections::HashSet::new();
        for task in tasks {
            let (number, path) = task.join().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), vec![number; 65536]);
            assert!(paths.insert(path));
        }
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_kept_copy_never_overwrites_another() {
        let folder = std::env::temp_dir().join(format!("jobfinder-recordings-{}", std::process::id()));
        let picked_folder = folder.join("picked");
        std::fs::create_dir_all(&picked_folder).unwrap();
        let picked = picked_folder.join("Call with Acme.m4a");
        std::fs::write(&picked, b"sound").unwrap();
        let kept = folder.join("kept");
        let first = keep_copy(&kept, &picked, 100).unwrap();
        let second = keep_copy(&kept, &picked, 100).unwrap();
        assert_eq!(file_name(&first), "100-Call with Acme.m4a");
        assert_eq!(file_name(&second), "100-1-Call with Acme.m4a");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn the_files_made_for_whisper_go_once_the_last_holder_lets_go() {
        let folder = std::env::temp_dir().join(format!("jobfinder-whisper-files-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let audio_file = folder.join("call.m4a");
        let wav = whisper_wav_path(&audio_file);
        let json = whisper_cli::json_path(&wav);
        let files = Arc::new(WhisperFiles::beside(&audio_file));
        let decoding_thread = Arc::clone(&files);
        drop(files);
        // Written by the decoding thread after the transcription was stopped.
        std::fs::write(&wav, b"sound").unwrap();
        std::fs::write(&json, b"{}").unwrap();
        drop(decoding_thread);
        assert!(!wav.exists() && !json.exists());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[tokio::test]
    async fn a_retry_waits_for_a_stopped_transcriptions_decoding_thread() {
        let transcribing = Arc::new(tokio::sync::Mutex::new(()));
        let stopped = Arc::clone(&transcribing).lock_owned().await;
        // What the stopped transcription's decoding thread still holds once its future is gone.
        let decoding_thread = Arc::new((WhisperFiles(Vec::new()), stopped));
        assert!(transcribing.try_lock().is_err(), "the retry waits");
        drop(decoding_thread);
        assert!(transcribing.try_lock().is_ok(), "and starts once the thread is done");
    }

    #[test]
    fn whatsapps_voice_messages_are_told_from_calls() {
        assert_eq!(kind_of(Path::new("PTT-20261105-WA0001.opus")), KIND_VOICE_MESSAGE);
        assert_eq!(kind_of(Path::new("Opkald 2026-11-05.M4A")), KIND_CALL);
        assert_eq!(encoding_format(Path::new("a.M4A")), Some("audio/mp4"));
        assert_eq!(encoding_format(Path::new("a.txt")), None);
    }

    #[tokio::test]
    async fn a_recording_for_a_match_that_isnt_there_is_refused() {
        let db = crate::db::open_in_memory().await.unwrap();
        let context = Context::new(
            db,
            std::sync::Arc::new(crate::taxonomy::Taxonomy::load()),
            crate::account::key_vault::KeyVault::with_key_in_memory(),
        );
        let refused = add_file(&context, 404, Path::new("nothing.m4a")).await.unwrap_err();
        assert_eq!(refused.kind, ErrorKind::NotFound);
    }
}
