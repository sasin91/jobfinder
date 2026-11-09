//! Reading the job search form: the sites box as domains, a post's link as the same post is
//! always written, and the limits the form and the scan keep to (recruiter's Job_search_rules).

use std::sync::LazyLock;

use regex::Regex;
use sha1::{Digest, Sha1};

/// Most sites a search can be limited to.
pub const MAX_SITES: usize = 20;

/// Longest name a search can have, in characters.
pub const MAX_NAME: usize = 120;
/// Longest "what to look for", in characters.
pub const MAX_KEYWORDS: usize = 500;
/// Longest "where", and longest title, company or location kept of a found post, in characters.
pub const MAX_FIELD: usize = 255;
/// Longest sites box, in characters.
pub const MAX_SITES_TEXT: usize = 2000;
/// Longest link of a found post, in characters.
pub const MAX_URL: usize = 2048;
/// Longest error kept with a post, a run or a pause, in characters.
pub const MAX_ERROR: usize = 500;
/// The highest score a search can tailor from (a percent).
/// 101 deliberately disables automatic document writing.
pub const MAX_TAILOR_FROM: i64 = 101;

/// Search terms a search's "what to look for" is read as, at most: each one is a search of
/// its own on each site.
pub const MAX_SEARCH_TERMS: usize = 5;

/// One term of "what to look for": separated by commas, semicolons or lines.
static TERM_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[,;\n]+").expect("term separators"));

/// One entry of the sites box: separated by spaces, commas or semicolons.
static SITE_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s,;]+").expect("site separators"));
/// A domain: labels of letters, digits and inner hyphens, ending in a top-level domain.
static DOMAIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,}$").expect("domain"));
/// Query parameters that only track where a click came from.
static TRACKING_PARAMETER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(utm_|fbclid$|gclid$|mc_|ref$|refid$|trackingid$|trk)").expect("tracking parameter")
});

/// The sites box (one per line, or separated by commas or spaces) as lowercase domains without
/// "www.", in order and without repeats, and what in it isn't a domain. Pasted links are cut
/// down to their domain ("https://www.jobnet.dk/find-job" is "jobnet.dk").
pub fn sites(text: &str) -> (Vec<String>, Vec<String>) {
    let mut sites: Vec<String> = Vec::new();
    let mut bad = Vec::new();
    for entry in SITE_SEPARATORS.split(text.trim()).filter(|entry| !entry.is_empty()) {
        let host = site_host(entry);
        if DOMAIN.is_match(&host) && host.len() <= MAX_FIELD {
            if !sites.contains(&host) {
                sites.push(host);
            }
        } else {
            bad.push(entry.to_string());
        }
    }
    (sites, bad)
}

/// An entry of the sites box as a host: lowercase, from a link's host, without "www." or a path.
fn site_host(entry: &str) -> String {
    let lower = entry.to_lowercase();
    let host = if lower.contains("://") {
        url::Url::parse(&lower)
            .ok()
            .and_then(|link| link.host_str().map(str::to_string))
            .unwrap_or_default()
    } else {
        lower
    };
    let host = host.split('/').next().unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// Why the sites box can't be saved, or None: something in it isn't a site, or it has too many.
pub fn sites_problem(text: &str) -> Option<String> {
    let (sites, bad) = sites(text);
    if !bad.is_empty() {
        let shown: Vec<&str> = bad.iter().take(5).map(String::as_str).collect();
        return Some(format!(
            "These aren't sites: {}. Write one site per line, like jobnet.dk.",
            shown.join(", ")
        ));
    }
    (sites.len() > MAX_SITES).then(|| format!("Pick at most {MAX_SITES} sites."))
}

/// A link as the same post is always written: lowercase scheme and host, no "www.", no
/// fragment, no tracking parameters (utm_*, fbclid, gclid, refId and the like), the rest sorted,
/// and no trailing slash. A link that doesn't parse is only trimmed.
pub fn normal_url(link: &str) -> String {
    let link = link.trim();
    let Ok(parsed) = url::Url::parse(link) else {
        return link.to_string();
    };
    let Some(host) = parsed.host_str() else {
        return link.to_string();
    };
    let host = host.strip_prefix("www.").unwrap_or(host);
    let port = parsed.port().map(|port| format!(":{port}")).unwrap_or_default();
    let path = parsed.path().trim_end_matches('/');
    let mut query: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| !TRACKING_PARAMETER.is_match(key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    query.sort();
    let query = if query.is_empty() {
        String::new()
    } else {
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&query)
            .finish();
        format!("?{encoded}")
    };
    format!("{}://{host}{port}{path}{query}", parsed.scheme())
}

/// The post's url_hash: sha1 of its normalised link, in hex.
pub fn url_hash(link: &str) -> String {
    let digest = Sha1::digest(normal_url(link).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether text is an http(s) link the scan can keep.
pub fn is_http_link(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && text.chars().count() <= MAX_URL
        && url::Url::parse(text).is_ok_and(|link| link.host_str().is_some())
}

/// "PHP udvikler, Laravel; backend" as the terms the sites are searched for, each once, at most
/// MAX_SEARCH_TERMS.
pub fn search_terms(keywords: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for term in TERM_SEPARATORS
        .split(keywords)
        .map(str::trim)
        .filter(|term| !term.is_empty())
    {
        if !terms.iter().any(|known| known.eq_ignore_ascii_case(term)) && terms.len() < MAX_SEARCH_TERMS {
            terms.push(term.to_string());
        }
    }
    terms
}

/// The first max characters of text, trimmed.
pub fn cut(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sites_box_reads_as_domains() {
        let (sites, bad) = sites("https://www.jobnet.dk/find-job\njobindex.dk, JOBNET.dk; not a site");
        assert_eq!(sites, vec!["jobnet.dk", "jobindex.dk"]);
        assert_eq!(bad, vec!["not", "a", "site"]);
        assert_eq!(sites_problem("jobnet.dk"), None);
        assert!(sites_problem("nope").unwrap().starts_with("These aren't sites: nope."));
    }

    #[test]
    fn the_same_post_is_always_written_the_same_way() {
        assert_eq!(
            normal_url(" https://WWW.Jobnet.dk/job/123/?utm_source=x&b=2&a=1#top "),
            "https://jobnet.dk/job/123?a=1&b=2"
        );
        assert_eq!(
            normal_url("https://jobnet.dk/job/123?fbclid=x"),
            "https://jobnet.dk/job/123"
        );
        assert_eq!(
            url_hash("https://www.jobnet.dk/job/123/"),
            url_hash("https://jobnet.dk/job/123?utm_medium=mail")
        );
        assert_eq!(url_hash("https://jobnet.dk/job/123").len(), 40);
    }

    #[test]
    fn what_to_look_for_reads_as_search_terms() {
        assert_eq!(
            search_terms(" PHP udvikler, Laravel;\nbackend,php udvikler ,, "),
            vec!["PHP udvikler", "Laravel", "backend"]
        );
        assert_eq!(search_terms("a,b,c,d,e,f,g").len(), MAX_SEARCH_TERMS);
        assert!(search_terms(" , ").is_empty());
    }

    #[test]
    fn only_http_links_are_kept() {
        assert!(is_http_link("https://jobnet.dk/job/1"));
        assert!(is_http_link("HTTP://jobnet.dk"));
        assert!(!is_http_link("ftp://jobnet.dk"));
        assert!(!is_http_link("jobnet.dk/job/1"));
    }
}
