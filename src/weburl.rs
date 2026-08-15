//! URL normalization for external-link identity — one web node per URL
//! however it was written in whichever note. Deliberately mild: lowercase
//! the scheme and host, drop the fragment and tracking params, strip
//! trailing slashes from the path, and KEEP every other query param
//! (`?id=123` often *is* the page). Pure, deterministic, headless-tested.

/// Query params that never distinguish pages — tracking noise.
fn tracking_param(name: &str) -> bool {
    name.starts_with("utm_") || matches!(name, "fbclid" | "gclid" | "mc_cid" | "mc_eid")
}

/// Canonical identity for a URL. Non-URLs (no `://`) pass through
/// unchanged — garbage in, deterministic garbage out.
pub fn normalize(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let rest = rest.split('#').next().unwrap_or(rest);
    let host_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (host, tail) = rest.split_at(host_end);
    // www. never distinguishes a resource — the same page cited with and
    // without it must be one node
    let host = host.strip_prefix("www.").unwrap_or(host);
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    let path = path.trim_end_matches('/');
    let query = query
        .map(|q| {
            q.split('&')
                .filter(|kv| {
                    let name = kv.split('=').next().unwrap_or(kv);
                    !kv.is_empty() && !tracking_param(name)
                })
                .collect::<Vec<_>>()
                .join("&")
        })
        .unwrap_or_default();
    let mut out = format!(
        "{}://{}{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase(),
        path
    );
    if !query.is_empty() {
        out.push('?');
        out.push_str(&query);
    }
    out
}

/// Display label for a web node: the host without a leading `www.`.
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let h = &rest[..end];
    h.strip_prefix("www.").unwrap_or(h)
}

/// Cap on humanized slug titles (canvas labels must stay scannable).
const TITLE_CAP: usize = 48;

/// A human title mined from the URL itself: the last path segment that
/// looks like a slug ("openai-slows-down-astra-development" → words),
/// skipping numeric ids and UUID-ish junk. None when the URL carries no
/// words — the caller falls back to the host.
pub fn slug_title(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r)?;
    let path = &rest[rest.find('/')?..];
    let path = path.split(['?', '#']).next().unwrap_or(path);
    // Judge only the LAST non-empty segment — walking further up would
    // surface category names ("news", "blog") as fake titles.
    let seg = path.rsplit('/').find(|s| !s.is_empty())?;
    let seg = seg
        .trim_end_matches(".html")
        .trim_end_matches(".htm")
        .trim_end_matches(".php");
    let letters = seg.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let uuidish = seg.len() >= 20
        && seg
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-' || c == '_');
    if seg.len() < 4 || letters * 2 < seg.len() || uuidish {
        return None;
    }
    let mut title = seg.replace(['-', '_', '+'], " ");
    if title.len() > TITLE_CAP {
        let mut i = TITLE_CAP;
        while !title.is_char_boundary(i) {
            i -= 1;
        }
        title.truncate(i);
        title.push('…');
    }
    Some(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_is_mild_and_deterministic() {
        // case, fragment, trailing slash
        assert_eq!(
            normalize("HTTPS://En.Wikipedia.org/wiki/GPT-5/#History"),
            "https://en.wikipedia.org/wiki/GPT-5"
        );
        // tracking params go, real params stay, order preserved
        assert_eq!(
            normalize("https://x.com/a?id=3&utm_source=nl&b=2&fbclid=z"),
            "https://x.com/a?id=3&b=2"
        );
        // all params tracking → no '?'
        assert_eq!(
            normalize("https://x.com/a?utm_source=nl"),
            "https://x.com/a"
        );
        // bare host, with and without slash, unify
        assert_eq!(normalize("https://x.com/"), "https://x.com");
        assert_eq!(normalize("https://x.com"), "https://x.com");
        // the PATH keeps its case — only scheme and host fold
        assert_eq!(
            normalize("http://X.com/CaseSensitive"),
            "http://x.com/CaseSensitive"
        );
        // non-URLs pass through
        assert_eq!(normalize("not a url"), "not a url");
    }

    #[test]
    fn host_labels() {
        assert_eq!(host("https://www.example.com/a/b?q=1"), "example.com");
        assert_eq!(host("https://docs.rs/tmux"), "docs.rs");
        assert_eq!(host("https://x.com"), "x.com");
    }

    #[test]
    fn www_merges_into_one_identity() {
        assert_eq!(
            normalize("https://www.kucoin.com/news/x"),
            normalize("https://kucoin.com/news/x")
        );
    }

    #[test]
    fn slug_titles_humanize_and_skip_junk() {
        assert_eq!(
            slug_title("https://www.engadget.com/2233237/openai-slows-astra-development/"),
            Some("openai slows astra development".into())
        );
        // numeric ids and UUIDs are not titles; a wordless URL yields None
        assert_eq!(
            slug_title("https://finance.biggo.com/news/5ed990e1-8bd9-4c4b-a5a8-e4029b26a040"),
            None
        );
        assert_eq!(slug_title("https://x.com/12345"), None);
        assert_eq!(slug_title("https://x.com"), None);
        // short-but-wordy segments count; extensions drop
        assert_eq!(
            slug_title("https://en.wikipedia.org/wiki/GPT-5"),
            Some("GPT 5".into())
        );
        assert_eq!(
            slug_title("https://a.com/page-name.html"),
            Some("page name".into())
        );
        // very long slugs truncate on a char boundary
        let long = format!("https://a.com/{}", "word-".repeat(30));
        assert!(slug_title(&long).unwrap().ends_with('…'));
    }
}
