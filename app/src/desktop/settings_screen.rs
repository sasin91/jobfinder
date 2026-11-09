//! The Settings screen: the settings pages, each opening its screen, and where the app's data is.

use iced::widget::{Column, button, column, container, row, space, text};
use iced::{Center, Element, Fill};

use super::style;
use super::{GAP, Message, Screen};
use crate::context::Context;

/// The padding inside a card.
const CARD_PADDING: f32 = 16.0;

/// The settings pages: label, what it sets, and its screen.
const PAGES: [(&str, &str, Screen); 7] = [
    (
        "Language models",
        "The API keys, and what each model is used for.",
        Screen::LanguageModels,
    ),
    (
        "Models",
        "Which models run each task, why, and Evaluate again.",
        Screen::LocalModel,
    ),
    (
        "Taxonomy",
        "The terms it learned from your matches: approve, reject or edit them.",
        Screen::Taxonomy,
    ),
    (
        "Job searches",
        "What the job scan searches for, and the posts it found.",
        Screen::JobSearches,
    ),
    (
        "Setup wizard",
        "Revisit guided setup for models, your CV, job sites and scanning.",
        Screen::Setup,
    ),
    ("Jobnet extension", "How to load it in Edge.", Screen::JoblogExtension),
    (
        "Devices",
        "Your phone or tablet: pair it, and see what is paired.",
        Screen::Devices,
    ),
];

pub fn view(context: &Context) -> Element<'_, Message> {
    let mut pages = Column::new();
    for (index, (label, note, screen)) in PAGES.iter().enumerate() {
        if index > 0 {
            pages = pages.push(container(space()).width(Fill).height(1).style(style::divider));
        }
        pages = pages.push(page_row(label, note, *screen));
    }
    let database = if context.database.path.is_empty() {
        "In memory".to_string()
    } else {
        context.database.path.clone()
    };
    let data = column![
        text("Database").font(style::semibold()).size(style::BODY_SIZE),
        text(database).size(style::CAPTION_SIZE).style(style::secondary),
    ]
    .spacing(2)
    .padding([12.0, CARD_PADDING]);
    column![
        column![
            text("Settings").font(style::title_font()).size(style::TITLE_SIZE),
            text("What the app runs on, and what it searches for.")
                .size(style::BODY_SIZE)
                .style(style::secondary),
        ]
        .spacing(4),
        container(pages).width(Fill).style(style::card),
        text("Your data").font(style::semibold()).size(style::SECTION_SIZE),
        container(data).width(Fill).style(style::card),
    ]
    .spacing(GAP * 1.5)
    .into()
}

/// A settings page: its name and what it sets, opening its screen.
fn page_row(label: &'static str, note: &'static str, screen: Screen) -> Element<'static, Message> {
    button(
        row![
            column![
                text(label).font(style::semibold()).size(style::BODY_SIZE),
                text(note).size(style::CAPTION_SIZE).style(style::secondary),
            ]
            .spacing(2)
            .width(Fill),
            text("›").size(style::BODY_SIZE).style(style::secondary),
        ]
        .align_y(Center),
    )
    .padding([12.0, CARD_PADDING])
    .width(Fill)
    .style(style::row_button)
    .on_press(Message::Show(screen))
    .into()
}
