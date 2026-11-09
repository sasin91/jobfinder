//! The Language models screen: the API keys (account) in the window. The saved keys with
//! Check and Delete and an Add a key button that opens the form, and one list of what the AI
//! does: per use the key it runs on, and under it per task the model, each saved as it is picked,
//! all through account_service.

use std::sync::Arc;

use iced::widget::{Column, button, column, container, pick_list, row, text, text_input};
use iced::{Center, Element, Fill, Task};

use super::style;
use super::widgets::{self, CARD_PADDING, ROW_PADDING};
use super::{GAP, Runner};
use crate::account::account_model::{APPLICATION, JOB_EVALUATION, PROVIDERS, RESUME, USES, UseChoices};
use crate::account::account_service::{self, AccountView, KeyForm, SaveKey};
use crate::account::saved_key::{KeyChoice, KeyUse, SavedKey};
use crate::account::task_model_row::TaskModelRow;
use crate::dashboard::Health;
use crate::game_mode::stand_in::WhilePlaying;
use crate::llm::{LLAMA_CPP, LOCAL};

/// A field's width in the key form.
const FIELD_WIDTH: f32 = 420.0;
/// The width of a use's key list and a task's model list, wide enough for a model file's name on
/// one line.
const PICK_WIDTH: f32 = 340.0;
/// How far a task's row is indented under its use.
const TASK_INDENT: f32 = 20.0;
/// The order the uses are listed in: the AI match first, as the tasks are (task_model::TASKS).
const USE_ORDER: [&str; 3] = [JOB_EVALUATION, APPLICATION, RESUME];

/// An entry in a list: what the form sends, and what the screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    value: String,
    label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// The screen's state: the page's data as last read, and the forms as typed.
#[derive(Default)]
pub struct State {
    /// None until read; Err says why it couldn't be.
    view: Option<Result<Arc<AccountView>, String>>,
    provider: Option<Choice>,
    model: String,
    api_key: String,
    base_url: String,
    /// Why the key form couldn't be saved.
    errors: Vec<String>,
    /// Per use (account_model::USES, in order), the key picked for it.
    uses: Vec<Option<Choice>>,
    /// Per task row (AccountView::task_models, in order), the model picked or typed for it.
    task_models: Vec<String>,
    /// The Add a key form is open.
    adding_key: bool,
    /// The provider whose Delete was pressed once, waiting for the second press.
    confirm_delete: Option<String>,
    /// Work is running (a save, a check), so the buttons wait.
    busy: bool,
    /// What the last click came to.
    said: Option<(Health, String)>,
}

#[derive(Clone)]
pub enum Msg {
    /// Reads the page again from scratch, the forms with it.
    Load,
    /// Reads the page again after a save, keeping the screen and the key form as they are.
    Refresh,
    Loaded {
        view: Result<Arc<AccountView>, String>,
        /// Fill the key form from it too (Load), not only the picks (Refresh).
        whole: bool,
    },
    /// Opens or closes the Add a key form.
    ShowKeyForm(bool),
    Provider(Choice),
    Model(String),
    ApiKey(String),
    BaseUrl(String),
    Save,
    Saved(Result<Arc<SaveKey>, String>),
    /// The key picked for the use at the index (account_model::USES); saved at once.
    PickUse(usize, Choice),
    /// The model file picked for the task row at the index; saved at once.
    PickTaskModel(usize, String),
    /// The model name typed for the task row at the index.
    TaskModel(usize, String),
    /// Saves the model name typed for the task row at the index.
    SaveTaskModel(usize),
    AskDelete(String),
    CancelDelete,
    Delete(String),
    Check(String),
    /// What a click came to: Ok said, or Err why not; a reload follows when reload.
    Said {
        said: Result<String, String>,
        reload: bool,
    },
    /// Goes to the Local model screen.
    ShowLocalModel,
    /// What another screen's work came to, said here (a local model saved: pick its uses).
    Tell(String),
    /// For the shell: a key was saved while setup is unfinished, so it goes back to Setup,
    /// saying this.
    BackToSetup(String),
}

pub fn update(state: &mut State, msg: Msg, runner: &Runner) -> Task<Msg> {
    match msg {
        Msg::Load => {
            // The forms go while the read runs, so nothing typed now is overwritten by it.
            state.view = None;
            return read_view(runner, true);
        }
        Msg::Refresh => return read_view(runner, false),
        Msg::Loaded { view, whole } => {
            if let Ok(view) = &view {
                if whole {
                    fill_key_form(state, view);
                }
                fill_picks(state, view);
            }
            state.view = Some(view);
            state.busy = false;
        }
        Msg::ShowKeyForm(open) => {
            state.adding_key = open;
            state.errors.clear();
        }
        Msg::Provider(choice) => state.provider = Some(choice),
        Msg::Model(model) => state.model = model,
        Msg::ApiKey(key) => state.api_key = key,
        Msg::BaseUrl(url) => state.base_url = url,
        Msg::Save => {
            state.busy = true;
            let form = KeyForm {
                provider: state.provider.as_ref().map(|c| c.value.clone()).unwrap_or_default(),
                model: state.model.clone(),
                api_key: state.api_key.clone(),
                base_url: state.base_url.clone(),
            };
            return runner.run(
                move |context| async move {
                    let saved = account_service::save_key(&context, &form).await?;
                    if form.provider == LLAMA_CPP && matches!(saved, SaveKey::Saved { .. }) {
                        crate::account::model_plan::download_selected(context).await?;
                    }
                    Ok::<_, crate::service_error::ServiceError>(saved)
                },
                |answer| Msg::Saved(answer.and_then(|saved| saved.map(Arc::new).map_err(|e| e.message))),
            );
        }
        Msg::Saved(Ok(saved)) => match &*saved {
            SaveKey::Invalid { errors, .. } => {
                state.errors = errors.clone();
                state.busy = false;
            }
            SaveKey::Saved {
                message,
                setup_unfinished,
            } => {
                state.errors.clear();
                state.api_key.clear();
                state.adding_key = false;
                state.busy = false;
                // Back to the wizard while it is unfinished.
                if *setup_unfinished {
                    return Task::done(Msg::BackToSetup(message.clone()));
                }
                state.said = Some((Health::Ready, message.clone()));
                return Task::done(Msg::Load);
            }
        },
        Msg::Saved(Err(why))
        | Msg::Said {
            said: Err(why),
            reload: false,
        } => {
            state.said = Some((Health::Attention, why));
            state.busy = false;
        }
        Msg::PickUse(index, choice) => {
            let Some(slot) = state.uses.get_mut(index) else {
                return Task::none();
            };
            *slot = Some(choice);
            state.busy = true;
            let chosen = use_choices(&state.uses);
            return runner.run(
                move |context| async move {
                    account_service::save_uses(&context, &chosen).await?;
                    crate::account::model_plan::download_selected(context).await
                },
                said(true),
            );
        }
        Msg::PickTaskModel(index, model) => {
            if let Some(slot) = state.task_models.get_mut(index) {
                *slot = model;
            }
            return save_task_model(state, index, runner);
        }
        Msg::TaskModel(index, model) => {
            if let Some(slot) = state.task_models.get_mut(index) {
                *slot = model;
            }
        }
        Msg::SaveTaskModel(index) => return save_task_model(state, index, runner),
        Msg::AskDelete(provider) => state.confirm_delete = Some(provider),
        Msg::CancelDelete => state.confirm_delete = None,
        Msg::Delete(provider) => {
            state.confirm_delete = None;
            state.busy = true;
            return runner.run(
                move |context| async move { account_service::delete_key(&context, &provider).await },
                said(true),
            );
        }
        Msg::Check(provider) => {
            state.busy = true;
            state.said = Some((Health::Neutral, "Asking the model for a tiny answer…".to_string()));
            return runner.run(
                move |context| async move { account_service::check_key(&context, &provider).await },
                said(false),
            );
        }
        Msg::Said { said, reload } => {
            state.said = Some(match said {
                Ok(said) => (Health::Ready, said),
                Err(why) => (Health::Attention, why),
            });
            state.busy = false;
            if reload {
                return Task::done(Msg::Refresh);
            }
        }
        // The shell changes screens.
        Msg::ShowLocalModel | Msg::BackToSetup(_) => {}
        Msg::Tell(said) => {
            state.said = Some((Health::Ready, said));
            return Task::done(Msg::Load);
        }
    }
    Task::none()
}

/// Reads the page; whole fills the key form from it too.
fn read_view(runner: &Runner, whole: bool) -> Task<Msg> {
    runner.run(
        |context| async move { account_service::account_view(&context, None).await.map(Arc::new) },
        move |read| Msg::Loaded {
            view: read.and_then(|view| view.map_err(|e| e.message)),
            whole,
        },
    )
}

/// Saves the model picked or typed for the task row at index.
fn save_task_model(state: &mut State, index: usize, runner: &Runner) -> Task<Msg> {
    let Some(Ok(view)) = &state.view else {
        return Task::none();
    };
    let (Some(row), Some(model)) = (view.task_models.get(index), state.task_models.get(index)) else {
        return Task::none();
    };
    let chosen = vec![(row.task.to_string(), model.clone())];
    state.busy = true;
    runner.run(
        move |context| async move {
            account_service::save_task_models(&context, &chosen).await?;
            crate::account::model_plan::download_selected(context).await
        },
        said(true),
    )
}

/// A service's answer as Msg::Said.
fn said(
    reload: bool,
) -> impl Fn(Result<Result<String, crate::service_error::ServiceError>, String>) -> Msg + Send + 'static {
    move |answer| Msg::Said {
        said: answer.and_then(|said| said.map_err(|e| e.message)),
        reload,
    }
}

/// The key form as the page fills it: the provider to add with its saved model and address; the
/// API key field starts empty.
fn fill_key_form(state: &mut State, view: &AccountView) {
    // A key belongs to the provider it was typed for; the provider is picked again here.
    state.api_key.clear();
    state.provider = view
        .providers
        .iter()
        .find(|option| option.selected)
        .map(|option| Choice {
            value: option.value.to_string(),
            label: option.label.to_string(),
        });
    state.model = view.model.clone();
    state.base_url = view.base_url.clone();
}

/// Each use's picked key and each task's model, as saved.
fn fill_picks(state: &mut State, view: &AccountView) {
    state.uses = view.key_uses.iter().map(picked).collect();
    state.task_models = view.task_models.iter().map(task_model_of).collect();
}

/// The key a use's list has picked.
fn picked(key_use: &KeyUse) -> Option<Choice> {
    key_use.choices.iter().find(|choice| choice.selected).map(choice_of)
}

/// A page's list entry as the screen's.
fn choice_of(choice: &KeyChoice) -> Choice {
    Choice {
        value: choice.value.clone(),
        label: choice.label.clone(),
    }
}

/// The model a task row has: the file its list picked, or the name saved for it.
fn task_model_of(row: &TaskModelRow) -> String {
    if !row.picks_a_file {
        return row.model.clone();
    }
    row.choices
        .iter()
        .find(|choice| choice.selected)
        .map(|choice| choice.value.clone())
        .unwrap_or_default()
}

/// The picks as account_service takes them; "" (no key) and no pick run a use on none.
fn use_choices(uses: &[Option<Choice>]) -> UseChoices {
    let pick = |name: &str| {
        let index = USES.iter().position(|(use_, _)| *use_ == name)?;
        let choice = uses.get(index)?.as_ref()?;
        Some(choice.value.clone()).filter(|value| !value.is_empty())
    };
    UseChoices {
        resume: pick(USES[0].0),
        application: pick(USES[1].0),
        job_evaluation: pick(USES[2].0),
    }
}

pub fn view(state: &State) -> Element<'_, Msg> {
    let title = widgets::page_title(
        "Language models",
        widgets::note("The AI runs on your own OpenAI or Anthropic key, or on a model on this computer."),
    );
    let mut screen = Column::new().spacing(GAP * 1.5).push(title);
    if let Some((health, said)) = &state.said {
        screen = screen.push(widgets::message_bar(*health, said.as_str()));
    }
    let view = match &state.view {
        None => return screen.push(widgets::note("Reading your keys…")).into(),
        Some(Err(why)) => {
            return screen
                .push(widgets::message_bar(Health::Attention, why.as_str()))
                .into();
        }
        Some(Ok(view)) => view,
    };
    if let Some(why) = &view.vault_error {
        screen = screen.push(widgets::message_bar(
            Health::Attention,
            format!("Your saved key couldn't be read: {why}"),
        ));
    }
    if view.saved.is_empty() {
        screen = screen.push(advice(view));
    } else {
        let add = button(text("Add a key").size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press(Msg::ShowKeyForm(true));
        let mut keys: Vec<Element<'_, Msg>> = view.saved.iter().map(|key| saved_row(key, state)).collect();
        if !state.adding_key {
            keys.push(container(add).padding(ROW_PADDING).into());
        }
        screen = screen.push(widgets::section("Your keys", widgets::rows(keys.into_iter())));
    }
    if !view.key_uses.is_empty() {
        screen = screen.push(widgets::section("What the AI runs on", runs_on_card(view, state)));
    }
    if let Some(while_playing) = &view.while_playing {
        screen = screen.push(widgets::section("While you play", while_playing_card(while_playing)));
    }
    if view.saved.is_empty() || state.adding_key {
        screen = screen.push(widgets::section("Add a key", key_form(view, state)));
    }
    screen.into()
}

/// What happens to the AI's work while a game runs, with the keys saved.
fn while_playing_card(while_playing: &WhilePlaying) -> Element<'_, Msg> {
    let what = match &while_playing.cloud_key {
        Some(cloud_key) => format!(
            "While a game runs, the model on this computer lets go of the graphics card, and the AI's work (a job scan's posts too) goes to your {cloud_key} key, on its own model. Once the game ends, it runs here again."
        ),
        None => "While a game runs, a job scan waits until it ends. With an OpenAI or Claude key, the work would go to that key meanwhile.".to_string(),
    };
    widgets::card(container(widgets::caption(what)).padding(CARD_PADDING).into())
}

/// Which model to run the AI on, while no key is saved (model_advice).
fn advice(view: &AccountView) -> Element<'_, Msg> {
    let advice = &view.advice;
    let body: Element<'_, Msg> = match &advice.local {
        Some(local) => column![
            text("Suggested: this app's own model").font(style::semibold()).size(style::BODY_SIZE),
            widgets::caption(format!(
                "It runs on this computer, so it needs no API key and costs nothing per use, and your CV and the job posts stay here. This computer ({}) runs {} ({}): it {}.",
                advice.computer, local.label, local.size, local.fit
            )),
            button(text("Set up this app's own model").size(style::BODY_SIZE))
                .style(style::primary_button)
                .on_press(Msg::ShowLocalModel),
        ]
        .spacing(8)
        .into(),
        None => column![
            text("Suggested: an OpenAI API key").font(style::semibold()).size(style::BODY_SIZE),
            widgets::caption(format!(
                "An OpenAI key runs on {} by default; a Claude key works too. This computer has no room on a graphics card for a model good enough for the AI match.",
                advice.openai_model
            )),
        ]
        .spacing(8)
        .into(),
    };
    widgets::card(container(body).padding(CARD_PADDING).into())
}

/// A saved key: its provider, last characters, model and address, and Check and Delete.
fn saved_row<'a>(key: &'a SavedKey, state: &'a State) -> Element<'a, Msg> {
    let mut about = Vec::new();
    if !key.key_hint.is_empty() {
        about.push(format!("key ending in …{}", key.key_hint));
    }
    if !key.model.is_empty() {
        about.push(format!("model {}", key.model));
    }
    if !key.base_url.is_empty() {
        about.push(format!("at {}", key.base_url));
    }
    about.push(format!("saved {}", key.saved_on));
    let mut lines = column![
        text(key.provider.as_str())
            .font(style::semibold())
            .size(style::BODY_SIZE),
        widgets::caption(about.join(" · ")),
    ]
    .spacing(2)
    .width(Fill);
    if key.unreadable {
        lines = lines.push(
            text("This key isn't kept anymore, so the AI can't use it. Save it again below.")
                .size(style::CAPTION_SIZE)
                .style(style::health_text(Health::Attention)),
        );
    }
    let waiting = |msg: Msg| (!state.busy).then_some(msg);
    let provider = key.provider_value.clone();
    let buttons = if state.confirm_delete.as_deref() == Some(key.provider_value.as_str()) {
        row![
            button(text("Cancel").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press(Msg::CancelDelete),
            button(text(format!("Delete the {} key", key.provider)).size(style::BODY_SIZE))
                .style(style::danger_button)
                .on_press_maybe(waiting(Msg::Delete(provider))),
        ]
    } else {
        let mut buttons = row![];
        if !key.unreadable {
            buttons = buttons.push(
                button(text("Check").size(style::BODY_SIZE))
                    .style(style::standard_button)
                    .on_press_maybe(waiting(Msg::Check(provider.clone()))),
            );
        }
        buttons.push(
            button(text("Delete").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press_maybe(waiting(Msg::AskDelete(provider))),
        )
    };
    let mark = if key.unreadable {
        Health::Attention
    } else {
        Health::Ready
    };
    row![style::mark(mark), lines, buttons.spacing(8)]
        .spacing(14)
        .align_y(Center)
        .padding(ROW_PADDING)
        .into()
}

/// One list of what the AI does: per use (USE_ORDER) the key it runs on, and under it, per task of
/// that use, the model. Every pick is saved as it is made.
fn runs_on_card<'a>(view: &'a AccountView, state: &'a State) -> Element<'a, Msg> {
    let uses = USE_ORDER.iter().filter_map(|name| {
        let index = view.key_uses.iter().position(|key_use| key_use.use_ == *name)?;
        Some(use_block(view, state, index))
    });
    widgets::rows(uses)
}

/// A use's key list, then its tasks' models; a use on no key says what happens without one.
fn use_block<'a>(view: &'a AccountView, state: &'a State, index: usize) -> Element<'a, Msg> {
    let key_use = &view.key_uses[index];
    let choices: Vec<Choice> = key_use.choices.iter().map(choice_of).collect();
    let selected = state.uses.get(index).cloned().flatten();
    let key_list = pick_list(choices, selected, move |choice| Msg::PickUse(index, choice))
        .text_size(style::BODY_SIZE)
        .padding(8)
        .style(style::pick_field)
        .menu_style(style::pick_menu)
        .width(PICK_WIDTH);
    let mut block = column![
        row![
            text(key_use.label)
                .font(style::semibold())
                .size(style::BODY_SIZE)
                .width(Fill),
            key_list,
        ]
        .align_y(Center)
    ]
    .spacing(8)
    .padding(ROW_PADDING);
    let tasks: Vec<(usize, &TaskModelRow)> = view
        .task_models
        .iter()
        .enumerate()
        .filter(|(_, task)| task.use_ == key_use.use_)
        .collect();
    if tasks.is_empty() {
        block = block.push(widgets::caption(without_a_key(key_use.use_)));
    }
    let only_one = tasks.len() == 1;
    for (task_index, task) in tasks {
        let label = if only_one { "Model" } else { task.label };
        block = block.push(task_row(state, task_index, task, label));
    }
    block.into()
}

/// What a use does on no key.
fn without_a_key(use_: &str) -> &'static str {
    if use_ == JOB_EVALUATION {
        "Off: the match reads job posts with the free reader."
    } else {
        "Off: nothing is written."
    }
}

/// A task's model: a list of the models folder's files for this app's own model, a typed name
/// for every other key.
fn task_row<'a>(state: &'a State, index: usize, task: &'a TaskModelRow, label: &'a str) -> Element<'a, Msg> {
    let model = state.task_models.get(index).map(String::as_str).unwrap_or("");
    let name = text(label).size(style::BODY_SIZE).width(Fill);
    let field: Element<'a, Msg> = if task.picks_a_file {
        let choices: Vec<Choice> = task.choices.iter().map(choice_of).collect();
        let selected = choices.iter().find(|choice| choice.value == model).cloned();
        pick_list(choices, selected, move |choice| Msg::PickTaskModel(index, choice.value))
            .text_size(style::BODY_SIZE)
            .padding(8)
            .style(style::pick_field)
            .menu_style(style::pick_menu)
            .width(PICK_WIDTH)
            .into()
    } else {
        typed_model(state, index, task, model)
    };
    row![name, field]
        .align_y(Center)
        .padding(iced::Padding::ZERO.left(TASK_INDENT))
        .into()
}

/// A typed model name, saved with Enter or its Save button, which shows once it differs from the
/// saved one.
fn typed_model<'a>(state: &'a State, index: usize, task: &'a TaskModelRow, model: &'a str) -> Element<'a, Msg> {
    let input = text_input(&task.keys_own, model)
        .on_input(move |typed| Msg::TaskModel(index, typed))
        .on_submit(Msg::SaveTaskModel(index))
        .size(style::BODY_SIZE)
        .padding(8)
        .style(style::text_field)
        .width(PICK_WIDTH);
    if model == task.model {
        return input.into();
    }
    row![
        button(text("Save").size(style::BODY_SIZE))
            .style(style::primary_button)
            .on_press_maybe((!state.busy).then_some(Msg::SaveTaskModel(index))),
        input,
    ]
    .spacing(8)
    .align_y(Center)
    .into()
}

/// The form that adds a key: the provider, then the fields it needs.
fn key_form<'a>(view: &'a AccountView, state: &'a State) -> Element<'a, Msg> {
    let providers: Vec<Choice> = PROVIDERS
        .iter()
        .map(|(value, label)| Choice {
            value: value.to_string(),
            label: label.to_string(),
        })
        .collect();
    let provider = state
        .provider
        .as_ref()
        .map(|choice| choice.value.as_str())
        .unwrap_or("");
    let mut form = Column::new().spacing(14).padding(CARD_PADDING);
    if !state.errors.is_empty() {
        form = form.push(widgets::errors(&state.errors));
    }
    form = form.push(widgets::field(
        "Provider",
        pick_list(providers, state.provider.clone(), Msg::Provider)
            .text_size(style::BODY_SIZE)
            .padding(8)
            .style(style::pick_field)
            .menu_style(style::pick_menu)
            .width(FIELD_WIDTH)
            .into(),
        None,
    ));
    if provider != LLAMA_CPP {
        form = form.push(widgets::field(
            "API key",
            text_input("sk-…", &state.api_key)
                .on_input(Msg::ApiKey)
                .secure(true)
                .size(style::BODY_SIZE)
                .padding(8)
                .style(style::text_field)
                .width(FIELD_WIDTH)
                .into(),
            Some("Kept sealed in your database and never shown again."),
        ));
    }
    form = form.push(widgets::field(
        "Model (optional)",
        text_input(model_placeholder(view, provider), &state.model)
            .on_input(Msg::Model)
            .size(style::BODY_SIZE)
            .padding(8)
            .style(style::text_field)
            .width(FIELD_WIDTH)
            .into(),
        Some(if provider == LLAMA_CPP {
            "Blank picks the one that fits this computer best."
        } else {
            "A local model needs its name, like llama3.1:8b."
        }),
    ));
    if provider == LOCAL {
        form = form.push(widgets::field(
            "Address of the local model",
            text_input(view.base_url_placeholder, &state.base_url)
                .on_input(Msg::BaseUrl)
                .size(style::BODY_SIZE)
                .padding(8)
                .style(style::text_field)
                .width(FIELD_WIDTH)
                .into(),
            Some("Ollama: http://localhost:11434/v1, LM Studio: http://localhost:1234/v1."),
        ));
    }
    let mut buttons = row![
        button(text("Save key").size(style::BODY_SIZE))
            .padding([8.0, 16.0])
            .style(style::primary_button)
            .on_press_maybe((!state.busy).then_some(Msg::Save)),
    ]
    .spacing(8)
    .align_y(Center);
    if !view.saved.is_empty() {
        buttons = buttons.push(
            button(text("Cancel").size(style::BODY_SIZE))
                .padding([8.0, 16.0])
                .style(style::standard_button)
                .on_press(Msg::ShowKeyForm(false)),
        );
    }
    form.push(widgets::caption(
        "Saving replaces the provider's saved key, if it has one.",
    ))
    .push(buttons)
    .into()
}

/// The model field's grey hint: this app's own model takes a file name, the others a model name.
fn model_placeholder<'a>(view: &'a AccountView, provider: &str) -> &'a str {
    if provider == LLAMA_CPP {
        "Blank: the one that fits this computer best"
    } else {
        &view.model_placeholder
    }
}
