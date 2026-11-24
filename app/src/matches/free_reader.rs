//! The free reading of a job post and a CV: no
//! language model, so it costs nothing per request.
//!
//! It spots the taxonomy's skills, roles and titles where the text names them by one of their
//! labels ("PHP", "Kørekort B", "projektledelse"), and reads years of experience with plain
//! rules. The answers have the shape of the model's extract_job and extract_cv answers, so
//! Cv_matcher scores them the same way. What it can't do: anything the text says in other words
//! ("you can lead a small team"), levels ("flydende dansk"), alternatives ("pædagog eller
//! pædagogisk assistent") and responsibilities.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use indexmap::{IndexMap, IndexSet};
use regex::Regex;

use super::{Job, KnownTerm, Profile, Requirement};
use crate::taxonomy::{self, Method, Taxonomy, Term};

/// Longest label, in words, worth looking for.
const MAX_WORDS: usize = 5;

/// Shortest label, in characters, worth looking for.
const MIN_LABEL_CHARS: usize = 2;

/// Longest heading, in characters: a longer line is text, not a heading.
const MAX_HEADING_CHARS: usize = 60;

/// How much of a post's first line is kept as its heading, in characters.
const HEADING_CHARS: usize = 160;

/// Years a post means by "et par års" (a couple of years) and by "flere års" (several years).
const COUPLE_OF_YEARS: i64 = 2;
const SEVERAL_YEARS: i64 = 3;

/// Months in a year, for counting work history in months.
const MONTHS_PER_YEAR: i64 = 12;

/// Labels that are everyday words in a job post or a CV, so finding them says nothing about a
/// skill ("Team", "Kunder", "Ansvar").
const TOO_COMMON: &str = "ansvar arbejde kunder kunde team teams opgaver ledelse udvikling service salg drift \
    kommunikation samarbejde planlægning struktur overblik support it web app apps \
    work design data administration management development projects project \
    uddannelse erfaring ansvarlig fleksibel selvstændig engageret";

/// Taxonomy skills Free_reader doesn't look for: labels that are everyday words in job posts
/// ("Løn", "Fokus", "Send", "Dygtig"). Term ids from
/// db/taxonomy.sql.
const IGNORE: [i64; 208] = [
    90, 176, 206, 255, 276, 415, 418, 420, 464, 488, 496, 536, 561, 658, 677, 703, 762, 870, 912, 948, 1022, 1047,
    1081, 1095, 1105, 1211, 1541, 1633, 1797, 1898, 1989, 2010, 2018, 2024, 2077, 2170, 2242, 2308, 2479, 2509, 2515,
    2518, 2522, 2527, 2536, 2574, 2595, 2663, 2690, 2693, 2725, 2766, 2768, 2812, 2915, 2960, 2961, 3061, 3239, 3326,
    3418, 3451, 3499, 3513, 3557, 3560, 3570, 3588, 3603, 3639, 3640, 3778, 3832, 3854, 3857, 3901, 3976, 4030, 4049,
    4208, 4216, 4372, 4484, 4497, 4551, 4630, 4685, 4795, 4822, 4854, 4865, 4962, 5013, 5059, 5073, 5178, 5186, 5204,
    5226, 5244, 5294, 5400, 5421, 5525, 5620, 5722, 5844, 5924, 5961, 5975, 6062, 6152, 6155, 6162, 6166, 6168, 6173,
    6185, 6203, 6250, 6319, 6338, 6438, 6480, 6506, 6517, 6518, 6578, 6582, 6618, 6722, 6736, 6738, 6813, 6846, 7008,
    7038, 7162, 7224, 7254, 7277, 7368, 7374, 7415, 7434, 7436, 7458, 7591, 7592, 7703, 7735, 7738, 7857, 7869, 7870,
    7920, 7944, 8039, 8153, 8188, 8211, 8216, 8227, 8239, 8275, 8323, 8516, 8549, 8562, 8572, 8618, 8701, 8804, 8886,
    8908, 8916, 8930, 8940, 8979, 8991, 9031, 9102, 9103, 9138, 9161, 9168, 9208, 9241, 9251, 9287, 9289, 9331, 9367,
    9371, 9394, 9398, 9414, 9454, 9506, 9558, 9591, 9592, 9629, 9655, 9684, 9696, 9857, 9879,
];

/// Taxonomy skills filed as soft skills (shown, not scored): the
/// terms the labelled ads list as a soft skill more often than as a hard one.
const SOFT: [i64; 95] = [
    17, 349, 353, 374, 403, 404, 409, 1782, 1783, 1983, 2120, 2756, 2816, 2832, 3085, 3189, 3379, 3380, 3383, 3462,
    3562, 3565, 3615, 3619, 3743, 3861, 3862, 4470, 4522, 4634, 4771, 4824, 4939, 5139, 5142, 5166, 5180, 5231, 5672,
    5727, 5734, 5824, 5990, 6045, 6066, 6233, 6236, 6237, 6314, 6593, 6622, 6653, 6692, 6696, 6778, 6794, 7079, 7144,
    7178, 7179, 7391, 7397, 7399, 7488, 7552, 7554, 7558, 7580, 7707, 7838, 7922, 8042, 8073, 8100, 8300, 8467, 8475,
    8694, 8743, 8770, 8810, 8822, 8852, 8995, 9035, 9170, 9189, 9214, 9215, 9221, 9360, 9536, 9712, 9902, 9939,
];

/// Skills under the taxonomy's "Soft Skills" category are shown, not scored.
const SOFT_SKILLS_CATEGORY: i64 = 13;

/// A line that says what follows is a nice-to-have.
static NICE_TO_HAVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(en fordel|fordel|gerne|ønskeligt|ønskværdigt|plus|nice to have|bonus|preferably|preferred|advantage|ideally)\b").unwrap()
});

/// Headings around the part of a job post that lists what is asked for, and of a CV that lists
/// education (whose dates are not work experience).
static ASKS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(din profil|om dig|vi forventer|vi søger en|vi forestiller os|kvalifikationer|krav|du har|du er|du kan|du bringer|hvem er du|requirements|qualifications|what you bring|who you are|you have|your profile|about you|what we expect|must have|nice to have)\b").unwrap()
});
static OFFERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(vi tilbyder|vi kan tilbyde|hvad vi tilbyder|om os|om virksomheden|om jobbet|ansøgning|løn og|we offer|what we offer|about us|benefits|how to apply|apply)\b").unwrap()
});
static EDUCATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(uddannelse|uddannelser|education|kurser|courses|certificeringer|certifications)\b").unwrap()
});
static WORK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(erfaring|erhvervserfaring|arbejdserfaring|experience|work experience|employment|ansættelser|karriere)\b").unwrap()
});

/// One-word traits ("Ambitiøs", "Struktureret", "Mødestabil", "Proactive"): Danish and English
/// adjective endings.
static TRAIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\p{L}+(ig|lig|isk|øs|iv|som|bar|ende|eret|et|ent|ant|stabil|minded|ive|ous|ful|ed|ic)$").unwrap()
});

static MENTIONS_EXPERIENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"erfaring|experience").unwrap());
static YEARS_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{1,2})\s*\+?\s*(?:-\s*\d{1,2}\s*)?(?:års|år|years?|yrs)").unwrap());
static YEARS_COUPLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"et par års|a couple of years|a few years").unwrap());
static YEARS_SEVERAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"flere års|mange års|several years|many years").unwrap());

/// A date range in a work history: an optional month name or number, a year, a dash or "til",
/// then another (month and) year or "now". Groups: 1-3 from, 4-6 to, 7 now.
static DATE_RANGE: LazyLock<Regex> = LazyLock::new(|| {
    let month = r"(?:(\p{L}{3,})\.?\s+|(\d{1,2})[/.-])?";
    Regex::new(&format!(
        r"(?i){month}((?:19|20)\d{{2}})\s*(?:-|–|—|til|to)\s*(?:{month}((?:19|20)\d{{2}})|(nu|now|present|i dag|dags dato|current|today))"
    ))
    .unwrap()
});

/// Any line break (\r\n, \n, \r and the Unicode ones).
static LINE_BREAK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\r\n|[\n\x0B\x0C\r\x{85}\x{2028}\x{2029}]").unwrap());
/// Bullets and list markers at the start of a line.
static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\s•·\-*–—▪●○◦>]+").unwrap());

/// Month abbreviations, Danish and English, as numbers 1-12.
const MONTHS: [(&str, i64); 14] = [
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("maj", 5),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("okt", 10),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

/// Characters a heading's leftover is trimmed of before deciding it was only a heading.
const HEADING_PUNCTUATION: &str = " \t:.!-";

/// A year and month (1-12), the "now" a work history running until today is counted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YearMonth {
    pub year: i64,
    pub month: i64,
}

impl YearMonth {
    /// The month a Unix time (seconds) falls in, in local time.
    pub fn of(timestamp: i64) -> Self {
        use chrono::Datelike;
        let day = chrono::DateTime::from_timestamp(timestamp, 0)
            .unwrap_or_default()
            .with_timezone(&chrono::Local)
            .date_naive();
        YearMonth {
            year: i64::from(day.year()),
            month: i64::from(day.month()),
        }
    }
}

pub struct FreeReader<'a> {
    taxonomy: &'a Taxonomy,
    /// Label words joined by spaces => term position in the taxonomy.
    labels: HashMap<String, usize>,
    soft: HashSet<i64>,
    now: YearMonth,
    /// Every phrase job() and cv() have put in their answers, with its term, for Cv_matcher's
    /// `known`.
    pub known: IndexMap<String, KnownTerm>,
}

impl<'a> FreeReader<'a> {
    pub fn new(taxonomy: &'a Taxonomy, now: YearMonth) -> Self {
        let common: HashSet<&str> = TOO_COMMON.split_whitespace().collect();
        let ignore: HashSet<i64> = IGNORE.into_iter().collect();
        let mut labels = HashMap::new();
        for (index, term) in taxonomy.terms.iter().enumerate() {
            if term.kind == "skill_category" || ignore.contains(&term.id) {
                continue;
            }
            for phrase in &term.phrases {
                let key = phrase.words.join(" ");
                let chars = key.chars().count();
                if phrase.words.len() > MAX_WORDS || common.contains(key.as_str()) || chars < MIN_LABEL_CHARS {
                    continue;
                }
                // A bare number is never a skill on its own.
                if phrase.words.len() == 1 && is_number(&key) {
                    continue;
                }
                // A skill and a title can share a label; the first (skills come first) wins.
                labels.entry(key).or_insert(index);
            }
        }
        Self {
            taxonomy,
            labels,
            soft: SOFT.into_iter().collect(),
            now,
            known: IndexMap::new(),
        }
    }

    /// The job post in extract_job's shape: the title and the requirements the taxonomy
    /// recognises, with years of experience by rule.
    pub fn job(&mut self, text: &str) -> Job {
        let lines = lines(text);
        let title = lines.first().cloned().unwrap_or_default();
        let asks = section(&lines, &ASKS, &OFFERS);

        let mut requirements = Vec::new();
        let mut seen = HashSet::new();
        for line in if asks.is_empty() { &lines } else { &asks } {
            let required = !NICE_TO_HAVE.is_match(line);
            let years = years_asked(line);
            if years > 0 {
                requirements.push(requirement(
                    format!("{years} års erfaring"),
                    format!("{years} years of experience"),
                    "experience",
                    required,
                    years,
                ));
            }
            for index in self.spot(line) {
                let term = &self.taxonomy.terms[index];
                if term.kind != "skill" || !seen.insert(term.id) {
                    continue;
                }
                let value = self.name(index);
                requirements.push(requirement(
                    value,
                    term.label("en").to_string(),
                    self.kind(term),
                    required,
                    0,
                ));
            }
        }

        let title_term = self.spot(&title).into_iter().find(|&index| {
            let kind = &self.taxonomy.terms[index].kind;
            kind == "title" || kind == "role"
        });
        let (job_title, job_title_en) = match title_term {
            Some(index) => (self.name(index), self.taxonomy.terms[index].label("en").to_string()),
            None => (String::new(), String::new()),
        };
        Job {
            text_en: text.to_string(),
            job_title,
            job_title_en,
            heading: title.chars().take(HEADING_CHARS).collect::<String>(),
            company: String::new(),
            requirements,
            responsibilities: Vec::new(),
            // Read from the title by seniority::with_level, as for a model's read.
            level: String::new(),
            // The free reader can't tell which language or functional field a post is in.
            working_language: String::new(),
            field: String::new(),
        }
    }

    /// The CV in extract_cv's shape: the skills, roles and titles it names, and the years between
    /// the dates of its work history, or the years it states on the profile's years line (the
    /// number the candidate confirmed), which win.
    pub fn cv(&mut self, text: &str) -> Profile {
        let lines = lines(text);
        let mut skills: IndexSet<String> = IndexSet::new();
        let mut titles: IndexSet<String> = IndexSet::new();
        for line in &lines {
            for index in self.spot(line) {
                let label = self.name(index);
                if self.taxonomy.terms[index].kind == "skill" {
                    skills.insert(label);
                } else {
                    titles.insert(label);
                }
            }
        }
        let education = section(&lines, &EDUCATION, &WORK);
        let work: Vec<&str> = lines
            .iter()
            .filter(|l| !education.contains(l))
            .map(String::as_str)
            .collect();
        Profile {
            text_en: text.to_string(),
            title: titles.into_iter().collect(),
            skill: skills.into_iter().collect(),
            experience_years: crate::profile::stated_years_of_work(text)
                .unwrap_or_else(|| years_worked(&work.join("\n"), self.now)),
            ..Profile::default()
        }
    }

    /// The terms a line names by a label, longest label first, each word used once ("Social- og
    /// sundhedsassistent" is one title, not also "assistent"). Term positions, each once.
    pub fn spot(&self, line: &str) -> Vec<usize> {
        let words = taxonomy::words(line);
        let mut found: IndexSet<usize> = IndexSet::new();
        let n = words.len();
        let mut i = 0;
        while i < n {
            let hit = (1..=MAX_WORDS.min(n - i))
                .rev()
                .find_map(|len| self.labels.get(&words[i..i + len].join(" ")).map(|&index| (index, len)));
            match hit {
                Some((index, len)) => {
                    // A term met again keeps its first place.
                    found.insert(index);
                    i += len;
                }
                None => i += 1,
            }
        }
        found.into_iter().collect()
    }

    fn kind(&self, term: &Term) -> String {
        if term.parent_id == Some(SOFT_SKILLS_CATEGORY) || self.soft.contains(&term.id) {
            return "soft_skill".into();
        }
        let label = term.label("da");
        if !label.contains(' ') && TRAIT.is_match(&label.to_lowercase()) {
            "soft_skill"
        } else {
            "skill"
        }
        .into()
    }

    /// The term's preferred label, remembered in `known` for Cv_matcher.
    fn name(&mut self, index: usize) -> String {
        let label = self.taxonomy.terms[index].label("da").to_string();
        self.known.entry(label.clone()).or_insert(KnownTerm {
            index,
            method: Method::Exact,
        });
        label
    }
}

/// Years of experience a line asks for: "3+ års erfaring", "flere års erfaring", "5 years of
/// experience".
pub fn years_asked(line: &str) -> i64 {
    let line = line.to_lowercase();
    if !MENTIONS_EXPERIENCE.is_match(&line) {
        return 0;
    }
    if let Some(m) = YEARS_NUMBER.captures(&line) {
        return m[1].parse().unwrap_or(0);
    }
    if YEARS_COUPLE.is_match(&line) {
        return COUPLE_OF_YEARS;
    }
    if YEARS_SEVERAL.is_match(&line) {
        SEVERAL_YEARS
    } else {
        0
    }
}

/// Whole years covered by the date ranges in a work history ("September 2024 – February 2026",
/// "2017 - nu", "03/2019 - 08/2021"), overlaps counted once.
pub fn years_worked(text: &str, now: YearMonth) -> i64 {
    let current = now.year * MONTHS_PER_YEAR + now.month - 1;
    let group = |m: &regex::Captures, i: usize| m.get(i).map_or("", |g| g.as_str()).to_string();
    let mut ranges: Vec<(i64, i64)> = Vec::new();
    for m in DATE_RANGE.captures_iter(text) {
        let from = group(&m, 3).parse::<i64>().unwrap_or(0) * MONTHS_PER_YEAR + month(&group(&m, 1), &group(&m, 2), 1);
        let to = match m.get(6) {
            Some(year) => {
                year.as_str().parse::<i64>().unwrap_or(0) * MONTHS_PER_YEAR
                    + month(&group(&m, 4), &group(&m, 5), MONTHS_PER_YEAR)
            }
            None => current,
        };
        if to >= from && to <= current + 1 {
            ranges.push((from, to));
        }
    }
    ranges.sort_by_key(|&(from, _)| from);
    let mut months = 0;
    let mut end = -1;
    for (from, to) in ranges {
        if to <= end {
            continue;
        }
        months += to - from.max(end + 1) + 1;
        end = to;
    }
    months / MONTHS_PER_YEAR
}

/// The month index (0-11) of a month name or number, or the default (1-12).
fn month(name: &str, number: &str, default: i64) -> i64 {
    if let Ok(n) = number.parse::<i64>()
        && (1..=MONTHS_PER_YEAR).contains(&n)
    {
        return n - 1;
    }
    let key: String = name.to_lowercase().chars().take(3).collect();
    MONTHS
        .iter()
        .find(|(abbreviation, _)| *abbreviation == key)
        .map_or(default, |&(_, n)| n)
        - 1
}

/// Non-empty lines, bullets and list markers dropped.
fn lines(text: &str) -> Vec<String> {
    LINE_BREAK
        .split(text)
        .map(|line| BULLET.replace(line, "").trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// The lines under headings matching `start`, up to a heading matching `stop`.
fn section(lines: &[String], start: &Regex, stop: &Regex) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in lines {
        let short = line.chars().count() <= MAX_HEADING_CHARS;
        if short && start.is_match(line) {
            inside = true;
            // "Du er struktureret" is both a heading and a requirement.
            if !start
                .replace(line, "")
                .trim_matches(|c| HEADING_PUNCTUATION.contains(c))
                .is_empty()
            {
                out.push(line.clone());
            }
            continue;
        }
        if short && stop.is_match(line) {
            inside = false;
            continue;
        }
        if inside {
            out.push(line.clone());
        }
    }
    out
}

fn requirement(value: String, english: String, kind: impl Into<String>, required: bool, years: i64) -> Requirement {
    Requirement {
        value,
        english,
        kind: kind.into(),
        required,
        alt_group: String::new(),
        min_years: years,
        min_level: String::new(),
    }
}
/// Non-empty and only ASCII digits: "2026", not "B2".
fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}
