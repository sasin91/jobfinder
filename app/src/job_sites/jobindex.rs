//! Jobindex (jobindex.dk), read from its search page: the page carries its results as JSON in
//! a script ("var Stash = {…}"), under a searchResponse with its results. Jobindex's search has
//! no place of its own: the place goes in the search words, which it matches with the posts'
//! towns too.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{ListedPost, first_of, text};
use crate::matches::page_reader;

/// The domain the sites box keeps for Jobindex.
pub const DOMAIN: &str = "jobindex.dk";

/// Where Jobindex is.
const SITE: &str = "https://www.jobindex.dk";

/// The oldest post a search lists, in days since it was published.
const MAX_AGE_DAYS: u32 = 14;

/// Where the search page's results begin.
const STASH: &str = "var Stash = ";

/// How deep into the page's JSON the results are looked for.
const MAX_DEPTH: usize = 12;

static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").expect("tag"));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("whitespace"));
/// A Jobindex post's id in its link: /vis-job/{id} (teaser) or /jobannonce/{id} (full post or
/// redirect to the employer's page).
static POST_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://(?:www\.)?jobindex\.dk/(vis-job|jobannonce)/([0-9a-z][0-9a-z_-]{0,63})(?:/[^?#]*)?(?:[?#].*)?$")
        .expect("Jobindex post id")
});

/// The post's link as Jobindex's full post (/jobannonce/{id}), when url is a Jobindex post's
/// (/vis-job/ teaser or /jobannonce/).
pub fn post_link(url: &str) -> Option<String> {
    let found = POST_ID.captures(url.trim())?;
    Some(format!("{SITE}/jobannonce/{}", found[2].to_lowercase()))
}

/// Whether url is Jobindex's short teaser page (/vis-job/{id}), which shows only a few lines and
/// a "Se jobbet" button rather than the full post.
pub fn is_teaser(url: &str) -> bool {
    POST_ID
        .captures(url.trim())
        .is_some_and(|found| found[1].eq_ignore_ascii_case("vis-job"))
}

/// Jobindex's newest posts for term, near place ("" for anywhere).
pub async fn search(term: &str, place: &str) -> Result<Vec<ListedPost>, String> {
    let fetched = page_reader::get(&search_url(term, place), &page_reader::PAGE).await?;
    listed(&String::from_utf8_lossy(&fetched.body))
}

/// The search page for term and place, newest first, of the last MAX_AGE_DAYS.
fn search_url(term: &str, place: &str) -> String {
    let words = format!("{term} {}", place.trim());
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("q", words.trim())
        .append_pair("sort", "date")
        .append_pair("jobage", &MAX_AGE_DAYS.to_string());
    format!("{SITE}/jobsoegning?{}", query.finish())
}

/// The posts of a search page.
fn listed(html: &str) -> Result<Vec<ListedPost>, String> {
    let stash = stash(html).ok_or("Jobindex's search page has no results in it any more.")?;
    let stash: Value =
        serde_json::from_str(stash).map_err(|e| format!("Jobindex's search page has results that don't read: {e}"))?;
    let results = search_results(&stash, 0).ok_or("Jobindex's search page has no results in it any more.")?;
    Ok(results.iter().filter_map(post).collect())
}

/// The JSON object after "var Stash = ", up to its closing brace (outside strings).
fn stash(html: &str) -> Option<&str> {
    let start = html.find(STASH)? + STASH.len();
    let rest = &html[start..];
    if !rest.starts_with('{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (at, c) in rest.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..=at]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The first searchResponse's results anywhere in value, at most MAX_DEPTH down.
fn search_results(value: &Value, depth: usize) -> Option<&Vec<Value>> {
    if depth > MAX_DEPTH {
        return None;
    }
    match value {
        Value::Object(object) => {
            if let Some(results) = object
                .get("searchResponse")
                .and_then(|found| found["results"].as_array())
            {
                return Some(results);
            }
            object.values().find_map(|inner| search_results(inner, depth + 1))
        }
        Value::Array(items) => items.iter().find_map(|inner| search_results(inner, depth + 1)),
        _ => None,
    }
}

/// Where a post is read. On Jobindex, /vis-job/ (share_url) is only a teaser card with a
/// "Se jobbet" link and recommended jobs — even for ads with tid "h…". /jobannonce/ is always
/// the full post: Jobindex's own ad page when hosted there, or a redirect to the employer's own
/// page (both for external "h…" ads and robot "r…" ads).
fn post_url(id: &str, share_url: &str) -> String {
    if !id.is_empty() {
        format!("{SITE}/jobannonce/{}", id.to_lowercase())
    } else {
        post_link(share_url).unwrap_or_else(|| share_url.to_string())
    }
}

/// One result as a post, when it has a link.
fn post(result: &Value) -> Option<ListedPost> {
    let url = post_url(text(result, "tid"), text(result, "share_url"));
    if url.is_empty() {
        return None;
    }
    Some(ListedPost {
        url,
        title: plain(text(result, "headline")),
        company: plain(first_of(&[
            text(&result["company"], "name"),
            text(result, "companytext"),
        ])),
        location: plain(text(result, "area")),
    })
}

/// Text from Jobindex's HTML snippets: without tags or entities, on one line.
fn plain(html: &str) -> String {
    let without_tags = TAG.replace_all(html, " ");
    let decoded = html_escape::decode_html_entities(&without_tags);
    WHITESPACE.replace_all(&decoded, " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_puts_the_place_in_the_words() {
        assert_eq!(
            search_url("PHP udvikler", "Odense"),
            "https://www.jobindex.dk/jobsoegning?q=PHP+udvikler+Odense&sort=date&jobage=14"
        );
        assert_eq!(
            search_url("PHP", ""),
            "https://www.jobindex.dk/jobsoegning?q=PHP&sort=date&jobage=14"
        );
    }

    #[test]
    fn the_search_page_lists_the_posts_in_its_stash() {
        let html = r#"<html><script>var Stash = {"jobsearch/result_app":{"storeData":{"searchResponse":{"hitcount":2,"results":[
            {"tid":"h1647303","share_url":"https://www.jobindex.dk/vis-job/h1647303","headline":"PHP-udvikler &amp; <b>Laravel</b>","company":{"name":"Acme A/S"},"area":"Odense"},
            {"tid":"","share_url":"https://www.jobindex.dk/vis-job/r1","headline":"Backend {}","companytext":"B \"x\"","area":"Vejle"},
            {"tid":"","headline":"No link"}
        ]}}}};
        var Other = {};</script></html>"#;
        let posts = listed(html).unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(
            posts[0],
            ListedPost {
                url: "https://www.jobindex.dk/jobannonce/h1647303".to_string(),
                title: "PHP-udvikler & Laravel".to_string(),
                company: "Acme A/S".to_string(),
                location: "Odense".to_string(),
            }
        );
        assert_eq!(posts[1].url, "https://www.jobindex.dk/jobannonce/r1");
        assert_eq!(posts[1].company, "B \"x\"");
        assert!(listed("<html>nothing</html>").is_err());
        assert_eq!(
            post_url("r14077115", "x"),
            "https://www.jobindex.dk/jobannonce/r14077115"
        );
        assert_eq!(
            post_url("h1710286", "https://www.jobindex.dk/vis-job/h1710286"),
            "https://www.jobindex.dk/jobannonce/h1710286"
        );
    }

    #[test]
    fn a_jobindex_teaser_link_points_to_the_full_jobannonce() {
        assert_eq!(
            post_link("https://www.jobindex.dk/vis-job/h1710286"),
            Some("https://www.jobindex.dk/jobannonce/h1710286".to_string())
        );
        assert_eq!(
            post_link("https://jobindex.dk/vis-job/H1710286?utm_source=test"),
            Some("https://www.jobindex.dk/jobannonce/h1710286".to_string())
        );
        assert_eq!(
            post_link("https://www.jobindex.dk/jobannonce/h1710286/capacity-manager"),
            Some("https://www.jobindex.dk/jobannonce/h1710286".to_string())
        );
        assert_eq!(post_link("https://www.jobindex.dk/jobsoegning?q=rust"), None);
        assert_eq!(post_link("https://example.com/vis-job/h1710286"), None);
        assert!(is_teaser("https://www.jobindex.dk/vis-job/h1710286"));
        assert!(!is_teaser("https://www.jobindex.dk/jobannonce/h1710286"));
        assert!(!is_teaser("https://jndata-career.talent-soft.com/job/job-capacity-manager_545.aspx"));
    }
}
