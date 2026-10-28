//! What an address typed in the Web panel means: a dev server port, a local host, a file, or
//! a site.

/// The URL to load for what was typed, or `None` for nothing.
///
/// - a URL with a scheme (`https://…`, `file:///…`, `about:blank`) stays as it is;
/// - a port (`3000`, `:3000`) is that port on this machine: `http://localhost:3000`;
/// - this machine by name or address (`localhost:3000/app`, `127.0.0.1:8080`) gets `http://`;
/// - an absolute path is a file;
/// - anything else is a site and gets `https://`.
pub fn normalize_url(input: &str) -> Option<String> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }
    if has_scheme(text) {
        return Some(text.to_string());
    }
    let port = text.strip_prefix(':').unwrap_or(text);
    if !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) {
        return Some(format!("http://localhost:{port}"));
    }
    if text.starts_with('/') {
        return Some(format!("file://{text}"));
    }
    if is_local(text) {
        return Some(format!("http://{text}"));
    }
    Some(format!("https://{text}"))
}

/// The `file://` address of `path`, with what an address can't hold as it is encoded (a
/// space, `%`, `#`, `?`).
pub fn file_url(path: &std::path::Path) -> String {
    let mut url = String::from("file://");
    for c in path.to_string_lossy().chars() {
        match c {
            ' ' => url.push_str("%20"),
            '%' => url.push_str("%25"),
            '#' => url.push_str("%23"),
            '?' => url.push_str("%3F"),
            c => url.push(c),
        }
    }
    url
}

/// `scheme://…`, or one of the schemes written without slashes.
fn has_scheme(text: &str) -> bool {
    if let Some((scheme, _)) = text.split_once("://") {
        let mut chars = scheme.chars();
        return chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    }
    ["about:", "data:", "mailto:"].iter().any(|s| text.starts_with(s))
}

/// The host part names this machine.
fn is_local(text: &str) -> bool {
    ["localhost", "127.0.0.1", "0.0.0.0", "[::1]"].iter().any(|h| {
        text.strip_prefix(h)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/', '?', '#']))
    })
}

/// A link that is a web page (http or https): opened in the Web panel rather than handed to
/// the system.
pub fn is_web_link(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The web address in `line` that covers byte `at` (as in a terminal row), if any: from its
/// scheme to the first space, quote or bracket, without a trailing full stop or comma.
pub fn web_link_at(line: &str, at: usize) -> Option<String> {
    web_link_span(line, at).map(|(from, to)| line[from..to].to_string())
}

/// Where in `line` (bytes) the web address at byte `at` is, if there is one.
pub fn web_link_span(line: &str, at: usize) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(i) = line[from..].find("http").map(|i| from + i) {
        let rest = &line[i..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let len = rest.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | '`')).unwrap_or(rest.len());
            let url = rest[..len].trim_end_matches(['.', ',', ';', ':']);
            if i <= at && at < i + url.len() {
                return Some((i, i + url.len()));
            }
            from = i + len.max(1);
        } else {
            from = i + 4;
        }
    }
    None
}

/// A short name for a URL, for a tab before the page has a title: its host (with the port),
/// else the file name, else the URL.
pub fn short_name(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    if url.starts_with("file://") {
        return rest.rsplit('/').find(|s| !s.is_empty()).unwrap_or(rest).to_string();
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if host.is_empty() { url.to_string() } else { host.to_string() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_address_encodes_spaces() {
        assert_eq!(super::file_url(std::path::Path::new("/Volumes/Extreme SSD/t #1/a.html")), "file:///Volumes/Extreme%20SSD/t%20%231/a.html");
    }

    #[test]
    fn web_links_in_a_line() {
        let line = "Serving at http://localhost:3000/app, see (https://example.com).";
        assert_eq!(web_link_at(line, 15).as_deref(), Some("http://localhost:3000/app"));
        assert_eq!(web_link_at(line, 50).as_deref(), Some("https://example.com"));
        assert_eq!(web_link_at(line, 2), None);
        assert!(is_web_link("HTTPS://x.y") && !is_web_link("file:///a") && !is_web_link("mailto:a@b"));
    }

    use super::*;

    fn n(s: &str) -> Option<String> {
        normalize_url(s)
    }

    #[test]
    fn ports_are_this_machine() {
        assert_eq!(n("3000").as_deref(), Some("http://localhost:3000"));
        assert_eq!(n(" :8080 ").as_deref(), Some("http://localhost:8080"));
        assert_eq!(n("localhost:3000").as_deref(), Some("http://localhost:3000"));
        assert_eq!(n("localhost").as_deref(), Some("http://localhost"));
        assert_eq!(n("127.0.0.1:5173/app?x=1").as_deref(), Some("http://127.0.0.1:5173/app?x=1"));
        assert_eq!(n("[::1]:4000").as_deref(), Some("http://[::1]:4000"));
    }

    #[test]
    fn sites_get_https() {
        assert_eq!(n("example.com").as_deref(), Some("https://example.com"));
        assert_eq!(n("docs.rs/wry").as_deref(), Some("https://docs.rs/wry"));
        // A host that only starts like "localhost" is a site.
        assert_eq!(n("localhostname.dev").as_deref(), Some("https://localhostname.dev"));
        // Too long for a port.
        assert_eq!(n("1234567").as_deref(), Some("https://1234567"));
    }

    #[test]
    fn schemes_and_files_stay() {
        assert_eq!(n("http://example.com").as_deref(), Some("http://example.com"));
        assert_eq!(n("https://a.b/c").as_deref(), Some("https://a.b/c"));
        assert_eq!(n("file:///tmp/x.html").as_deref(), Some("file:///tmp/x.html"));
        assert_eq!(n("about:blank").as_deref(), Some("about:blank"));
        assert_eq!(n("/tmp/report.html").as_deref(), Some("file:///tmp/report.html"));
        assert_eq!(n("   "), None);
    }

    #[test]
    fn short_names() {
        assert_eq!(short_name("http://localhost:3000/app"), "localhost:3000");
        assert_eq!(short_name("https://docs.rs/wry?x#y"), "docs.rs");
        assert_eq!(short_name("file:///tmp/report.html"), "report.html");
        assert_eq!(short_name("about:blank"), "about:blank");
    }
}
