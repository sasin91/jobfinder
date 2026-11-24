//! Applying for a saved match in the window:
//! - Apply: how the post asks for the application (by email, or on the employer's site), each
//!   value to copy, the PDFs to attach or upload, the profile links, and the email app or the job
//!   post to open (match_service::application_delivery);
//! - Jobnet joblog: Jobnet's fields in its order to copy over, how far the candidate is (saved
//!   with the match), the three PDFs, the "Copy as AI prompt" fallback, and whether it is in their
//!   joblog (match_service::joblog_panel, save_joblog_status). A post with no postcode and town
//!   gets the company's registered address from the CVR register (cvr), marked as such.
//!
//! Both are read when first opened. The PDFs are the screen's (matching): they use the
//! application and résumé as edited in their boxes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use iced::widget::{Column, button, checkbox, column, container, row, text, text_editor, text_input};
use iced::{Center, Element, Fill, Task};

use super::style;
use super::widgets::{self, CARD_PADDING};
use super::{GAP, Runner};
use crate::dashboard::Health;
use crate::matches::application_delivery::{self, ApplicationDelivery, EMAIL};
use crate::matches::cvr::{self, CvrAddress};
use crate::matches::document_box::APPLICATION;
use crate::matches::joblog_panel::{self, CHARACTERS_LEFT, JoblogPanel, Logged, PanelField};
use crate::matches::{joblog, match_service};

/// A long value's text box's height.
const AREA_HEIGHT: f32 = 160.0;
/// The AI prompt's box's height.
const PROMPT_HEIGHT: f32 = 200.0;
/// The joblog fields the CVR register's address fills in, when the post names no postcode.
const COMPANY: &str = "name";
const ADDRESS: &str = "companyAddress";
const POSTCODE: &str = "postalCodeAndCity";
const COUNTRY: &str = "companyCountry";
const COUNTRY_DENMARK: &str = "Danmark";
const UNKNOWN_POSTCODE: &str = "unknownPostalOrCity";
const UNKNOWN_POSTCODE_NO: &str = "no";
/// The Apply fields the email link is made from (application_delivery's ids).
const TO: &str = "delivery-to";
const SUBJECT: &str = "delivery-subject";
/// The joblog's date field, which follows the status when it is saved.
const APPLIED_ON: &str = "submissionDate";

/// The number the next Applying is known by.
static NEXT_VIEW: AtomicU64 = AtomicU64::new(1);

/// A saved match's Apply and Jobnet joblog sections.
pub struct Applying {
    id: i64,
    /// This one's number: the match may show on two pages, each with its own sections, and an
    /// answer goes to the one that asked.
    view: u64,
    apply_open: bool,
    apply: Option<Result<Delivery, String>>,
    /// The number of the newest read of Apply, and whether it is running: an older answer is
    /// dropped, and showing the section again doesn't read it twice.
    apply_read: u64,
    apply_reading: bool,
    /// The one-line values as edited before a document was written, by field id, for the read
    /// after it.
    apply_lines_kept: Vec<(String, String)>,
    joblog_open: bool,
    joblog: Option<Result<Joblog, String>>,
    /// The joblog's first read is running.
    joblog_reading: bool,
    /// A document was written while that read ran: its PDFs are read again once it answers.
    joblog_files_stale: bool,
    /// A status saved from another page while that read ran: newer than what it read.
    joblog_saved_meanwhile: Option<(&'static str, Logged)>,
}

struct Delivery {
    delivery: ApplicationDelivery,
    /// Each field's value as edited, in delivery.fields' order.
    values: Vec<Value>,
}

struct Joblog {
    panel: JoblogPanel,
    /// Each field's value as edited, by section and field; a choice keeps its chosen option.
    values: Vec<Vec<Value>>,
    /// The CVR register's note under the fields it filled.
    cvr_note: Option<String>,
    /// The AI prompt, once copied.
    prompt: Option<text_editor::Content>,
    /// The status is being saved: the choices wait.
    saving: bool,
}

enum Value {
    Line(String),
    Area(text_editor::Content),
}

impl Value {
    fn new(value: &str, multiline: bool) -> Self {
        if multiline {
            Value::Area(text_editor::Content::with_text(value))
        } else {
            Value::Line(value.to_string())
        }
    }

    fn text(&self) -> String {
        match self {
            Value::Line(line) => line.clone(),
            Value::Area(area) => area.text(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ApplyMsg {
    ToggleApply,
    Delivered {
        read: u64,
        answer: Arc<Result<ApplicationDelivery, String>>,
    },
    DeliveryLine(usize, String),
    DeliveryArea(usize, text_editor::Action),
    CopyDelivery(usize),
    CopyLink(usize),
    /// For the screen: a PDF of the match's kind, as edited in its box.
    SavePdf(&'static str),
    /// A PDF that can't be had yet: why.
    Unavailable(&'static str),
    OpenLink(String),
    /// The email app, with To and Subject as edited.
    OpenEmail,
    ToggleJoblog,
    JoblogRead(Arc<Result<(JoblogPanel, Option<CvrAddress>), String>>),
    /// The joblog read again after writing: only its PDFs are new.
    JoblogFiles(Arc<Result<JoblogPanel, String>>),
    JoblogLine(usize, usize, String),
    JoblogArea(usize, usize, text_editor::Action),
    CopyJoblog(usize, usize),
    /// How far the candidate is: a status from joblog::SEARCH_STATUSES.
    Status(&'static str),
    Logged(bool),
    /// The status saved, and whether it is in the joblog.
    Saved(Result<(&'static str, Logged), String>),
    CopyPrompt,
    Prompt(text_editor::Action),
}

/// What an update says on the screen's message bar.
pub type Said = Option<(Health, String)>;

impl Applying {
    pub fn new(id: i64) -> Self {
        Applying {
            id,
            view: NEXT_VIEW.fetch_add(1, Ordering::Relaxed),
            apply_open: false,
            apply: None,
            apply_read: 0,
            apply_reading: false,
            apply_lines_kept: Vec::new(),
            joblog_open: false,
            joblog: None,
            joblog_reading: false,
            joblog_files_stale: false,
            joblog_saved_meanwhile: None,
        }
    }

    pub fn id(&self) -> i64 {
        self.id
    }

    pub fn view(&self) -> u64 {
        self.view
    }

    /// A document was written: the PDFs and the application to send are read again when next
    /// shown.
    pub fn written(&mut self, runner: &Runner) -> Task<ApplyMsg> {
        // The one-line values edited (To, Subject) stay: only a document changed.
        if let Some(delivery) = self.delivery_mut() {
            self.apply_lines_kept = delivery.lines();
        }
        self.apply = None;
        // A read from before the writing is out of date.
        self.apply_read += 1;
        self.apply_reading = false;
        let delivery = if self.apply_open {
            self.read_delivery(runner)
        } else {
            Task::none()
        };
        // The joblog's status, edits and address stay; only its PDFs are read again.
        let joblog = if matches!(self.joblog, Some(Ok(_))) {
            self.read_joblog_files(runner)
        } else {
            self.joblog_files_stale = self.joblog_reading;
            Task::none()
        };
        Task::batch([delivery, joblog])
    }

    fn read_joblog_files(&self, runner: &Runner) -> Task<ApplyMsg> {
        let id = self.id;
        runner.run(
            move |context| async move { match_service::joblog_panel(&context, id).await },
            |read| ApplyMsg::JoblogFiles(Arc::new(read.and_then(|read| read.map_err(|e| e.message)))),
        )
    }

    /// The joblog's status, saved from another page showing the same match.
    pub fn joblog_saved(&mut self, status: &'static str, logged: &Logged) {
        if self.joblog_reading {
            self.joblog_saved_meanwhile = Some((status, logged.clone()));
        } else if let Some(joblog) = self.joblog_mut() {
            joblog.saved(status, logged.clone());
        }
    }

    fn read_delivery(&mut self, runner: &Runner) -> Task<ApplyMsg> {
        self.apply_read += 1;
        self.apply_reading = true;
        let (id, read) = (self.id, self.apply_read);
        runner.run(
            move |context| async move { match_service::application_delivery(&context, id).await },
            move |answer| ApplyMsg::Delivered {
                read,
                answer: Arc::new(answer.and_then(|answer| answer.map_err(|e| e.message))),
            },
        )
    }

    /// application: the application as edited in its box, for the email's body.
    pub fn update(&mut self, msg: ApplyMsg, runner: &Runner, application: Option<String>) -> (Task<ApplyMsg>, Said) {
        let copied = |value: String| {
            (
                iced::clipboard::write(value),
                Some((Health::Ready, "Copied to clipboard.".to_string())),
            )
        };
        match msg {
            ApplyMsg::ToggleApply => {
                self.apply_open = !self.apply_open;
                if self.apply_open && self.apply.is_none() {
                    let read = if self.apply_reading {
                        Task::none()
                    } else {
                        self.read_delivery(runner)
                    };
                    return (read, None);
                }
                // The application may have been edited in its box while this was hidden.
                if self.apply_open
                    && let Some(delivery) = self.delivery_mut()
                {
                    delivery.with_application(application);
                }
            }
            ApplyMsg::Delivered { read, answer } => {
                if read != self.apply_read {
                    return (Task::none(), None);
                }
                self.apply_reading = false;
                let kept = std::mem::take(&mut self.apply_lines_kept);
                self.apply = Some(match answer.as_ref() {
                    Ok(delivery) => {
                        let mut shown = Delivery::new(delivery.clone(), application);
                        shown.with_lines(kept);
                        Ok(shown)
                    }
                    Err(why) => Err(why.clone()),
                });
            }
            ApplyMsg::DeliveryLine(n, line) => {
                if let Some(Value::Line(value)) = self.delivery_mut().and_then(|d| d.values.get_mut(n)) {
                    *value = line;
                }
            }
            ApplyMsg::DeliveryArea(n, action) => {
                if let Some(Value::Area(area)) = self.delivery_mut().and_then(|d| d.values.get_mut(n)) {
                    area.perform(action);
                }
            }
            ApplyMsg::CopyDelivery(n) => {
                if let Some(value) = self.delivery_mut().and_then(|d| d.values.get(n)) {
                    return copied(value.text());
                }
            }
            ApplyMsg::CopyLink(n) => {
                if let Some(link) = self.delivery_mut().and_then(|d| d.delivery.links.get(n)) {
                    return copied(link.value.clone());
                }
            }
            ApplyMsg::Unavailable(why) => return (Task::none(), Some((Health::Attention, why.to_string()))),
            ApplyMsg::OpenEmail => {
                if let Some(href) = self.delivery_mut().map(|delivery| delivery.mailto()) {
                    return self.update(ApplyMsg::OpenLink(href), runner, application);
                }
            }
            ApplyMsg::OpenLink(href) => {
                if let Err(why) = super::open_in_browser(&href) {
                    return (Task::none(), Some((Health::Attention, why)));
                }
            }
            ApplyMsg::ToggleJoblog => {
                self.joblog_open = !self.joblog_open;
                if self.joblog_open && self.joblog.is_none() && !self.joblog_reading {
                    self.joblog_reading = true;
                    let id = self.id;
                    return (
                        runner.run(
                            move |context| async move { read_joblog(&context, id).await },
                            |read| ApplyMsg::JoblogRead(Arc::new(read.and_then(|read| read))),
                        ),
                        None,
                    );
                }
            }
            ApplyMsg::JoblogRead(read) => {
                self.joblog_reading = false;
                // Only the first read fills the joblog: later ones would undo its edits.
                if self.joblog.is_none() {
                    self.joblog = Some(match read.as_ref() {
                        Ok((panel, address)) => Ok(Joblog::new(panel.clone(), address.as_ref())),
                        Err(why) => Err(why.clone()),
                    });
                }
                if let Some((status, logged)) = self.joblog_saved_meanwhile.take()
                    && let Some(joblog) = self.joblog_mut()
                {
                    joblog.saved(status, logged);
                }
                if std::mem::take(&mut self.joblog_files_stale) {
                    return (self.read_joblog_files(runner), None);
                }
            }
            ApplyMsg::JoblogFiles(read) => match (read.as_ref(), self.joblog_mut()) {
                (Ok(panel), Some(joblog)) => {
                    joblog.panel.files = panel.files.clone();
                    joblog.panel.missing = panel.missing.clone();
                }
                (Err(why), _) => {
                    return (
                        Task::none(),
                        Some((
                            Health::Attention,
                            format!("The joblog's PDFs couldn't be read again: {why}"),
                        )),
                    );
                }
                _ => {}
            },
            ApplyMsg::JoblogLine(s, f, line) => {
                if let Some(Value::Line(value)) = self.joblog_value(s, f) {
                    *value = line;
                }
            }
            ApplyMsg::JoblogArea(s, f, action) => {
                let limited = self
                    .joblog_mut()
                    .and_then(|joblog| joblog.panel.sections.get(s)?.fields.get(f))
                    .is_some_and(|field| field.left.is_some());
                if let Some(Value::Area(area)) = self.joblog_value(s, f) {
                    area.perform(action);
                    // Jobnet's notes take no more than this: the rest is cut, as the page's box does.
                    if limited {
                        cut_to(area, joblog::MAX_NOTES);
                    }
                }
            }
            ApplyMsg::CopyJoblog(s, f) => {
                if let Some(value) = self.joblog_value(s, f) {
                    return copied(value.text());
                }
            }
            ApplyMsg::Status(status) => return (self.save(Change::Status(status), runner), None),
            ApplyMsg::Logged(logged) => return (self.save(Change::Logged(logged), runner), None),
            ApplyMsg::Saved(saved) => {
                let Some(joblog) = self.joblog_mut() else {
                    return (Task::none(), None);
                };
                joblog.saving = false;
                match saved {
                    Ok((status, logged)) => {
                        joblog.saved(status, logged);
                        return (Task::none(), Some((Health::Ready, "Saved with the match.".to_string())));
                    }
                    Err(why) => {
                        return (
                            Task::none(),
                            Some((Health::Attention, format!("Couldn't save it: {why}"))),
                        );
                    }
                }
            }
            ApplyMsg::CopyPrompt => {
                if let Some(joblog) = self.joblog_mut() {
                    let prompt = joblog.ai_prompt();
                    joblog.prompt = Some(text_editor::Content::with_text(&prompt));
                    return copied(prompt);
                }
            }
            ApplyMsg::Prompt(action) => {
                // Read only: it can be selected and copied, not changed.
                if let Some(prompt) = self.joblog_mut().and_then(|joblog| joblog.prompt.as_mut())
                    && !action.is_edit()
                {
                    prompt.perform(action);
                }
            }
            ApplyMsg::SavePdf(_) => {}
        }
        (Task::none(), None)
    }

    fn save(&mut self, change: Change, runner: &Runner) -> Task<ApplyMsg> {
        let Some(joblog) = self.joblog_mut().filter(|joblog| !joblog.saving) else {
            return Task::none();
        };
        joblog.saving = true;
        let id = self.id;
        runner.run(
            move |context| async move { save_change(&context, id, change).await },
            |saved| ApplyMsg::Saved(saved.and_then(|saved| saved)),
        )
    }

    fn delivery_mut(&mut self) -> Option<&mut Delivery> {
        self.apply.as_mut().and_then(|apply| apply.as_mut().ok())
    }

    fn joblog_mut(&mut self) -> Option<&mut Joblog> {
        self.joblog.as_mut().and_then(|joblog| joblog.as_mut().ok())
    }

    fn joblog_value(&mut self, section: usize, field: usize) -> Option<&mut Value> {
        self.joblog_mut()?.values.get_mut(section)?.get_mut(field)
    }
}

/// What the candidate changed in the joblog: how far they are, or whether it is in their joblog.
#[derive(Debug, Clone, Copy)]
enum Change {
    Status(&'static str),
    Logged(bool),
}

/// Saves change; the other value is the one saved with the match now, not the one shown, which
/// may be older (the match may be open on another page too).
async fn save_change(
    context: &crate::context::Context,
    id: i64,
    change: Change,
) -> Result<(&'static str, Logged), String> {
    let saved = match_service::joblog_panel(context, id).await.map_err(|e| e.message)?;
    let (status, logged, synced) = match change {
        Change::Status(status) => (status, saved.logged.checked, false),
        Change::Logged(logged) => (saved_status(&saved), logged, logged),
    };
    let logged = match_service::save_joblog_status_synced(context, id, status, logged, synced, None)
        .await
        .map_err(|e| e.message)?;
    Ok((status, logged))
}

/// Marks saved match id as applied for, the path the extension's Mark as applied takes
/// (match_service::mark_applied), and reads its joblog entry back: the status and whether it is in
/// the joblog, for the Jobnet joblog sections shown.
pub async fn mark_applied(context: &crate::context::Context, id: i64) -> Result<(&'static str, Logged), String> {
    match_service::mark_applied(context, id).await.map_err(|e| e.message)?;
    let saved = match_service::joblog_panel(context, id).await.map_err(|e| e.message)?;
    Ok((saved_status(&saved), saved.logged))
}

/// The status a joblog entry has, as read.
fn saved_status(panel: &JoblogPanel) -> &'static str {
    panel
        .sections
        .iter()
        .flat_map(|section| &section.fields)
        .find(|field| field.choose)
        .map(chosen)
        .filter(|status| !status.is_empty())
        .unwrap_or(joblog::SEARCH_STATUSES[0].0)
}

/// The joblog entry, and the CVR register's address when the post names no postcode and town.
async fn read_joblog(context: &crate::context::Context, id: i64) -> Result<(JoblogPanel, Option<CvrAddress>), String> {
    let panel = match_service::joblog_panel(context, id).await.map_err(|e| e.message)?;
    let value = |name: &str| field_of(&panel, name).map(|field| field.value.trim().to_string());
    let address = match (value(POSTCODE), value(COMPANY)) {
        (Some(postcode), Some(company)) if postcode.is_empty() && !company.is_empty() => {
            cvr::registered_address(&company).await
        }
        _ => None,
    };
    Ok((panel, address))
}

fn field_of<'a>(panel: &'a JoblogPanel, name: &str) -> Option<&'a PanelField> {
    panel
        .sections
        .iter()
        .flat_map(|section| &section.fields)
        .find(|field| field.name == name)
}

impl Delivery {
    fn new(delivery: ApplicationDelivery, application: Option<String>) -> Self {
        let values = delivery
            .fields
            .iter()
            .map(|field| Value::new(&field.value, field.multiline))
            .collect();
        let mut shown = Delivery { delivery, values };
        shown.with_application(application);
        shown
    }

    /// The mailto link with To and Subject as edited.
    fn mailto(&self) -> String {
        let value = |id: &str| {
            self.delivery
                .fields
                .iter()
                .zip(&self.values)
                .find(|(field, _)| field.id == id)
                .map(|(_, value)| value.text().trim().to_string())
                .unwrap_or_default()
        };
        application_delivery::mailto(&value(TO), &value(SUBJECT))
    }

    /// The one-line values as edited, by field id.
    fn lines(&self) -> Vec<(String, String)> {
        self.delivery
            .fields
            .iter()
            .zip(&self.values)
            .filter(|(field, _)| !field.multiline)
            .map(|(field, value)| (field.id.clone(), value.text()))
            .collect()
    }

    /// Puts back one-line values kept by field id.
    fn with_lines(&mut self, kept: Vec<(String, String)>) {
        for (field, value) in self.delivery.fields.iter().zip(&mut self.values) {
            if let Some((_, line)) = kept.iter().find(|(id, _)| *id == field.id && !field.multiline) {
                *value = Value::Line(line.clone());
            }
        }
    }

    /// The body (the one long field) as the application is edited in its box.
    fn with_application(&mut self, application: Option<String>) {
        let Some(edited) = application.filter(|edited| !edited.trim().is_empty()) else {
            return;
        };
        for (field, value) in self.delivery.fields.iter().zip(&mut self.values) {
            if field.multiline {
                *value = Value::new(&edited, true);
            }
        }
    }
}

impl Joblog {
    fn new(mut panel: JoblogPanel, address: Option<&CvrAddress>) -> Self {
        if let Some(address) = address {
            with_address(&mut panel, address);
        }
        let values = panel
            .sections
            .iter()
            .map(|section| {
                section
                    .fields
                    .iter()
                    .map(|field| {
                        if field.choice {
                            Value::Line(chosen(field).to_string())
                        } else {
                            Value::new(&field.value, field.multiline)
                        }
                    })
                    .collect()
            })
            .collect();
        Joblog {
            panel,
            values,
            cvr_note: address.map(CvrAddress::note),
            prompt: None,
            saving: false,
        }
    }

    /// Saved: the status picked, whether it is in the joblog, and the day applied in the date
    /// field.
    fn saved(&mut self, status: &'static str, logged: Logged) {
        for (section, values) in self.panel.sections.iter().zip(&mut self.values) {
            for (field, value) in section.fields.iter().zip(values) {
                if field.choose {
                    *value = Value::Line(status.to_string());
                } else if field.name == APPLIED_ON {
                    *value = Value::Line(logged.applied_on.clone());
                }
            }
        }
        self.panel.logged = logged;
    }

    fn ai_prompt(&self) -> String {
        let sections: Vec<(&str, Vec<(&str, String)>)> = self
            .panel
            .sections
            .iter()
            .zip(&self.values)
            .map(|(section, values)| {
                let fields = section
                    .fields
                    .iter()
                    .zip(values)
                    .map(|(field, value)| {
                        let said = if field.choice {
                            field
                                .options
                                .iter()
                                .find(|option| option.value == value.text())
                                .map(|option| option.prompt.clone())
                                .unwrap_or_default()
                        } else {
                            value.text()
                        };
                        (field.label, said)
                    })
                    .collect();
                (section.heading, fields)
            })
            .collect();
        joblog_panel::ai_prompt(&sections)
    }
}

/// The register's address in the empty fields, and "No" for an unknown postcode.
fn with_address(panel: &mut JoblogPanel, address: &CvrAddress) {
    for field in panel.sections.iter_mut().flat_map(|section| &mut section.fields) {
        match field.name {
            POSTCODE => field.value = address.postal_code_and_city.clone(),
            ADDRESS if field.value.is_empty() && !address.address.is_empty() => field.value = address.address.clone(),
            COUNTRY if field.value.is_empty() => field.value = COUNTRY_DENMARK.to_string(),
            UNKNOWN_POSTCODE => {
                for option in &mut field.options {
                    option.chosen = option.value == UNKNOWN_POSTCODE_NO;
                }
            }
            _ => {}
        }
    }
}

/// Cuts area's text to its first most characters.
fn cut_to(area: &mut text_editor::Content, most: usize) {
    let text = area.text();
    if text.chars().count() > most {
        *area = text_editor::Content::with_text(&text.chars().take(most).collect::<String>());
    }
}

/// The option chosen of a choice field.
fn chosen(field: &PanelField) -> &'static str {
    field
        .options
        .iter()
        .find(|option| option.chosen)
        .map_or("", |option| option.value)
}

/// The Apply and Jobnet joblog sections, each opened by its heading's button.
pub fn view(applying: &Applying, busy: bool) -> Element<'_, ApplyMsg> {
    let apply_heading = match &applying.apply {
        Some(Ok(delivery)) => delivery.delivery.heading,
        _ => "Apply",
    };
    let mut view = Column::new().spacing(GAP).push(opener(
        apply_heading,
        "How the post asks for the application, with everything to copy and attach.",
        applying.apply_open,
        ApplyMsg::ToggleApply,
    ));
    if applying.apply_open {
        view = view.push(match &applying.apply {
            None => widgets::caption("Reading…"),
            Some(Err(why)) => widgets::message_bar(Health::Attention, why.as_str()),
            Some(Ok(delivery)) => delivery_view(delivery, busy),
        });
    }
    view = view.push(opener(
        "Jobnet joblog",
        "The entry for your joblog on Jobnet, in Jobnet's order, to copy over.",
        applying.joblog_open,
        ApplyMsg::ToggleJoblog,
    ));
    if applying.joblog_open {
        view = view.push(match &applying.joblog {
            None => widgets::caption("Reading…"),
            Some(Err(why)) => widgets::message_bar(Health::Attention, why.as_str()),
            Some(Ok(joblog)) => joblog_view(joblog, busy),
        });
    }
    view.into()
}

fn opener<'a>(title: &'a str, under: &'a str, open: bool, toggle: ApplyMsg) -> Element<'a, ApplyMsg> {
    widgets::card(
        container(
            row![
                column![widgets::section_title(title), widgets::caption(under)]
                    .spacing(4)
                    .width(Fill),
                button(text(if open { "Hide" } else { "Show" }).size(style::BODY_SIZE))
                    .style(style::standard_button)
                    .on_press(toggle),
            ]
            .spacing(14)
            .align_y(Center),
        )
        .padding(CARD_PADDING)
        .into(),
    )
}

fn delivery_view(delivery: &Delivery, busy: bool) -> Element<'_, ApplyMsg> {
    let d = &delivery.delivery;
    let mut body = Column::new().spacing(GAP);
    if !d.intro.is_empty() {
        body = body.push(widgets::caption(d.intro.as_str()));
    }
    for (n, (field, value)) in d.fields.iter().zip(&delivery.values).enumerate() {
        let input: Element<'_, ApplyMsg> = match value {
            Value::Line(line) => text_input("", line)
                .on_input(move |line| ApplyMsg::DeliveryLine(n, line))
                .size(style::BODY_SIZE)
                .padding(8)
                .style(style::text_field)
                .into(),
            Value::Area(area) => text_editor(area)
                .on_action(move |action| ApplyMsg::DeliveryArea(n, action))
                .size(style::BODY_SIZE)
                .padding(8)
                .height(AREA_HEIGHT)
                .style(style::text_area)
                .into(),
        };
        body = body.push(copyable(
            field.label.as_str(),
            input,
            ApplyMsg::CopyDelivery(n),
            (!field.hint.is_empty()).then_some(field.hint.as_str()),
        ));
    }
    body = body.push(widgets::section_title(d.links_heading));
    for (n, link) in d.links.iter().enumerate() {
        body = body.push(copyable(
            link.label.as_str(),
            text(link.value.as_str()).size(style::BODY_SIZE).into(),
            ApplyMsg::CopyLink(n),
            None,
        ));
    }
    if !d.links_hint.is_empty() {
        body = body.push(widgets::caption(d.links_hint));
    }
    if !d.asks.is_empty() {
        body = body.push(text(d.asks.as_str()).size(style::BODY_SIZE));
    }
    body = body.push(widgets::section_title(d.files_heading));
    let files = d.files.iter().map(|file| {
        pdf_button(
            format!("{} (PDF)", file.label),
            file.kind,
            file.ready,
            file.missing,
            busy,
        )
    });
    body = body.push(row(files).spacing(8));
    for missing in &d.missing {
        body = body.push(widgets::caption(missing.as_str()));
    }
    if let Some(open) = &d.open {
        body = body.push(
            button(text(open.label).size(style::BODY_SIZE))
                .style(style::primary_button)
                .on_press(if d.by == EMAIL {
                    ApplyMsg::OpenEmail
                } else {
                    ApplyMsg::OpenLink(open.href.clone())
                }),
        );
    }
    if d.by == EMAIL {
        body = body.push(widgets::caption(
            "An email link can't carry files: attach the PDFs in your email app.",
        ));
    }
    widgets::card(container(body).padding(CARD_PADDING).into())
}

fn joblog_view(joblog: &Joblog, busy: bool) -> Element<'_, ApplyMsg> {
    let mut body = Column::new().spacing(GAP);
    for (s, (section, values)) in joblog.panel.sections.iter().zip(&joblog.values).enumerate() {
        body = body.push(widgets::section_title(section.heading));
        for (f, (field, value)) in section.fields.iter().zip(values).enumerate() {
            body = body.push(joblog_field(joblog, s, f, field, value));
        }
        body = body.push(widgets::divider());
    }
    body = body.push(widgets::section_title("Gem ansøgning, CV og jobannonce"));
    let files = joblog.panel.files.iter().map(|file| {
        let why = if file.ready { "" } else { file.title };
        pdf_button(file.label.clone(), file.kind, file.ready, why, busy)
    });
    body = body.push(row(files).spacing(8));
    for missing in &joblog.panel.missing {
        body = body.push(widgets::caption(missing.as_str()));
    }
    body = body
        .push(widgets::divider())
        .push(widgets::section_title("Fill it in with an AI"))
        .push(widgets::caption(
            "Copies these values as instructions for an AI that can use your browser, such as Claude in Chrome. It fills the form and stops before Gem; you add the PDFs.",
        ))
        .push(
            button(text("Copy as AI prompt").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press(ApplyMsg::CopyPrompt),
        );
    if let Some(prompt) = &joblog.prompt {
        body = body.push(
            text_editor(prompt)
                .on_action(ApplyMsg::Prompt)
                .size(style::CAPTION_SIZE)
                .padding(8)
                .height(PROMPT_HEIGHT)
                .style(style::text_area),
        );
    }
    let logged = &joblog.panel.logged;
    let mut saved = column![
        widgets::divider(),
        widgets::section_title("In your Jobnet joblog"),
        checkbox(logged.checked)
            .label(joblog_panel::LOGGED_LABEL)
            .on_toggle_maybe((!joblog.saving).then_some(ApplyMsg::Logged))
            .size(16)
            .text_size(style::BODY_SIZE)
            .style(style::check_box),
    ]
    .spacing(GAP);
    if !logged.at.is_empty() {
        saved = saved.push(widgets::caption(format!(
            "{} {}.",
            joblog_panel::LOGGED_ON,
            super::documents::day_and_time(&logged.at)
        )));
    }
    body = body.push(saved);
    widgets::card(container(body).padding(CARD_PADDING).into())
}

fn joblog_field<'a>(
    joblog: &'a Joblog,
    s: usize,
    f: usize,
    field: &'a PanelField,
    value: &'a Value,
) -> Element<'a, ApplyMsg> {
    let label = if field.required {
        format!("{} *", field.label)
    } else {
        field.label.to_string()
    };
    let filled_from_register = joblog
        .cvr_note
        .as_deref()
        .filter(|_| matches!(field.name, POSTCODE | ADDRESS) && !value.text().is_empty());
    let hint = filled_from_register.or((!field.hint.is_empty()).then_some(field.hint.as_str()));
    let mut shown = column![text(label).font(style::semibold()).size(style::CAPTION_SIZE)].spacing(6);
    if field.choice {
        let picked = value.text();
        let options = field.options.iter().map(|option| {
            let is_chosen = option.value == picked;
            let chip = button(text(option.text).size(style::BODY_SIZE)).style(style::subtle_button(is_chosen));
            // Only how far the candidate is is picked here; the rest are Jobnet's to pick.
            if field.choose {
                chip.on_press_maybe((!joblog.saving && !is_chosen).then_some(ApplyMsg::Status(option.value)))
                    .into()
            } else {
                chip.into()
            }
        });
        shown = shown.push(row(options).spacing(6));
    } else {
        let input: Element<'a, ApplyMsg> = match value {
            Value::Line(line) => text_input("", line)
                .on_input(move |line| ApplyMsg::JoblogLine(s, f, line))
                .size(style::BODY_SIZE)
                .padding(8)
                .style(style::text_field)
                .into(),
            Value::Area(area) => text_editor(area)
                .on_action(move |action| ApplyMsg::JoblogArea(s, f, action))
                .size(style::BODY_SIZE)
                .padding(8)
                .height(AREA_HEIGHT)
                .style(style::text_area)
                .into(),
        };
        shown = shown.push(
            row![
                container(input).width(Fill),
                button(text("Copy").size(style::BODY_SIZE))
                    .style(style::standard_button)
                    .on_press(ApplyMsg::CopyJoblog(s, f)),
            ]
            .spacing(8)
            .align_y(Center),
        );
        if field.left.is_some() {
            let left = joblog::MAX_NOTES.saturating_sub(value.text().chars().count());
            shown = shown.push(widgets::caption(format!("{CHARACTERS_LEFT} {left}")));
        }
    }
    if let Some(hint) = hint {
        shown = shown.push(widgets::caption(hint.to_string()));
    }
    shown.into()
}

/// A labelled value with a Copy button beside it.
fn copyable<'a>(
    label: &'a str,
    input: Element<'a, ApplyMsg>,
    copy: ApplyMsg,
    hint: Option<&'a str>,
) -> Element<'a, ApplyMsg> {
    let input = row![
        container(input).width(Fill),
        button(text("Copy").size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press(copy),
    ]
    .spacing(8)
    .align_y(Center);
    widgets::field(label, input.into(), hint)
}

/// A PDF's button: saves it, or says why it can't be had yet.
fn pdf_button<'a>(
    label: String,
    kind: &'static str,
    ready: bool,
    missing: &'static str,
    busy: bool,
) -> Element<'a, ApplyMsg> {
    let press = if ready {
        (!busy).then_some(ApplyMsg::SavePdf(kind))
    } else {
        Some(ApplyMsg::Unavailable(missing))
    };
    button(text(label).size(style::BODY_SIZE))
        .style(style::standard_button)
        .on_press_maybe(press)
        .into()
}

/// Whether a kind is one the application and résumé boxes hold, so its PDF is their edited text.
pub fn is_written(kind: &str) -> bool {
    kind == APPLICATION || kind == crate::matches::document_box::RESUME
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matches::joblog_panel::{PanelOption, PanelSection};

    fn text_field(name: &'static str, value: &str) -> PanelField {
        PanelField {
            name,
            label: name,
            required: false,
            hint: String::new(),
            choice: false,
            choose: false,
            options: Vec::new(),
            value: value.to_string(),
            multiline: false,
            numeric: false,
            left: None,
        }
    }

    fn panel() -> JoblogPanel {
        let options = joblog::SEARCH_STATUSES
            .iter()
            .enumerate()
            .map(|(n, &(value, text))| PanelOption {
                value,
                text,
                chosen: n == 0,
                prompt: format!("choose \"{text}\""),
            })
            .collect();
        JoblogPanel {
            id: 21,
            sections: vec![PanelSection {
                heading: "Job",
                fields: vec![
                    text_field(COMPANY, "Nordisk Fragt A/S"),
                    text_field(POSTCODE, ""),
                    text_field(ADDRESS, ""),
                    text_field(APPLIED_ON, ""),
                    PanelField {
                        choice: true,
                        choose: true,
                        options,
                        ..text_field(joblog_panel::CHOOSE, "")
                    },
                ],
            }],
            files: Vec::new(),
            missing: Vec::new(),
            logged: Logged::from(None, String::new()),
        }
    }

    #[test]
    fn the_registers_address_fills_the_empty_fields() {
        let address = CvrAddress {
            name: "NORDISK FRAGT ApS".into(),
            address: "Havnevej 1".into(),
            postal_code_and_city: "8000 Aarhus C".into(),
        };
        let joblog = Joblog::new(panel(), Some(&address));
        assert_eq!(joblog.values[0][1].text(), "8000 Aarhus C");
        assert_eq!(joblog.values[0][2].text(), "Havnevej 1");
        assert!(joblog.cvr_note.as_deref().unwrap().contains("NORDISK FRAGT ApS"));
    }

    #[test]
    fn saving_the_status_picks_it_and_dates_the_application() {
        let mut joblog = Joblog::new(panel(), None);
        assert_eq!(joblog.values[0][4].text(), "IkkeSoegt");
        joblog.saved("Soegt", Logged::from(Some(1_760_000_000), "09-10-2025".to_string()));
        assert_eq!(joblog.values[0][4].text(), "Soegt");
        assert_eq!(joblog.values[0][3].text(), "09-10-2025");
        assert!(joblog.panel.logged.checked);
    }

    #[test]
    fn the_prompt_has_the_edited_values_and_the_chosen_status() {
        let mut joblog = Joblog::new(panel(), None);
        joblog.values[0][0] = Value::Line("Firma".into());
        let prompt = joblog.ai_prompt();
        assert!(prompt.contains("- name: Firma"));
        assert!(prompt.contains("- postalCodeAndCity: leave empty"));
        assert!(prompt.contains("choose \"Ikke søgt\""));
    }

    #[test]
    fn the_body_follows_the_application_as_edited() {
        use crate::matches::application_delivery::{FORM, Field};
        let field = |id: &str, multiline| Field {
            id: id.to_string(),
            label: id.to_string(),
            value: "Saved".to_string(),
            hint: String::new(),
            multiline,
        };
        let delivery = ApplicationDelivery {
            id: 21,
            by: FORM,
            heading: "Apply on the employer's site",
            intro: String::new(),
            fields: vec![field("delivery-subject", false), field("delivery-body", true)],
            asks: String::new(),
            files_heading: "Files to upload",
            files: Vec::new(),
            missing: Vec::new(),
            open: None,
            links_heading: "Your profile links",
            links: Vec::new(),
            links_hint: "",
        };
        let mut shown = Delivery::new(delivery, Some("Edited".into()));
        assert_eq!(shown.values[0].text(), "Saved");
        assert_eq!(shown.values[1].text(), "Edited");
        shown.with_application(Some("Edited again".into()));
        assert_eq!(shown.values[1].text(), "Edited again");
        shown.with_application(None);
        assert_eq!(shown.values[1].text(), "Edited again");
        shown.values[0] = Value::Line("Ny overskrift".into());
        assert_eq!(shown.mailto(), "mailto:?subject=Ny%20overskrift");
        // Read again after writing: the subject as edited stays.
        let kept = shown.lines();
        let mut again = Delivery::new(shown.delivery.clone(), None);
        again.with_lines(kept);
        assert_eq!(again.values[0].text(), "Ny overskrift");
        assert_eq!(again.values[1].text(), "Saved");
    }

    #[test]
    fn the_notes_are_cut_to_jobnets_limit() {
        let mut area = text_editor::Content::with_text(&"x".repeat(joblog::MAX_NOTES + 5));
        cut_to(&mut area, joblog::MAX_NOTES);
        assert_eq!(area.text().chars().count(), joblog::MAX_NOTES);
    }

    #[test]
    fn the_saved_status_is_read_from_the_entry() {
        assert_eq!(saved_status(&panel()), "IkkeSoegt");
    }
}
