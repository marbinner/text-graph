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
}
