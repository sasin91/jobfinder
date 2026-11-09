//! setup: the first-run wizard, which the window opens (desktop::setup_screen) on a database
//! that hasn't been set up (setup_steps::is_first_run). Its steps, in order:
//!
//! 1. The database: made at start-up; the wizard says where it is.
//! 2. The AI model: this app's own llama.cpp model when the computer has room for a good one,
//!    else an OpenAI or Claude key (model_advice). The steps themselves are the Local model and
//!    Language models screens, which come back here while the setup isn't finished.
//! 3. The CV, read into the profile (profile_fill), which needs the model.
//! 4. Jobs and individual sources, using the shared search service.
//! 5. A bounded first scan, or an explicit skip.
//! 6. Manual scanning or an automatic schedule, then explicit completion.
//!
//! The steps' state and their work are in setup_steps.

pub mod progress;
pub mod setup_steps;

pub use self::setup_steps::{first_run_or_logged, is_first_run};

#[cfg(test)]
mod tests {
    use super::setup_steps;
    use crate::profile::profile_model::FilledFrom;

    #[test]
    fn the_cv_step_says_what_the_profile_was_read_from() {
        let filled = FilledFrom {
            name: "Mette Sørensen".to_string(),
            cv_name: "cv.pdf".to_string(),
            // 31 October 2026, 12:00 UTC.
            filled_at: Some(1_793_448_000),
        };
        assert_eq!(
            setup_steps::filled_line(&filled),
            "Mette Sørensen, read from cv.pdf on 31 October 2026"
        );
        let imported = FilledFrom {
            cv_name: String::new(),
            filled_at: None,
            ..filled
        };
        assert_eq!(setup_steps::filled_line(&imported), "Mette Sørensen");
    }
}
