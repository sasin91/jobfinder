//! The job sites the app reads itself, without a language model searching the web: their own
//! search (Jobnet's JSON API, Jobindex's search page) for the job scan, and Jobnet's API for one
//! of its posts, whose page shows the post with JavaScript. Pages that need JavaScript
//! elsewhere are read in the computer's own browser (browser). LinkedIn and Indeed, which show
//! their posts to a signed-in browser, are read in JobFinder's own signed-in browser (signed_in),
//! and only for a search that names them in its sites.
//!
//! Every request the app makes itself goes through page_reader's fetching rules (public
//! addresses only, every redirect checked).

pub mod browser;
pub mod cdp;
pub mod indeed;
pub mod jobindex;
pub mod jobnet;
pub mod linkedin;
pub mod signed_in;

use crate::matches::page_reader::{self, Page, ReadWith, Readers};
use signed_in::{Seen, SignedInBrowser};

/// A site the app searches itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobSite {
    Jobnet,
    Jobindex,
    Linkedin,
    Indeed,
}

impl JobSite {
    /// The sites a search without sites of its own is read on: the ones read without signing in.
    pub const ALL: [JobSite; 2] = [JobSite::Jobnet, JobSite::Jobindex];

    /// The sites read in JobFinder's signed-in browser, for a search that names them.
    pub const SIGNED_IN: [JobSite; 2] = [JobSite::Linkedin, JobSite::Indeed];

    /// The site's domain as the sites box keeps it (job_search_rules::sites).
    pub fn domain(self) -> &'static str {
        match self {
            JobSite::Jobnet => jobnet::DOMAIN,
            JobSite::Jobindex => jobindex::DOMAIN,
            JobSite::Linkedin => linkedin::DOMAIN,
            JobSite::Indeed => indeed::DOMAIN,
        }
    }

    /// Whether the site is read in JobFinder's signed-in browser.
    pub fn is_signed_in(self) -> bool {
        JobSite::SIGNED_IN.contains(&self)
    }

    /// The site the app reads for a domain of the sites box, if it reads it.
    pub fn of(domain: &str) -> Option<JobSite> {
        if domain == indeed::SHORT_DOMAIN {
            return Some(JobSite::Indeed);
        }
        JobSite::ALL
            .into_iter()
            .chain(JobSite::SIGNED_IN)
            .find(|site| site.domain() == domain)
    }

    /// The site's newest posts for one search term, near place ("" for anywhere). A signed-in
    /// site is read in signed_in, and can't be without one.
    pub async fn search(
        self,
        term: &str,
        place: &str,
        signed_in: Option<&SignedInBrowser>,
    ) -> Result<Vec<ListedPost>, String> {
        match (self, signed_in) {
            (JobSite::Jobnet, _) => jobnet::search(term, place).await,
            (JobSite::Jobindex, _) => jobindex::search(term, place).await,
            (JobSite::Linkedin, Some(browser)) => linkedin::search(browser, term, place).await,
            (JobSite::Indeed, Some(browser)) => indeed::search(browser, term, place).await,
            (_, None) => Err("JobFinder has no browser of its own here to read it in.".to_string()),
        }
    }
}

/// The content type of HTML the browser has decoded already (the extension's), so a <meta
/// charset> left in it isn't applied a second time.
const DECODED_HTML: &str = "text/html; charset=utf-8";

/// The pages JobFinder's signed-in browser opens on to sign in to the signed-in sites.
pub const SIGN_IN_PAGES: [&str; 2] = [linkedin::SIGN_IN_PAGE, indeed::SITE];

/// A post as a site's search lists it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListedPost {
    pub url: String,
    pub title: String,
    pub company: String,
    pub location: String,
}

/// The post at url read by its site's own reader, when it is a site whose posts are read that
/// way: Jobnet from its API, LinkedIn and Indeed in the signed-in browser (when readers has
/// one), else None.
pub async fn read_post(url: &str, readers: Readers<'_>) -> Option<Result<Page, String>> {
    if let Some(id) = jobnet::post_id(url) {
        return Some(jobnet::read_post(&id).await);
    }
    let browser = readers.signed_in?;
    let (link, read) = if let Some(link) = linkedin::post_link(url) {
        let read = linkedin::read_post(browser, &link).await;
        (link, read)
    } else {
        let link = indeed::post_link(url)?;
        let read = indeed::read_post(browser, &link).await;
        (link, read)
    };
    Some(read.map(|text| Page {
        read_with: ReadWith::SignedIn,
        ..Page::of(link, text)
    }))
}

/// Whether url is a post of a site read in the signed-in browser (LinkedIn, Indeed): when that
/// read fails, it is not fetched some other way, since the site may have asked the browser to
/// sign in or for a check, and the scan leaves it alone then.
pub fn is_signed_in_post(url: &str) -> bool {
    linkedin::post_link(url).is_some() || indeed::post_link(url).is_some()
}

/// A post's link as its site always writes it (Jobindex's, LinkedIn's and Indeed's, however they
/// were found), so the same post found twice is one post and Jobindex's /vis-job/ teasers go to
/// the full /jobannonce/ post; other links as they are.
pub fn post_link(url: &str) -> String {
    jobindex::post_link(url)
        .or_else(|| linkedin::post_link(url))
        .or_else(|| indeed::post_link(url))
        .unwrap_or_else(|| url.trim().to_string())
}

/// The post a page the user's browser showed has (its HTML as shown, sent by the extension),
/// read as its site's reader reads it (LinkedIn, Indeed), else as any page is read. url is
/// post_link's.
pub fn sent_page(url: &str, html: &str) -> Result<Page, String> {
    let by_site = if linkedin::post_link(url).is_some() {
        linkedin::post_page(url, html)
    } else if indeed::post_link(url).is_some() {
        indeed::post_page(url, html)
    } else {
        Seen::NotYet
    };
    let text = match by_site {
        Seen::Shown(text) => text,
        _ => page_reader::to_text(html.as_bytes(), DECODED_HTML, url)?,
    };
    Ok(Page {
        read_with: ReadWith::Sent,
        ..Page::of(url.to_string(), text)
    })
}

/// A JSON value's text, "" when it isn't text.
fn text<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("").trim()
}

/// The first of texts that isn't empty, or "".
fn first_of<'a>(texts: &[&'a str]) -> &'a str {
    texts.iter().copied().find(|text| !text.is_empty()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sent_page_is_read_as_the_browser_decoded_it() {
        let html = "<html><head><meta charset=\"iso-8859-1\"><title>Udvikler i Århus</title></head><body><main><p>Vi søger en udvikler til vores kontor i Århus. Du får gode kolleger, frokostordning og fleksible arbejdstider, og du arbejder med PHP, Rust og SQLite hver dag i et lille team.</p></main></body></html>";
        let page = sent_page("https://example.com/job/1", html).unwrap();
        assert_eq!(page.title, "Udvikler i Århus");
        assert!(page.text.contains("Vi søger"), "{}", page.text);
        assert_eq!(page.read_with, ReadWith::Sent);
    }

    #[test]
    fn signed_in_posts_are_known_by_their_links() {
        assert!(is_signed_in_post("https://www.linkedin.com/jobs/view/4012345678/"));
        assert!(is_signed_in_post("https://dk.indeed.com/viewjob?jk=0123456789abcdef"));
        assert!(!is_signed_in_post("https://www.jobindex.dk/vis-job/h1"));
    }

    #[test]
    fn post_link_rewrites_jobindex_teasers_to_jobannonce() {
        assert_eq!(
            post_link("https://www.jobindex.dk/vis-job/h1710286"),
            "https://www.jobindex.dk/jobannonce/h1710286"
        );
    }
}
