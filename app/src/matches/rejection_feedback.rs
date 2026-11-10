//! Local ranking preferences inferred from explicit rejections.

use sqlx::SqlitePool;

use super::Job;
use super::match_model::SCORE_DECIMALS;
use super::match_score::round_to;
use super::matcher::{Score, match_tag};

/// Compare whole titles and company names, retaining punctuation that can distinguish roles.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Apply the strongest matching signal once, regardless of how many posts were rejected.
pub(super) async fn adjust(db: &SqlitePool, job: &Job, score: &mut Score) {
    let titles = [&job.job_title, &job.job_title_en, &job.heading].map(|title| normalized(title));
    let company = normalized(&job.company);
    if score.index == 0.0 || (company.is_empty() && titles.iter().all(String::is_empty)) {
        return;
    }

    let signals = match sqlx::query_file!("queries/match_model/rejection_signals.sql")
        .fetch_all(db)
        .await
    {
        Ok(signals) => signals,
        Err(error) => {
            crate::log_line!("match (rejection feedback): {error}");
            return;
        }
    };
    let mut same_company = false;
    for signal in signals {
        let value = normalized(&signal.value);
        if value.is_empty() {
            continue;
        }
        if signal.reason == "irrelevant" && titles.contains(&value) {
            lower(
                score,
                0.25,
                "Lowered because you marked a match with this role title as irrelevant.",
            );
            return;
        }
        same_company |= signal.reason == "company" && company == value;
    }
    if same_company {
        lower(
            score,
            0.10,
            "Lowered because you previously rejected a match from this company.",
        );
    }
}

fn lower(score: &mut Score, penalty: f64, note: &str) {
    score.index = round_to((score.index - penalty).max(0.0), SCORE_DECIMALS);
    score.tag = match_tag(score.index).to_string();
    score.feedback_note = Some(note.to_string());
}
