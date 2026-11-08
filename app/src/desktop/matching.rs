//! Matching in the window: a job post (a link, its text or a file) matched with the CV (the
//! profile from setup, or another one pasted or picked), its result with the verdicts, and the
//! saved matches by list, each opening its result, with Not interested and Bring back. All of
//! it through match_service, the work the match page does, so the two can't disagree.
//!
//! What the window can't do yet (writing the application and résumé, the Apply dialog, the
//! Jobnet joblog) opens the saved match's page in the browser.

use std::sync::Arc;

use iced::widget::{Column, Row, button, checkbox, column, container, pick_list, row, text, text_editor, text_input};
use iced::{Center, Element, Fill, Task};

use super::apply::{self, ApplyMsg, Applying};
use super::batch::{self, Batch};
use super::documents::{self, DocMsg, Docs};
use super::drawings;
use super::recordings_card::{self, RecMsg, Recordings};
use super::style;
use super::task_manager::Working;
use super::widgets::{self, CARD_PADDING, ROW_PADDING};
use super::{GAP, Runner, Step};
use crate::context::Context;
use crate::dashboard::Health;
use crate::matches::document_box::{APPLICATION, RESUME};
use crate::matches::document_question::Answer as QuestionAnswer;
use crate::matches::document_writer::Revision;
use crate::matches::joblog;
use crate::matches::joblog_panel::Logged;
use crate::matches::match_model::{LISTS, SortOrder};
use crate::matches::match_result::{MatchResult, OpeningLine, Section};
use crate::matches::match_service::{
    self, JobAsk, MAX_JOBS, MatchAnswer, MatchAsk, ReadCv, RejectOutcome, SavedMatchList, WriteAnswer,
};
use crate::matches::page_reader::Page as Fetched;
use crate::matches::reject_button::{self, RejectButton};
use crate::matches::reject_reason::{self, Rejection};
use crate::matches::saved_match_row::SavedMatchRow;
use crate::matches::still_open;
use crate::matches::verdict_row::VerdictRow;
use crate::profile;
use crate::service_error::{ErrorKind, ServiceError};
use crate::task_manager::TaskRow;
use crate::task_manager::running_work::WorkKind;

/// The CV box's height.
const CV_HEIGHT: f32 = 180.0;
/// The job box's height.
const JOB_HEIGHT: f32 = 220.0;
/// The height of another job post's box.
const MORE_JOB_HEIGHT: f32 = 140.0;
/// The score's size beside the job's name.
const SCORE_SIZE: f32 = 40.0;
/// The filter that shows every requirement (match_result::filters).
const ALL: &str = "all";
/// The files the Choose file dialog offers.
const FILE_KINDS: [&str; 4] = ["pdf", "txt", "md", "text"];
/// How a PDF starts, for one picked with another name.
const PDF_START: &[u8] = b"%PDF";
/// How often the writing's working list is read again while it runs.
const WORK_READ_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// The Match screen's pages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Page {
    /// The status and the glance at the saved matches (match_screen).
    Overview,
    /// Match a job.
    New,
    /// A list of saved matches (match_model::LISTS).
    List(&'static str),
    /// A saved match's result.
    Saved(i64),
}

/// Which box a picked file goes in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Cv,
    Job,
}

/// A result on screen, with its filter and its Not interested form.
struct Shown {
    result: MatchResult,
    /// The filter key shown: ALL, or a verdict.
    filter: &'static str,
    /// The Not interested form is open.
    rejecting: bool,
    /// The reasons ticked, in reject_reason::REASONS's order.
    ticked: Vec<&'static str>,
    /// Each reason's note.
    notes: Vec<(&'static str, String)>,
    /// What the form says is missing.
    problem: Option<String>,
    /// The application and résumé, for a saved match.
    docs: Option<Docs>,
    /// Its Apply and Jobnet joblog sections.
    applying: Option<Applying>,
}

impl Shown {
    fn new(result: MatchResult) -> Self {
        Shown {
            docs: result.documents.as_ref().map(Docs::new),
            applying: result.saved_id.map(Applying::new),
            result,
            filter: ALL,
            rejecting: false,
            ticked: Vec::new(),
            notes: Vec::new(),
            problem: None,
        }
    }

    fn note(&self, reason: &str) -> &str {
        self.notes
            .iter()
            .find(|(of, _)| *of == reason)
            .map_or("", |(_, note)| note.as_str())
    }
}

/// The CV a match is made with.
#[derive(Default)]
struct Cv {
    /// The profile from setup as text: None until read, Some(None) when there is none.
    profile: Option<Option<String>>,
    /// Another CV is pasted or picked rather than the profile.
    own: bool,
    text: text_editor::Content,
    /// The file it was picked from.
    file_name: Option<String>,
    /// The CV as read the last time, used again while its text is the same.
    read: Option<ReadCv>,
}

impl Cv {
    /// The text matched: the profile, or the CV in the box.
    fn text(&self) -> String {
        match (&self.profile, self.own) {
            (Some(Some(profile)), false) => profile.clone(),
            _ => self.text.text(),
        }
    }

    fn uses_profile(&self) -> bool {
        !self.own && matches!(self.profile, Some(Some(_)))
    }
}

#[derive(Default)]
struct List {
    /// None until read; Err says why it couldn't be.
    read: Option<Result<(), String>>,
    rows: Vec<SavedMatchRow>,
    next_after: Option<String>,
    empty: &'static str,
    sort: SortOrder,
}

pub struct State {
    pub page: Page,
    cv: Cv,
    job_url: String,
    job: text_editor::Content,
    job_file: Option<String>,
    /// More job posts to match with the same CV (text or a link), after the first.
    more_jobs: Vec<MoreJob>,
    /// The last batch of several posts, ranked.
    batch: Option<Batch>,
    /// Per ranked row (by its place in the batch), its result with its edits and documents
    /// written, made when the batch is matched; the open row's is in fresh meanwhile.
    ranked_shown: Vec<Option<Shown>>,
    /// Matching, fetching or reading a file: the form's buttons wait.
    busy: bool,
    /// The step running, or what the last one came to.
    said: Option<(Health, String)>,
    /// The result of the last match on the Match a job page.
    fresh: Option<Shown>,
    list: List,
    /// The saved match open on its page.
    saved: Option<Result<Shown, String>>,
    /// Where a saved match's page goes back to.
    back: Page,
    /// The number of the newest read, so an answer to an older one is dropped.
    request: u64,
    /// The writing and questions of the application and résumé, as the task manager has them.
    working: Working,
    /// Questions to the writer waiting for their answers.
    questions: usize,
    /// working is read again every WORK_READ_EVERY, so a second start doesn't start a second
    /// round.
    watching: bool,
    /// The calls and notes of the saved match open on its page.
    recordings: Recordings,
}

impl Default for State {
    fn default() -> Self {
        State {
            page: Page::Overview,
            cv: Cv::default(),
            job_url: String::new(),
            job: text_editor::Content::new(),
            job_file: None,
            more_jobs: Vec::new(),
            batch: None,
            ranked_shown: Vec::new(),
            busy: false,
            said: None,
            fresh: None,
            list: List::default(),
            saved: None,
            back: Page::Overview,
            request: 0,
            working: Working::default(),
            questions: 0,
            watching: false,
            recordings: Recordings::default(),
        }
    }
}

/// Another job post to match: its box, and the link it was fetched from.
#[derive(Default)]
struct MoreJob {
    text: text_editor::Content,
    /// Where the box's text was fetched from, saved with the match; "" when it was typed.
    url: String,
}

/// A file picked: its name and text; None when the dialog was closed.
pub(super) type Picked = Option<(String, String)>;

#[derive(Clone)]
pub enum Msg {
    Go(Page),
    ProfileRead(Result<Option<String>, String>),
    UseOwnCv(bool),
    CvEdit(text_editor::Action),
    JobEdit(text_editor::Action),
    JobUrl(String),
    ChooseFile(Target),
    FileRead(Target, Result<Picked, String>),
    Fetch,
    Fetched(Result<Arc<Fetched>, String>),
    Match,
    /// A step of the match, as it starts.
    Matching(String),
    Matched(Arc<Result<MatchAnswer, String>>),
    AddJob,
    RemoveJob(usize),
    /// An edit in another job post's box.
    MoreJobEdit(usize, text_editor::Action),
    /// What matching several posts came to: an answer per post, in their order.
    BatchMatched(Arc<Result<Vec<Result<MatchAnswer, String>>, String>>),
    /// Opens a ranked row's result under the list.
    OpenRanked(usize),
    ListLoaded {
        request: u64,
        more: bool,
        list: Result<Arc<SavedMatchList>, String>,
    },
    ShowMore,
    SavedLoaded {
        request: u64,
        result: Result<Arc<MatchResult>, String>,
    },
    Filter(&'static str),
    OpenReject(bool),
    /// "Open job post": the post the match was made from, in the browser.
    OpenJobPost(String),
    /// "Copy as prompt" in the footer: the shown saved match as a prompt for a chatbot.
    CopyPrompt,
    PromptMade(Result<String, String>),
    /// "Mark as applied" in the footer: the shown saved match was applied for.
    MarkApplied,
    MarkedApplied {
        id: i64,
        /// The joblog status and entry as saved.
        outcome: Result<(&'static str, Logged), String>,
    },
    Tick(&'static str, bool),
    Note(&'static str, String),
    Reject,
    BringBack,
    /// "Check which are still open" on Saved matches.
    CheckOpen,
    /// What the check found, in words.
    CheckedOpen(Result<String, String>),
    /// "It's still open" on a closed saved match's page.
    KeepOpen,
    KeptOpen {
        id: i64,
        outcome: Result<(), String>,
    },
    Rejected {
        /// The saved match rejected or brought back.
        id: i64,
        undo: bool,
        rejections: Arc<Vec<Rejection>>,
        outcome: Result<RejectOutcome, String>,
    },
    /// The application and résumé of saved match id.
    Doc(i64, DocMsg),
    /// The writer's answer to a question in a thread on passage of saved match id's kind.
    Answered {
        id: i64,
        kind: &'static str,
        passage: String,
        answer: Arc<Result<QuestionAnswer, String>>,
    },
    /// Read the window's writing in the task manager (again).
    ReadWork,
    WorkRead(Vec<TaskRow>),
    /// What stopping it from its working list came to.
    WorkStopped(Result<String, String>),
    /// What writing a document of saved match id came to.
    Wrote {
        id: i64,
        answer: Arc<Result<WriteAnswer, String>>,
    },
    /// The just matched job's documents read again: whether each can be written now.
    Availability {
        id: i64,
        result: Result<Arc<MatchResult>, String>,
    },
    /// The Apply and Jobnet joblog sections of saved match id, as one result shows them
    /// (Applying::view): a click there, or the answer to its read or save.
    Apply(i64, u64, ApplyMsg),
    /// Where a PDF was saved; None when the dialog was closed.
    PdfSaved(Result<Option<String>, String>),
    /// The calls and notes of the saved match open on its page.
    Recordings(RecMsg),
    /// For the shell: go to Language models.
    ShowLanguageModels,
    /// For the shell: a match was saved or rejected, so the overview is read again.
    Changed,
    Sort(SortOrder),
}

pub fn update(state: &mut State, msg: Msg, runner: &Runner) -> Task<Msg> {
    if is_stale(state.request, &msg) || (state.busy && starts_work(&msg)) {
        return Task::none();
    }
    match msg {
        Msg::Go(page) => return go(state, page, runner),
        Msg::ProfileRead(read) => match read {
            // Another CV chosen stays chosen; the profile is there to go back to.
            Ok(profile) => state.cv.profile = Some(profile),
            Err(why) => {
                state.cv.profile = Some(None);
                state.said = Some((Health::Attention, format!("Your profile couldn't be read: {why}")));
            }
        },
        Msg::UseOwnCv(own) => state.cv.own = own,
        Msg::CvEdit(action) => state.cv.text.perform(action),
        Msg::JobEdit(action) => state.job.perform(action),
        Msg::JobUrl(url) => state.job_url = url,
        Msg::ChooseFile(target) => {
            state.busy = true;
            return runner.run(
                |context| async move { pick_file(context).await },
                move |picked| Msg::FileRead(target, picked.and_then(|picked| picked.map_err(|e| e.message))),
            );
        }
        Msg::FileRead(target, picked) => {
            state.busy = false;
            match picked {
                Ok(None) => {}
                Ok(Some((name, text))) => {
                    let content = text_editor::Content::with_text(&text);
                    match target {
                        Target::Cv => {
                            state.cv.text = content;
                            state.cv.file_name = Some(name);
                            state.cv.own = true;
                        }
                        Target::Job => {
                            state.job = content;
                            state.job_file = Some(name);
                            // Another post: the last one's link would be saved with it.
                            state.job_url.clear();
                        }
                    }
                    state.said = None;
                }
                Err(why) => state.said = Some((Health::Attention, why)),
            }
        }
        Msg::Fetch => {
            let url = state.job_url.trim().to_string();
            if !match_service::is_link(&url) {
                state.said = Some((
                    Health::Attention,
                    "Paste a link starting with http:// or https://.".to_string(),
                ));
                return Task::none();
            }
            state.busy = true;
            state.said = Some((Health::Neutral, "Fetching the job post…".to_string()));
            return runner.run(
                move |context| async move { match_service::fetch_job(&context, &url).await.map(Arc::new) },
                |page| Msg::Fetched(page.and_then(|page| page.map_err(|e| e.message))),
            );
        }
        Msg::Fetched(page) => {
            state.busy = false;
            state.said = Some(match page {
                Ok(page) => {
                    put_fetched(state, &page);
                    (
                        Health::Ready,
                        format!(
                            "Read {} characters from {}. Check the text, then match.",
                            page.text.chars().count(),
                            host_of(&page.url)
                        ),
                    )
                }
                Err(why) => (Health::Attention, why),
            });
        }
        Msg::Match if state.more_jobs.is_empty() => return start_match(state, runner),
        Msg::Match => return start_batch(state, runner),
        Msg::AddJob => {
            if state.more_jobs.len() + 1 < MAX_JOBS {
                state.more_jobs.push(MoreJob::default());
            }
        }
        Msg::RemoveJob(index) => {
            if index < state.more_jobs.len() {
                state.more_jobs.remove(index);
            }
        }
        Msg::MoreJobEdit(index, action) => {
            if let Some(job) = state.more_jobs.get_mut(index) {
                job.text.perform(action);
            }
        }
        Msg::BatchMatched(answers) => return batch_matched(state, &answers),
        Msg::OpenRanked(index) => {
            if let Some(batch) = &mut state.batch {
                // The open row's result is kept as it is, edits and written documents with it.
                if let Some(open) = batch.open
                    && let Some(slot) = state.ranked_shown.get_mut(open)
                {
                    *slot = state.fresh.take();
                }
                let open = (batch.open != Some(index)).then_some(index);
                state.fresh = open.and_then(|index| state.ranked_shown.get_mut(index).and_then(Option::take));
                batch.open = open;
                // A use may have been turned on or off since the batch: Write follows it.
                return refresh_availability(state, runner);
            }
        }
        Msg::Matching(step) => state.said = Some((Health::Neutral, step)),
        Msg::Matched(answer) => return matched(state, &answer),
        Msg::ListLoaded { more, list, .. } => match list {
            Ok(list) => {
                if !more {
                    state.list.rows.clear();
                }
                state.list.rows.extend(list.matches.iter().cloned());
                state.list.next_after = list.next_after.clone();
                state.list.read = Some(Ok(()));
            }
            Err(why) => state.list.read = Some(Err(why)),
        },
        Msg::ShowMore => {
            if let (Page::List(list), Some(after)) = (state.page, state.list.next_after.clone()) {
                return load_list(state, list, after, runner);
            }
        }
        Msg::SavedLoaded { result, .. } => {
            state.saved = Some(result.map(|result| Shown::new(Arc::unwrap_or_clone(result))));
        }
        Msg::Availability { id, result } => match result {
            Ok(read) => {
                let docs = state.fresh.as_mut().and_then(|shown| shown.docs.as_mut());
                if let (Some(docs), Some(now)) = (docs.filter(|docs| docs.id == Some(id)), &read.documents) {
                    docs.availability(&now.boxes);
                }
            }
            Err(error) => {
                crate::app_log::error(&format!("Could not read the match's documents again: {error}"));
                state.said = Some((
                    Health::Attention,
                    format!("The documents couldn't be read again: {error}"),
                ));
            }
        },
        Msg::Filter(key) => {
            if let Some(shown) = shown_mut(state) {
                shown.filter = key;
            }
        }
        Msg::OpenReject(open) => {
            if let Some(shown) = shown_mut(state) {
                shown.rejecting = open;
                shown.problem = None;
            }
        }
        Msg::Tick(reason, on) => {
            if let Some(shown) = shown_mut(state) {
                shown.ticked.retain(|ticked| *ticked != reason);
                if on {
                    shown.ticked.push(reason);
                }
            }
        }
        Msg::Note(reason, note) => {
            if let Some(shown) = shown_mut(state) {
                let note: String = note.chars().take(reject_reason::NOTE_MAX).collect();
                shown.notes.retain(|(of, _)| *of != reason);
                shown.notes.push((reason, note));
            }
        }
        Msg::Reject => return reject(state, false, runner),
        Msg::BringBack => return reject(state, true, runner),
        Msg::CheckOpen => {
            state.busy = true;
            state.said = Some((
                Health::Neutral,
                "Checking whether your saved jobs are still open…".to_string(),
            ));
            return runner.run(
                |context| async move { still_open::check_now(&context).await },
                |checked| Msg::CheckedOpen(checked.and_then(|checked| checked.map_err(|e| e.message))),
            );
        }
        Msg::CheckedOpen(checked) => {
            state.busy = false;
            state.said = Some(match checked {
                Ok(said) => (Health::Ready, said),
                Err(why) => (Health::Attention, why),
            });
            // The lists have changed.
            if let Page::List(list) = state.page {
                return Task::batch([reload_list(state, list, runner), Task::done(Msg::Changed)]);
            }
            state.list.read = None;
            return Task::done(Msg::Changed);
        }
        Msg::Sort(order) => {
            if let Page::List(list) = state.page
                && state.list.sort != order
            {
                state.list.sort = order;
                runner.context.set_saved_match_sort(order);
                return reload_list(state, list, runner);
            }
        }
        Msg::OpenJobPost(url) => {
            if let Err(why) = super::open_in_browser(&url) {
                state.said = Some((Health::Attention, why));
            }
        }
        Msg::CopyPrompt => {
            let Some(shown) = shown_mut(state) else {
                return Task::none();
            };
            let Some(id) = shown.result.saved_id else {
                return Task::none();
            };
            let written = |kind| written_text(shown, kind).unwrap_or_default();
            let (application, resume) = (written(APPLICATION), written(RESUME));
            state.busy = true;
            return runner.run(
                move |context| async move { match_service::chat_prompt(&context, id, &application, &resume).await },
                |made| Msg::PromptMade(made.and_then(|made| made.map_err(|e| e.message))),
            );
        }
        Msg::PromptMade(made) => {
            state.busy = false;
            match made {
                Ok(prompt) => {
                    state.said = Some((
                        Health::Ready,
                        "Copied as a prompt: paste it into the chatbot you use.".to_string(),
                    ));
                    return iced::clipboard::write(prompt);
                }
                Err(why) => state.said = Some((Health::Attention, why)),
            }
        }
        Msg::MarkApplied => {
            let Some(id) = shown_mut(state).and_then(|shown| shown.result.saved_id) else {
                return Task::none();
            };
            state.busy = true;
            return runner.run(
                move |context| async move { apply::mark_applied(&context, id).await },
                move |outcome| Msg::MarkedApplied {
                    id,
                    outcome: outcome.and_then(|outcome| outcome),
                },
            );
        }
        Msg::MarkedApplied { id, outcome } => {
            state.busy = false;
            match outcome {
                Ok((status, logged)) => {
                    for shown in results_of(state, id) {
                        shown.result.applied_on = logged.applied_on.clone();
                        if let Some(sections) = shown.applying.as_mut() {
                            sections.joblog_saved(status, &logged);
                        }
                    }
                    state.said = Some((Health::Ready, marked_applied(status, &logged.applied_on)));
                    // It moves from the shortlist to "Applied, not on Jobnet yet".
                    state.list.read = None;
                    return Task::done(Msg::Changed);
                }
                Err(why) => state.said = Some((Health::Attention, why)),
            }
        }
        Msg::KeepOpen => {
            let Some(id) = shown_mut(state).and_then(|shown| shown.result.saved_id) else {
                return Task::none();
            };
            state.busy = true;
            return runner.run(
                move |context| async move { still_open::keep_open(&context, id).await },
                move |outcome| Msg::KeptOpen {
                    id,
                    outcome: outcome.and_then(|outcome| outcome.map_err(|e| e.message)),
                },
            );
        }
        Msg::KeptOpen { id, outcome } => {
            state.busy = false;
            match outcome {
                Ok(()) => {
                    for shown in results_of(state, id) {
                        shown.result.opening = None;
                    }
                    state.said = Some((
                        Health::Ready,
                        "Marked as still open: it is back in its lists, and JobFinder won't check it again."
                            .to_string(),
                    ));
                    state.list.read = None;
                    return Task::done(Msg::Changed);
                }
                Err(why) => state.said = Some((Health::Attention, why)),
            }
        }
        Msg::Rejected {
            id,
            undo,
            rejections,
            outcome,
        } => {
            state.busy = false;
            match outcome {
                Ok(RejectOutcome::Done(said)) => {
                    // Every result of this match shows it, whichever page is open now.
                    for shown in results_of(state, id) {
                        shown.result.reject = RejectButton::from(Some(id), (!undo).then_some(rejections.as_slice()));
                        shown.rejecting = false;
                        shown.ticked.clear();
                        shown.notes.clear();
                        shown.problem = None;
                    }
                    state.said = said.map(|said| (Health::Ready, said.to_string()));
                    // The lists have changed.
                    if let Page::List(list) = state.page {
                        return Task::batch([go(state, Page::List(list), runner), Task::done(Msg::Changed)]);
                    }
                    state.list.read = None;
                    return Task::done(Msg::Changed);
                }
                Ok(RejectOutcome::Unfinished(problem)) => {
                    for shown in results_of(state, id) {
                        shown.problem = Some(problem.clone());
                    }
                }
                Err(why) => state.said = Some((Health::Attention, why)),
            }
        }
        // The shell's.
        Msg::Doc(id, msg) => return document(state, id, msg, runner),
        Msg::Apply(id, view, msg) => return applying(state, id, view, msg, runner),
        Msg::Recordings(msg) => return state.recordings.update(msg, runner).map(Msg::Recordings),
        Msg::Wrote { id, answer } => {
            state.busy = false;
            match answer.as_ref() {
                Ok(answer) => {
                    let mut read_again = Vec::new();
                    for shown in results_of(state, id) {
                        if let Some(docs) = shown.docs.as_mut() {
                            docs.written(answer);
                        }
                        if let Some(applying) = shown.applying.as_mut() {
                            let view = applying.view();
                            read_again.push(applying.written(runner).map(move |msg| Msg::Apply(id, view, msg)));
                        }
                    }
                    state.said = Some(match &answer.problem {
                        Some(problem) => (Health::Attention, format!("{}. {problem}", answer.done)),
                        None => (Health::Ready, format!("{}.", answer.done)),
                    });
                    state.list.read = None;
                    return Task::batch(read_again).chain(Task::done(Msg::Changed));
                }
                Err(why) => {
                    for shown in results_of(state, id) {
                        if let Some(docs) = shown.docs.as_mut() {
                            docs.not_written();
                        }
                    }
                    state.said = Some((Health::Attention, why.clone()));
                }
            }
        }
        Msg::ReadWork => {
            return runner.run(
                |context| async move {
                    crate::task_manager::of_kind(crate::task_manager::rows(&context).await, WorkKind::Writing)
                },
                |rows| Msg::WorkRead(rows.unwrap_or_default()),
            );
        }
        Msg::WorkRead(rows) => {
            state.working = Working {
                rows,
                read_at: chrono::Utc::now().timestamp(),
            };
            // The work's row may not be listed yet when it has just started.
            if state.busy || state.questions > 0 || !state.working.rows.is_empty() {
                return runner.run(|_| tokio::time::sleep(WORK_READ_EVERY), |_| Msg::ReadWork);
            }
            state.watching = false;
        }
        Msg::WorkStopped(said) => {
            if let Err(why) = said {
                crate::log_line!("match: stopping the writing failed: {why}");
                state.said = Some((Health::Attention, why));
            }
        }
        Msg::Answered {
            id,
            kind,
            passage,
            answer,
        } => {
            state.questions = state.questions.saturating_sub(1);
            for shown in results_of(state, id) {
                if let Some(docs) = shown.docs.as_mut() {
                    docs.answered(kind, &passage, &answer);
                }
            }
        }
        Msg::PdfSaved(saved) => {
            state.busy = false;
            match saved {
                Ok(None) => {}
                Ok(Some(path)) => state.said = Some((Health::Ready, format!("Saved {path}."))),
                Err(why) => state.said = Some((Health::Attention, why)),
            }
        }
        Msg::Changed | Msg::ShowLanguageModels => {}
    }
    Task::none()
}

/// A click that starts work, which waits while other work runs (a disabled button can't send
/// it, but Enter in the link field can), so two answers can't land on the form in either order.
fn starts_work(msg: &Msg) -> bool {
    matches!(
        msg,
        Msg::Fetch
            | Msg::Match
            | Msg::OpenRanked(_)
            | Msg::ChooseFile(_)
            | Msg::Reject
            | Msg::BringBack
            | Msg::CheckOpen
            | Msg::KeepOpen
            | Msg::MarkApplied
            | Msg::CopyPrompt
            | Msg::Doc(_, DocMsg::Write(_) | DocMsg::Revise(_) | DocMsg::SavePdf(..))
            | Msg::Apply(_, _, ApplyMsg::SavePdf(_))
    )
}

/// An answer to a read older than the newest.
fn is_stale(request: u64, msg: &Msg) -> bool {
    match msg {
        Msg::ListLoaded { request: asked, .. } | Msg::SavedLoaded { request: asked, .. } => *asked != request,
        _ => false,
    }
}

/// Reads the just matched job's documents again, since a key may have been set or turned off
/// on Language models while away; their texts stay as they are.
fn refresh_availability(state: &State, runner: &Runner) -> Task<Msg> {
    let Some(id) = state
        .fresh
        .as_ref()
        .and_then(|shown| shown.docs.as_ref())
        .and_then(|docs| docs.id)
    else {
        return Task::none();
    };
    runner.run(
        move |context| async move { match_service::saved_result(&context, id).await.map(Arc::new) },
        move |result| Msg::Availability {
            id,
            result: result.and_then(|result| result.map_err(|e| e.message)),
        },
    )
}

fn go(state: &mut State, page: Page, runner: &Runner) -> Task<Msg> {
    if !state.busy {
        state.said = None;
    }
    if matches!(page, Page::Saved(_)) && !matches!(state.page, Page::Saved(_)) {
        state.back = state.page;
    }
    state.page = page;
    match page {
        Page::Overview => Task::none(),
        // Read on every visit, so a CV read again in setup is the one matched.
        Page::New => Task::batch([
            runner.run(
                |context| async move { profile::text_for_user(context.db()).await },
                |read| Msg::ProfileRead(read.and_then(|read| read.map_err(|e| e.to_string()))),
            ),
            refresh_availability(state, runner),
        ]),
        Page::List(list) => {
            state.list.empty = LISTS
                .iter()
                .find(|(name, ..)| *name == list)
                .map_or("", |(.., empty)| *empty);
            reload_list(state, list, runner)
        }
        Page::Saved(id) => {
            state.request += 1;
            let request = state.request;
            state.saved = None;
            Task::batch([
                runner.run(
                    move |context| async move { match_service::saved_result(&context, id).await.map(Arc::new) },
                    move |result| Msg::SavedLoaded {
                        request,
                        result: result.and_then(|result| result.map_err(|e| e.message)),
                    },
                ),
                state.recordings.open(id, runner).map(Msg::Recordings),
            ])
        }
    }
}

/// Reads a batch of list after the cursor after ("" for the first one).
/// Reads list from its start: nothing of the last read stays, its cursor least of all.
fn reload_list(state: &mut State, list: &'static str, runner: &Runner) -> Task<Msg> {
    state.list.read = None;
    state.list.rows.clear();
    state.list.next_after = None;
    load_list(state, list, String::new(), runner)
}

fn load_list(state: &mut State, list: &'static str, after: String, runner: &Runner) -> Task<Msg> {
    let sort = state.list.sort;
    state.request += 1;
    let request = state.request;
    let more = !after.is_empty();
    runner.run(
        move |context| async move {
            match_service::sorted_saved_matches(&context, Some(list), &after, sort)
                .await
                .map(Arc::new)
        },
        move |list| Msg::ListLoaded {
            request,
            more,
            list: list.and_then(|list| list.map_err(|e| e.message)),
        },
    )
}

fn start_match(state: &mut State, runner: &Runner) -> Task<Msg> {
    state.busy = true;
    state.said = Some((Health::Neutral, "Matching…".to_string()));
    let job_text = state.job.text();
    let job_url = state.job_url.clone();
    let cv_text = state.cv.text();
    let read_cv = state.cv.read.clone();
    runner.run_saying(
        move |context, say| async move {
            let ask = MatchAsk {
                job_text: &job_text,
                job_url: &job_url,
                cv_text: &cv_text,
                read_cv: read_cv.as_ref(),
            };
            match_service::match_job(&context, ask, &*say).await
        },
        |step| match step {
            Step::Said(line) => Msg::Matching(line),
            Step::Done(answer) => Msg::Matched(Arc::new(answer.and_then(|answer| answer.map_err(|e| e.message)))),
        },
    )
}

/// The job posts as typed: the first box (or its link), then the others.
fn jobs_typed(state: &State) -> Vec<JobAsk> {
    let first = JobAsk {
        job_text: state.job.text(),
        job_url: state.job_url.clone(),
    };
    let more = state.more_jobs.iter().map(|job| JobAsk {
        job_text: job.text.text(),
        job_url: job.url.clone(),
    });
    std::iter::once(first).chain(more).collect()
}

fn start_batch(state: &mut State, runner: &Runner) -> Task<Msg> {
    state.busy = true;
    state.said = Some((Health::Neutral, "Matching…".to_string()));
    let jobs = jobs_typed(state);
    let cv_text = state.cv.text();
    let read_cv = state.cv.read.clone();
    runner.run_saying(
        move |context, say| async move { match_service::match_jobs(&context, &jobs, &cv_text, read_cv, &*say).await },
        |step| match step {
            Step::Said(line) => Msg::Matching(line),
            Step::Done(answers) => Msg::BatchMatched(Arc::new(answers.and_then(|answers| {
                answers
                    .map(|answers| {
                        answers
                            .into_iter()
                            .map(|answer| answer.map_err(|e| e.message))
                            .collect()
                    })
                    .map_err(|e| e.message)
            }))),
        },
    )
}

fn batch_matched(state: &mut State, answers: &Result<Vec<Result<MatchAnswer, String>>, String>) -> Task<Msg> {
    state.busy = false;
    let answers = match answers {
        Ok(answers) => answers,
        Err(why) => {
            state.said = Some((Health::Attention, why.clone()));
            return Task::none();
        }
    };
    let typed: Vec<String> = jobs_typed(state)
        .into_iter()
        .map(|job| {
            if job.job_text.trim().is_empty() {
                job.job_url
            } else {
                job.job_text
            }
        })
        .collect();
    // A fetched post goes in its box, so it can be checked and matched again without fetching.
    for (index, answer) in answers.iter().enumerate() {
        let Ok(MatchAnswer {
            fetched: Some(page), ..
        }) = answer
        else {
            continue;
        };
        match index.checked_sub(1) {
            None => put_fetched(state, page),
            Some(more) => {
                if let Some(job) = state.more_jobs.get_mut(more) {
                    job.text = text_editor::Content::with_text(&page.text);
                    job.url = page.url.clone();
                }
            }
        }
    }
    if let Some(read) = answers
        .iter()
        .rev()
        .find_map(|answer| answer.as_ref().ok()?.read_cv.clone())
    {
        state.cv.read = Some(read);
    }
    let batch = Batch::new(&typed, answers);
    // The list says how it went, and each post that failed says why.
    state.said = None;
    let saved_now = batch.saved_now > 0;
    // Every row's result is made now, not when first opened, so a document written or a status
    // saved through another row (or the saved match's page) of the same match reaches it too.
    state.ranked_shown = (0..answers.len())
        .map(|index| batch.result(index).cloned().map(Shown::new))
        .collect();
    state.batch = Some(batch);
    state.fresh = None;
    if saved_now {
        state.list.read = None;
        return Task::done(Msg::Changed);
    }
    Task::none()
}

fn matched(state: &mut State, answer: &Result<MatchAnswer, String>) -> Task<Msg> {
    state.busy = false;
    let answer = match answer {
        Ok(answer) => answer,
        Err(why) => {
            state.said = Some((Health::Attention, why.clone()));
            return Task::none();
        }
    };
    if let Some(page) = &answer.fetched {
        put_fetched(state, page);
    }
    if let Some(read) = &answer.read_cv {
        state.cv.read = Some(read.clone());
    }
    state.said = if answer.matched_before {
        Some((
            Health::Neutral,
            "You matched this post with this CV before, so this is the saved match. Change the CV or the post to match again."
                .to_string(),
        ))
    } else {
        answer.save_error.clone().map(|why| (Health::Attention, why))
    };
    state.fresh = Some(Shown::new(answer.result.clone()));
    state.batch = None;
    state.ranked_shown.clear();
    let saved_now = answer.result.saved_id.is_some() && !answer.matched_before;
    if saved_now {
        state.list.read = None;
        return Task::done(Msg::Changed);
    }
    Task::none()
}

/// The fetched post in the job boxes.
fn put_fetched(state: &mut State, page: &Fetched) {
    state.job_url = page.url.clone();
    state.job = text_editor::Content::with_text(&page.text);
    state.job_file = None;
}

/// "www.jobindex.dk" of "https://www.jobindex.dk/job/1".
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// The result on the page shown.
fn shown_mut(state: &mut State) -> Option<&mut Shown> {
    match state.page {
        Page::New => state.fresh.as_mut(),
        Page::Saved(_) => state.saved.as_mut().and_then(|saved| saved.as_mut().ok()),
        Page::Overview | Page::List(_) => None,
    }
}

/// A click in saved match id's application and résumé: writing, copying and saving run here,
/// the rest is the panel's own.
fn document(state: &mut State, id: i64, msg: DocMsg, runner: &Runner) -> Task<Msg> {
    let Some(docs) = shown_mut(state)
        .and_then(|shown| shown.docs.as_mut())
        .filter(|docs| docs.id == Some(id))
    else {
        return Task::none();
    };
    match msg {
        DocMsg::Write(kind) if !docs.asks_first(kind) => {
            let notes = docs.notes();
            write_document(state, id, notes, kind, None, runner)
        }
        DocMsg::Revise(kind) => match docs.revision(kind) {
            Some(revision) => {
                let notes = docs.notes();
                write_document(state, id, notes, kind, Some(revision), runner)
            }
            None => Task::none(),
        },
        DocMsg::Stop(task) => runner.run(
            move |context| async move { crate::task_manager::stop(&context, task).await },
            |said| Msg::WorkStopped(said.and_then(|said| said.map_err(|e| e.message))),
        ),
        DocMsg::Ask(kind, thread) => match docs.comment(kind, thread, true) {
            Some(question) => {
                let passage = question.comment.passage.clone();
                state.questions += 1;
                let asking = runner.run(
                    move |context| async move { match_service::ask_about(&context, id, &question).await },
                    move |answer| Msg::Answered {
                        id,
                        kind,
                        passage: passage.clone(),
                        answer: Arc::new(answer.and_then(|answer| answer.map_err(|e| e.message))),
                    },
                );
                Task::batch([asking, watch_work(state)])
            }
            None => Task::none(),
        },
        DocMsg::Copy(kind, version) => match docs.text_of(kind, version) {
            Some(copied) => {
                state.said = Some((Health::Ready, "Copied to clipboard.".to_string()));
                iced::clipboard::write(copied)
            }
            None => Task::none(),
        },
        DocMsg::SavePdf(kind, version) => {
            let edited = docs.text_of(kind, None).unwrap_or_default();
            save_pdf(state, id, kind, edited, version, runner)
        }
        DocMsg::Open(match_id) => go(state, Page::Saved(match_id), runner),
        DocMsg::OpenLanguageModels => Task::done(Msg::ShowLanguageModels),
        msg => {
            docs.update(msg);
            Task::none()
        }
    }
}

/// Writes saved match id's kind with notes, or revises it on revision's threads.
fn write_document(
    state: &mut State,
    id: i64,
    notes: String,
    kind: &'static str,
    revision: Option<Revision>,
    runner: &Runner,
) -> Task<Msg> {
    if let Some(docs) = shown_mut(state).and_then(|shown| shown.docs.as_mut()) {
        docs.set_writing(Some(kind));
    }
    state.busy = true;
    state.said = Some((Health::Neutral, "Writing…".to_string()));
    let writing =
        runner.run_saying(
            move |context, say| async move {
                match_service::write_document(context, id, &notes, kind, revision, &*say).await
            },
            move |step| match step {
                Step::Said(line) => Msg::Matching(line),
                Step::Done(answer) => Msg::Wrote {
                    id,
                    answer: Arc::new(answer.and_then(|answer| answer.map_err(|e| e.message))),
                },
            },
        );
    Task::batch([writing, watch_work(state)])
}

/// Starts reading the window's writing in the task manager, unless it is read already.
fn watch_work(state: &mut State) -> Task<Msg> {
    if state.watching {
        return Task::none();
    }
    state.watching = true;
    Task::done(Msg::ReadWork)
}

/// Saved match id's kind as a PDF, edited being its text as shown (empty: the saved text), or its
/// earlier version.
fn save_pdf(
    state: &mut State,
    id: i64,
    kind: &'static str,
    edited: String,
    version: Option<i64>,
    runner: &Runner,
) -> Task<Msg> {
    state.busy = true;
    runner.run(
        move |context| async move { documents::save_pdf(context, id, kind, edited, version).await },
        |saved| {
            Msg::PdfSaved(saved.and_then(|saved| {
                saved
                    .map(|path| path.map(|path| path.display().to_string()))
                    .map_err(|e| e.message)
            }))
        },
    )
}

/// A click in saved match id's Apply or Jobnet joblog: a PDF is saved here, with the text as
/// edited in its box; the rest is the sections' own.
fn applying(state: &mut State, id: i64, view: u64, msg: ApplyMsg, runner: &Runner) -> Task<Msg> {
    // A status saved is the match's: every other result of it shows it too, even when the one
    // that saved it has gone since.
    if let ApplyMsg::Saved(Ok((status, logged))) = &msg {
        for other in results_of(state, id) {
            // The footer's Mark as applied or Applied day follows the status saved.
            other.result.applied_on = logged.applied_on.clone();
            if let Some(sections) = other.applying.as_mut().filter(|a| a.view() != view) {
                sections.joblog_saved(status, logged);
            }
        }
    }
    // The result whose sections it came from, whichever page is open now, so an answer to a
    // section left while it read or saved still reaches it, and no other.
    let Some(shown) = results_of(state, id).find(|shown| shown.applying.as_ref().is_some_and(|a| a.view() == view))
    else {
        return Task::none();
    };
    let edited = |kind: &str| shown.docs.as_ref().and_then(|docs| docs.text_of(kind, None));
    if let ApplyMsg::SavePdf(kind) = msg {
        let text = if apply::is_written(kind) {
            edited(kind).unwrap_or_default()
        } else {
            String::new()
        };
        return save_pdf(state, id, kind, text, None, runner);
    }
    let application = edited(APPLICATION);
    let Some(sections) = shown.applying.as_mut() else {
        return Task::none();
    };
    let (task, said) = sections.update(msg, runner, application);
    if said.is_some() {
        state.said = said;
    }
    task.map(move |msg| Msg::Apply(id, view, msg))
}

/// The results of saved match id the window holds: the last match's, the one open on its page,
/// and the ranked rows kept while another is open, so an answer to a read or save that one
/// started still reaches it.
fn results_of(state: &mut State, id: i64) -> impl Iterator<Item = &mut Shown> {
    let saved = state.saved.as_mut().and_then(|saved| saved.as_mut().ok());
    let kept = state.ranked_shown.iter_mut().flatten();
    state
        .fresh
        .as_mut()
        .into_iter()
        .chain(saved)
        .chain(kept)
        .filter(move |shown| shown.result.saved_id == Some(id))
}

fn reject(state: &mut State, undo: bool, runner: &Runner) -> Task<Msg> {
    let Some(shown) = shown_mut(state) else {
        return Task::none();
    };
    let Some(id) = shown.result.saved_id else {
        return Task::none();
    };
    let reasons: Vec<String> = if undo {
        Vec::new()
    } else {
        shown.ticked.iter().map(|reason| reason.to_string()).collect()
    };
    let notes: Vec<(String, String)> = if undo {
        Vec::new()
    } else {
        shown
            .notes
            .iter()
            .map(|(reason, note)| (reason.to_string(), note.clone()))
            .collect()
    };
    let rejections = Arc::new(reject_reason::rows(&reasons, &notes));
    state.busy = true;
    runner.run(
        move |context| async move { match_service::reject(&context, id, &reasons, &notes, undo).await },
        move |outcome| Msg::Rejected {
            id,
            undo,
            rejections: Arc::clone(&rejections),
            outcome: outcome.and_then(|outcome| outcome.map_err(|e| e.message)),
        },
    )
}

/// A CV or job post from a file: a PDF's text (read_pdf) or a text file's; None when the dialog
/// was closed.
pub(super) async fn pick_file(context: Arc<Context>) -> Result<Picked, ServiceError> {
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title("Choose a PDF or text file")
        .add_filter("PDF or text", &FILE_KINDS)
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    let name = file.file_name();
    let bytes = file.read().await;
    if bytes.len() > match_service::MAX_PDF_BYTES {
        return Err(ServiceError::new(
            ErrorKind::TooLarge,
            format!("{name} is over {} MB.", match_service::MAX_PDF_BYTES / (1024 * 1024)),
        ));
    }
    let text = if bytes.starts_with(PDF_START) {
        match_service::read_pdf(&context, bytes).await?
    } else {
        String::from_utf8(bytes)
            .map_err(|_| ServiceError::new(ErrorKind::Invalid, format!("{name} is neither a PDF nor a text file.")))?
    };
    Ok(Some((name, text)))
}

// ---------- the pages ----------

pub fn view(state: &State) -> Element<'_, Msg> {
    let mut screen = Column::new().spacing(GAP * 1.5);
    screen = screen.push(back_button(state));
    match state.page {
        // The shell shows the overview.
        Page::Overview => {}
        Page::New => screen = screen.push(new_view(state)),
        Page::List(list) => screen = screen.push(list_view(state, list)),
        Page::Saved(_) => screen = screen.push(saved_view(state)),
    }
    screen.into()
}

fn back_button(state: &State) -> Element<'_, Msg> {
    let (label, to) = match (state.page, state.back) {
        (Page::Saved(_), Page::List(list)) => ("‹ Saved matches", Page::List(list)),
        (Page::Saved(_), Page::New) => ("‹ Match a job", Page::New),
        _ => ("‹ Overview", Page::Overview),
    };
    button(text(label).size(style::BODY_SIZE))
        .padding([4.0, 8.0])
        .style(style::subtle_button(false))
        .on_press(Msg::Go(to))
        .into()
}

/// The step running or what the last click came to.
fn said_bar(state: &State) -> Option<Element<'_, Msg>> {
    state
        .said
        .as_ref()
        .map(|(health, said)| widgets::message_bar(*health, said.as_str()))
}

fn new_view(state: &State) -> Element<'_, Msg> {
    let mut page = Column::new().spacing(GAP * 1.5).push(widgets::page_title(
        "Match a job",
        widgets::note("Paste a link to a job post, its text, or choose a file, and match it with your CV."),
    ));
    page = page.push(widgets::section("Job post", job_card(state)));
    for (index, job) in state.more_jobs.iter().enumerate() {
        page = page.push(more_job_card(state, index, job));
    }
    page = page.push(widgets::section("CV", cv_card(state)));
    let jobs = state.more_jobs.len() + 1;
    let match_label = if jobs > 1 {
        format!("Match {jobs} jobs")
    } else {
        "Match".to_string()
    };
    let mut buttons = row![
        button(text(match_label).size(style::BODY_SIZE))
            .padding([8.0, 24.0])
            .style(style::primary_button)
            .on_press_maybe((!state.busy).then_some(Msg::Match)),
        button(text("Add another job post").size(style::BODY_SIZE))
            .padding([8.0, 16.0])
            .style(style::standard_button)
            .on_press_maybe((!state.busy && jobs < MAX_JOBS).then_some(Msg::AddJob)),
    ]
    .spacing(8)
    .align_y(Center);
    if jobs > 1 {
        buttons = buttons.push(widgets::caption(format!(
            "{jobs} of {MAX_JOBS}. Each is matched in turn with the same CV; with an AI model, each is a few model calls."
        )));
    }
    page = page.push(buttons);
    if let Some(bar) = said_bar(state) {
        page = page.push(bar);
    }
    if let Some(batch) = &state.batch {
        page = page.push(batch::view(batch));
    }
    if let Some(shown) = &state.fresh {
        // Only a saved match's page has the footer, so a fresh result keeps its Open job post.
        page = page.push(result_view(
            shown,
            state.busy,
            &state.working,
            job_post_button(&shown.result.job_url),
        ));
    }
    page.into()
}

fn job_card(state: &State) -> Element<'_, Msg> {
    let link = row![
        text_input("https://…", &state.job_url)
            .on_input(Msg::JobUrl)
            .on_submit(Msg::Fetch)
            .size(style::BODY_SIZE)
            .padding(8)
            .style(style::text_field)
            .width(Fill),
        button(text("Fetch").size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press_maybe((!state.busy).then_some(Msg::Fetch)),
    ]
    .spacing(8)
    .align_y(Center);
    let mut body = column![
        widgets::field("Link to the post", link.into(), None),
        widgets::field(
            "The post's text",
            text_editor(&state.job)
                .placeholder("Paste the job post here, or a link to it")
                .on_action(Msg::JobEdit)
                .size(style::BODY_SIZE)
                .padding(8)
                .height(JOB_HEIGHT)
                .style(style::text_area)
                .into(),
            None,
        ),
    ]
    .spacing(GAP);
    body = body.push(file_row(state, Target::Job, state.job_file.as_deref()));
    container(body).padding(CARD_PADDING).width(Fill).into()
}

/// Another job post's box, with Remove.
fn more_job_card<'a>(state: &'a State, index: usize, job: &'a MoreJob) -> Element<'a, Msg> {
    let heading = row![
        widgets::section_title(format!("Job post {}", index + 2)),
        iced::widget::space().width(Fill),
        button(text("Remove").size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press_maybe((!state.busy).then_some(Msg::RemoveJob(index))),
    ]
    .align_y(Center);
    let editor = text_editor(&job.text)
        .placeholder("Paste the job post here, or a link to it")
        .on_action(move |action| Msg::MoreJobEdit(index, action))
        .size(style::BODY_SIZE)
        .padding(8)
        .height(MORE_JOB_HEIGHT)
        .style(style::text_area);
    let mut body = column![editor].spacing(8);
    if !job.url.is_empty() {
        body = body.push(widgets::caption(format!("Fetched from {}", job.url)));
    }
    column![
        heading,
        widgets::card(container(body).padding(CARD_PADDING).width(Fill).into())
    ]
    .spacing(10)
    .into()
}

fn cv_card(state: &State) -> Element<'_, Msg> {
    let cv = &state.cv;
    let mut body = Column::new().spacing(GAP);
    match &cv.profile {
        None => body = body.push(widgets::caption("Reading your profile…")),
        Some(Some(_)) if !cv.own => {
            body = body.push(
                row![
                    style::mark(Health::Ready),
                    column![
                        text("Your profile from setup")
                            .font(style::semibold())
                            .size(style::BODY_SIZE),
                        widgets::caption(read_cv_line(cv)),
                    ]
                    .spacing(2)
                    .width(Fill),
                    button(text("Use another CV").size(style::BODY_SIZE))
                        .style(style::standard_button)
                        .on_press_maybe((!state.busy).then_some(Msg::UseOwnCv(true))),
                ]
                .spacing(14)
                .align_y(Center),
            );
        }
        Some(profile) => {
            if profile.is_some() {
                body = body.push(
                    row![
                        text("Matching with the CV below instead of your profile.")
                            .size(style::CAPTION_SIZE)
                            .style(style::secondary)
                            .width(Fill),
                        button(text("Use my profile").size(style::BODY_SIZE))
                            .style(style::standard_button)
                            .on_press_maybe((!state.busy).then_some(Msg::UseOwnCv(false))),
                    ]
                    .spacing(14)
                    .align_y(Center),
                );
            } else {
                body = body.push(widgets::caption(
                    "There's no profile from setup yet, so paste a CV or choose its file.",
                ));
            }
            body = body
                .push(
                    text_editor(&cv.text)
                        .placeholder("Paste your CV here")
                        .on_action(Msg::CvEdit)
                        .size(style::BODY_SIZE)
                        .padding(8)
                        .height(CV_HEIGHT)
                        .style(style::text_area),
                )
                .push(file_row(state, Target::Cv, cv.file_name.as_deref()));
            if !cv.uses_profile() && cv.read.as_ref().is_some_and(|read| read.text == cv.text().trim()) {
                body = body.push(widgets::caption(read_cv_line(cv)));
            }
        }
    }
    container(body).padding(CARD_PADDING).width(Fill).into()
}

/// What was read in the CV, or when it is read.
fn read_cv_line(cv: &Cv) -> String {
    match cv.read.as_ref().filter(|read| read.text == cv.text().trim()) {
        Some(read) => {
            let profile = &read.profile;
            let name = if profile.name.is_empty() {
                "CV"
            } else {
                profile.name.as_str()
            };
            format!(
                "{name}: {} years, {} skills.",
                profile.experience_years,
                profile.skill.len()
            )
        }
        None => "It's read when you match.".to_string(),
    }
}

/// Choose file, and the file chosen.
fn file_row<'a>(state: &'a State, target: Target, file_name: Option<&'a str>) -> Element<'a, Msg> {
    let mut line = row![
        button(text("Choose file…").size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press_maybe((!state.busy).then_some(Msg::ChooseFile(target)))
    ]
    .spacing(10)
    .align_y(Center);
    line = line.push(widgets::caption(match file_name {
        Some(name) => format!("From {name}"),
        None => "A PDF or a text file.".to_string(),
    }));
    line.into()
}

fn list_view<'a>(state: &'a State, current: &'static str) -> Element<'a, Msg> {
    let tabs = Row::with_children(LISTS.iter().map(|(list, label, _)| {
        button(text(*label).size(style::BODY_SIZE))
            .padding([6.0, 12.0])
            .style(style::subtle_button(*list == current))
            .on_press(Msg::Go(Page::List(list)))
            .into()
    }))
    .spacing(4)
    .wrap();
    let check_open = button(text("Check which are still open").size(style::BODY_SIZE))
        .style(style::standard_button)
        .on_press_maybe((!state.busy).then_some(Msg::CheckOpen));
    let sort_select = pick_list(
        [SortOrder::Best, SortOrder::Newest, SortOrder::Oldest],
        Some(state.list.sort),
        Msg::Sort,
    )
    .text_size(style::BODY_SIZE)
    .style(style::pick_field);
    let mut page = Column::new()
        .spacing(GAP * 1.5)
        .push(widgets::page_title(
            "Saved matches",
            widgets::note("Open a match to see its verdicts, or to say you're not interested."),
        ))
        .push(
            row![container(tabs).width(Fill), sort_select, check_open]
                .spacing(12)
                .align_y(Center),
        );
    if let Some(bar) = said_bar(state) {
        page = page.push(bar);
    }
    let body: Element<'_, Msg> = match &state.list.read {
        None => widgets::note("Reading the saved matches…"),
        Some(Err(why)) => widgets::message_bar(Health::Attention, why.as_str()),
        Some(Ok(())) if state.list.rows.is_empty() => {
            widgets::card(drawings::empty(&drawings::FINDS, state.list.empty))
        }
        Some(Ok(())) => widgets::card(widgets::rows(state.list.rows.iter().map(saved_row))),
    };
    page = page.push(body);
    if state.list.next_after.is_some() && matches!(state.list.read, Some(Ok(()))) {
        page = page.push(
            button(text("Show more").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press(Msg::ShowMore),
        );
    }
    page.into()
}

/// A saved match in a list, opening its result.
fn saved_row(saved: &SavedMatchRow) -> Element<'_, Msg> {
    let mut about = column![
        text(saved.title.as_str())
            .font(style::semibold())
            .size(style::BODY_SIZE),
        widgets::caption(saved.about.as_str()),
    ]
    .spacing(2)
    .width(Fill);
    if !saved.done.is_empty() {
        about = about.push(widgets::caption(saved.done.as_str()));
    }
    if !saved.opening.is_empty() {
        about = about.push(widgets::caption(saved.opening.as_str()));
    }
    button(
        row![
            about,
            widgets::caption(saved.saved_on.as_str()),
            text("›").size(style::SECTION_SIZE).style(style::secondary),
        ]
        .spacing(14)
        .align_y(Center),
    )
    .padding(ROW_PADDING)
    .width(Fill)
    .style(style::row_button)
    .on_press(Msg::Go(Page::Saved(saved.id)))
    .into()
}

fn saved_view(state: &State) -> Element<'_, Msg> {
    let mut page = Column::new().spacing(GAP * 1.5);
    if let Some(bar) = said_bar(state) {
        page = page.push(bar);
    }
    page.push(match &state.saved {
        None => widgets::note("Reading the match…"),
        Some(Err(why)) => widgets::message_bar(Health::Attention, why.as_str()),
        Some(Ok(shown)) => column![
            result_view(shown, state.busy, &state.working, None),
            recordings_card::view(&state.recordings).map(Msg::Recordings),
        ]
        .spacing(GAP * 1.5)
        .into(),
    })
    .into()
}

// ---------- a result ----------

/// A result; job_post: its Open job post, for a page without the footer (footer), None on one with.
fn result_view<'a>(
    shown: &'a Shown,
    busy: bool,
    working: &'a Working,
    job_post: Option<Element<'a, Msg>>,
) -> Element<'a, Msg> {
    let result = &shown.result;
    let mut view = Column::new().spacing(GAP * 1.5).push(score_card(result));
    if let Some(opening) = &result.opening {
        view = view.push(opening_bar(opening, busy));
    }
    if let Some(reject) = &result.reject {
        view = view.push(reject_card(shown, reject, busy));
    }
    view = view.extend(job_post);
    if let Some(disqualifier) = &result.disqualifier {
        let mut lines = column![
            text(disqualifier.heading)
                .font(style::semibold())
                .size(style::BODY_SIZE)
        ]
        .spacing(4);
        for line in &disqualifier.lines {
            lines = lines.push(text(join_reason(&line.text, &line.reason)).size(style::BODY_SIZE));
        }
        lines = lines.push(widgets::caption(disqualifier.note));
        view = view.push(
            container(row![style::mark(Health::Attention), lines.width(Fill)].spacing(12))
                .padding([12.0, CARD_PADDING])
                .width(Fill)
                .style(style::warning_bar),
        );
    }
    if let Some(apply) = &result.apply {
        let mut lines = column![text(apply.heading).font(style::semibold()).size(style::BODY_SIZE)].spacing(4);
        if !apply.note.is_empty() {
            lines = lines.push(widgets::caption(apply.note.as_str()));
        }
        for (label, value) in [
            ("Send it to", &apply.email),
            ("Subject line:", &apply.subject),
            ("Include:", &apply.keyword),
        ] {
            if !value.is_empty() {
                lines = lines.push(text(format!("{label} {value}")).size(style::BODY_SIZE));
            }
        }
        if !apply.asks.is_empty() {
            lines = lines.push(text(apply.asks.as_str()).size(style::BODY_SIZE));
        }
        for line in &apply.lines {
            lines = lines.push(widgets::caption(format!("“{line}”")));
        }
        let health = if apply.trap { Health::Attention } else { Health::Neutral };
        view = view.push(
            container(row![style::mark(health), lines.width(Fill)].spacing(12))
                .padding([12.0, CARD_PADDING])
                .width(Fill)
                .style(if apply.trap { style::warning_bar } else { style::card }),
        );
    }
    if !result.filters.is_empty() {
        let filters = Row::with_children(result.filters.iter().map(|filter| {
            button(text(format!("{} {}", filter.label, filter.count)).size(style::BODY_SIZE))
                .padding([6.0, 12.0])
                .style(style::subtle_button(filter.key == shown.filter))
                .on_press(Msg::Filter(filter.key))
                .into()
        }))
        .spacing(4)
        .wrap();
        view = view.push(filters);
        for section in &result.sections {
            if let Some(card) = section_card(section, shown.filter) {
                view = view.push(card);
            }
        }
    }
    if !result.soft_skills.is_empty() {
        let chips = Row::with_children(result.soft_skills.iter().map(|skill| {
            container(text(skill.as_str()).size(style::CAPTION_SIZE))
                .padding([2.0, 10.0])
                .style(style::chip(Health::Neutral))
                .into()
        }))
        .spacing(6)
        .wrap();
        view = view.push(
            column![
                row![widgets::section_title("Soft skills"), widgets::caption("not scored")]
                    .spacing(8)
                    .align_y(Center),
                chips,
            ]
            .spacing(10),
        );
    }
    if !result.laya.text.is_empty() {
        view = view.push(if result.laya.failed {
            widgets::caption(result.laya.text.as_str())
        } else {
            text(result.laya.text.as_str()).size(style::BODY_SIZE).into()
        });
    }
    if let (Some(docs), Some(id)) = (&shown.docs, result.saved_id) {
        view = view.push(widgets::section(
            "Application and résumé",
            documents::view(docs, busy, working).map(move |msg| Msg::Doc(id, msg)),
        ));
    }
    if let Some(applying) = &shown.applying {
        let id = applying.id();
        let sections = applying.view();
        view = view.push(apply::view(applying, busy).map(move |msg| Msg::Apply(id, sections, msg)));
    }
    view.into()
}

/// The score, the job and how the score is made up.
fn score_card(result: &MatchResult) -> Element<'_, Msg> {
    let score = &result.score;
    let mut under = row![
        container(text(score.tag_name.as_str()).size(style::CAPTION_SIZE))
            .padding([2.0, 10.0])
            .style(style::chip(tag_health(&score.tag))),
    ]
    .spacing(10)
    .align_y(Center);
    if let Some(day) = result.saved_at.get(..10) {
        under = under.push(widgets::caption(format!("saved {day}")));
    }
    widgets::card(
        container(
            row![
                text(format!("{}%", score.percent))
                    .font(style::semibold())
                    .size(SCORE_SIZE),
                column![
                    text(if result.heading.is_empty() {
                        "Untitled"
                    } else {
                        result.heading.as_str()
                    })
                    .font(style::semibold())
                    .size(style::SECTION_SIZE),
                    widgets::caption(result.parts.as_str()),
                    under,
                ]
                .spacing(4)
                .width(Fill),
            ]
            .spacing(20)
            .align_y(Center),
        )
        .padding(CARD_PADDING)
        .into(),
    )
}

/// A score tag's colours: top and good are good news, weak is not.
pub(super) fn tag_health(tag: &str) -> Health {
    match tag {
        "top" | "good" => Health::Ready,
        "poor" => Health::Attention,
        _ => Health::Neutral,
    }
}

/// Whether a saved match's job still takes applications; a closed one can be said to be open.
fn opening_bar(opening: &OpeningLine, busy: bool) -> Element<'_, Msg> {
    let mut line = row![
        style::mark(if opening.closed {
            Health::Attention
        } else {
            Health::Neutral
        }),
        text(opening.text.as_str()).size(style::BODY_SIZE).width(Fill),
    ]
    .spacing(12)
    .align_y(Center);
    if opening.closed {
        line = line.push(
            button(text("It's still open").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press_maybe((!busy).then_some(Msg::KeepOpen)),
        );
    }
    container(line)
        .padding([12.0, CARD_PADDING])
        .width(Fill)
        .style(if opening.closed {
            style::warning_bar
        } else {
            style::card
        })
        .into()
}

/// "Open job post", for a result whose post was read from a web page; None for a pasted post.
fn job_post_button<'a>(job_url: &str) -> Option<Element<'a, Msg>> {
    let url = job_url.trim();
    is_web_link(url).then(|| {
        button(text(OPEN_JOB_POST).size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press(Msg::OpenJobPost(url.to_string()))
            .into()
    })
}

/// The match details' footer, under the page: the application's and résumé's PDFs, the job post,
/// Copy as prompt, and Mark as applied, or the day it was applied for. None off a saved match's page.
pub fn footer(state: &State) -> Option<Element<'_, Msg>> {
    let Page::Saved(_) = state.page else {
        return None;
    };
    let shown = state.saved.as_ref()?.as_ref().ok()?;
    let id = shown.result.saved_id?;
    let busy = state.busy;
    // Mark as applied leads; the row wraps onto more lines in a narrow window.
    let mut actions = Row::new()
        .spacing(8)
        .align_y(Center)
        .push(applied_action(&shown.result.applied_on, busy));
    for (label, kind) in FOOTER_PDFS {
        let save = Msg::Doc(id, DocMsg::SavePdf(kind, None));
        actions = actions.push(
            button(text(label).size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press_maybe((!busy && is_written(shown, kind)).then_some(save)),
        );
    }
    actions = actions.extend(job_post_button(&shown.result.job_url));
    actions = actions.push(
        button(text(COPY_PROMPT).size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press_maybe((!busy).then_some(Msg::CopyPrompt)),
    );
    Some(actions.wrap().vertical_spacing(8).into())
}

/// The footer's PDF buttons: their labels and the kind each saves.
const FOOTER_PDFS: [(&str, &str); 2] = [("Application PDF…", APPLICATION), ("Résumé PDF…", RESUME)];

/// The label of the footer's button that copies the match as a prompt for a chatbot.
const COPY_PROMPT: &str = "Copy as prompt";

/// The shown match's kind as in its box; None when it isn't written.
fn written_text(shown: &Shown, kind: &str) -> Option<String> {
    shown
        .docs
        .as_ref()
        .and_then(|docs| docs.text_of(kind, None))
        .filter(|text| !text.trim().is_empty())
}

/// Whether the shown match has kind written, so its PDF has a text.
fn is_written(shown: &Shown, kind: &str) -> bool {
    written_text(shown, kind).is_some()
}

/// Mark as applied, or the day it was applied for (applied_on, dd-mm-yyyy) once it is.
fn applied_action<'a>(applied_on: &str, busy: bool) -> Element<'a, Msg> {
    if applied_on.is_empty() {
        return button(text(MARK_APPLIED).size(style::BODY_SIZE))
            .padding([6.0, 18.0])
            .style(style::primary_button)
            .on_press_maybe((!busy).then_some(Msg::MarkApplied))
            .into();
    }
    row![
        style::mark(Health::Ready),
        text(format!("Applied {applied_on}")).size(style::BODY_SIZE),
    ]
    .spacing(8)
    .align_y(Center)
    .into()
}

/// The label of the footer's button that marks the match as applied for.
const MARK_APPLIED: &str = "Mark as applied";

/// What Mark as applied says it did: status as Jobnet has it (Soegt), and the day.
fn marked_applied(status: &str, applied_on: &str) -> String {
    let status = joblog::search_status(status).unwrap_or(status);
    if applied_on.is_empty() {
        format!("Marked as applied: {status}.")
    } else {
        format!("Marked as applied: {status}, {applied_on}.")
    }
}

/// The label of the button that opens the job post in the browser.
const OPEN_JOB_POST: &str = "Open job post";

/// Whether a link is a web page's (http or https), so a stray word in the field opens nothing.
fn is_web_link(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"))
}

/// Not interested, the form it opens, or the rejected match's Bring back.
fn reject_card<'a>(shown: &'a Shown, reject: &'a RejectButton, busy: bool) -> Element<'a, Msg> {
    if reject.rejected {
        return widgets::card(
            container(
                row![
                    text(reject.note.as_str())
                        .size(style::CAPTION_SIZE)
                        .style(style::secondary)
                        .width(Fill),
                    button(text(reject.submit).size(style::BODY_SIZE))
                        .style(style::standard_button)
                        .on_press_maybe((!busy).then_some(Msg::BringBack)),
                ]
                .spacing(14)
                .align_y(Center),
            )
            .padding(ROW_PADDING)
            .into(),
        );
    }
    if !shown.rejecting {
        return row![
            button(text(reject.open).size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press_maybe((!busy).then_some(Msg::OpenReject(true)))
        ]
        .into();
    }
    let mut form = column![
        text("Why not? Tick all that apply")
            .font(style::semibold())
            .size(style::BODY_SIZE)
    ]
    .spacing(8);
    for reason in &reject.reasons {
        let ticked = shown.ticked.contains(&reason.value);
        let value = reason.value;
        form = form.push(
            row![
                checkbox(ticked)
                    .label(reason.label)
                    .text_size(style::BODY_SIZE)
                    .style(style::check_box)
                    .on_toggle(move |on| Msg::Tick(value, on))
                    .width(REASON_WIDTH),
                text_input(reason.placeholder, shown.note(value))
                    .on_input(move |note| Msg::Note(value, note))
                    .size(style::BODY_SIZE)
                    .padding(6)
                    .style(style::text_field)
                    .width(Fill),
            ]
            .spacing(12)
            .align_y(Center),
        );
    }
    if let Some(problem) = &shown.problem {
        form = form.push(
            text(problem.as_str())
                .size(style::CAPTION_SIZE)
                .style(style::health_text(Health::Attention)),
        );
    }
    form = form.push(
        row![
            button(text(reject_button::REJECT).size(style::BODY_SIZE))
                .style(style::danger_button)
                .on_press_maybe((!busy).then_some(Msg::Reject)),
            button(text("Cancel").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press(Msg::OpenReject(false)),
        ]
        .spacing(8),
    );
    widgets::card(container(form).padding(CARD_PADDING).into())
}

/// The width of a reason's checkbox, so the notes line up; room for the longest label,
/// "Already applied to this position", on one line.
const REASON_WIDTH: f32 = 260.0;

/// A group of requirements with the rows the filter shows; None when it shows none.
fn section_card<'a>(section: &'a Section, filter: &'static str) -> Option<Element<'a, Msg>> {
    let rows: Vec<&VerdictRow> = section
        .rows
        .iter()
        .filter(|row| filter == ALL || row.verdict == filter)
        .collect();
    if rows.is_empty() {
        return None;
    }
    let mut heading = row![widgets::section_title(section.heading.as_str())]
        .spacing(8)
        .align_y(Center);
    if !section.note.is_empty() {
        heading = heading.push(widgets::caption(section.note.as_str()));
    }
    Some(
        column![
            heading,
            widgets::card(widgets::rows(rows.into_iter().map(|row| verdict_view(row, 0.0))))
        ]
        .spacing(10)
        .into(),
    )
}

/// The verdict chip's column, so the requirements line up.
const VERDICT_WIDTH: f32 = 72.0;
/// How far an alternative is set in under its requirement.
const ALTERNATIVE_INDENT: f32 = 24.0;

/// A requirement: its verdict, text, why, the CV's words and what decided it; its alternatives
/// under it.
fn verdict_view(row: &VerdictRow, indent: f32) -> Element<'_, Msg> {
    let mut body = column![text(row.text.as_str()).font(style::semibold()).size(style::BODY_SIZE)].spacing(2);
    if !row.reason.is_empty() {
        body = body.push(text(row.reason.as_str()).size(style::CAPTION_SIZE));
    }
    if !row.evidence.is_empty() {
        body = body.push(widgets::caption(format!("“{}”", row.evidence)));
    }
    if !row.decided_by.is_empty() {
        body = body.push(widgets::caption(row.decided_by.as_str()));
    }
    let line = iced::widget::row![
        container(
            container(text(row.verdict).size(style::CAPTION_SIZE))
                .padding([2.0, 10.0])
                .style(style::chip(verdict_health(row.verdict)))
        )
        .width(VERDICT_WIDTH),
        body.width(Fill),
    ]
    .spacing(12)
    .padding(iced::Padding::from(ROW_PADDING).left(CARD_PADDING + indent));
    if row.alternatives.is_empty() {
        return line.into();
    }
    Column::with_children(
        std::iter::once(line.into()).chain(
            row.alternatives
                .iter()
                .map(|alternative| verdict_view(alternative, indent + ALTERNATIVE_INDENT)),
        ),
    )
    .into()
}

/// A verdict's colours.
fn verdict_health(verdict: &str) -> Health {
    match verdict {
        "met" => Health::Ready,
        "missing" => Health::Attention,
        _ => Health::Neutral,
    }
}

/// "Located near Copenhagen · Not named in the CV."
fn join_reason(text: &str, reason: &str) -> String {
    if reason.is_empty() {
        text.to_string()
    } else {
        format!("{text} · {reason}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_web_link_opens_the_job_post() {
        assert!(is_web_link("https://www.jobindex.dk/job/1"));
        assert!(is_web_link("http://acme.dk/jobs/1"));
        assert!(!is_web_link(""));
        assert!(!is_web_link("pasted text"));
        assert!(!is_web_link("file:///C:/job.pdf"));
        assert!(job_post_button("  ").is_none());
        assert_eq!(
            marked_applied("Soegt", "04-11-2026"),
            "Marked as applied: Søgt, 04-11-2026."
        );
        assert_eq!(marked_applied("Samtale", ""), "Marked as applied: Samtale.");
    }

    #[test]
    fn an_answer_to_an_older_read_is_dropped() {
        let old = Msg::SavedLoaded {
            request: 1,
            result: Err("old".to_string()),
        };
        assert!(is_stale(2, &old));
        assert!(!is_stale(1, &old));
        assert!(!is_stale(2, &Msg::Match));
    }

    #[test]
    fn work_waits_for_the_work_running() {
        assert!(starts_work(&Msg::Fetch));
        assert!(starts_work(&Msg::Match));
        assert!(!starts_work(&Msg::JobUrl(String::new())));
        assert!(!starts_work(&Msg::Matching(String::new())));
    }

    #[test]
    fn the_host_is_named_without_its_path() {
        assert_eq!(host_of("https://www.jobindex.dk/job/1?x=2"), "www.jobindex.dk");
        assert_eq!(host_of("https://jobnet.dk"), "jobnet.dk");
    }

    #[test]
    fn the_cv_matched_is_the_profile_until_another_is_chosen() {
        let mut cv = Cv {
            profile: Some(Some("Profile text".to_string())),
            text: text_editor::Content::with_text("Pasted CV"),
            ..Cv::default()
        };
        assert_eq!(cv.text(), "Profile text");
        cv.own = true;
        assert!(cv.text().starts_with("Pasted CV"));
        let without = Cv {
            profile: Some(None),
            text: text_editor::Content::with_text("Pasted CV"),
            ..Cv::default()
        };
        assert!(without.text().starts_with("Pasted CV"));
    }
}
