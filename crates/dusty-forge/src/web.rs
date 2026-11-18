//! The web address of a Git remote.

/// Convert Git transports to HTTPS; keep HTTP(S) schemes and web ports intact.
pub fn web_base(remote: &str) -> Option<reqwest::Url> {
    let remote = remote.trim().trim_end_matches('/');
    let remote = remote.strip_suffix(".git").unwrap_or(remote);
    let (scheme, authority, path) = if let Some((scheme, rest)) = remote.split_once("://") {
        if !matches!(scheme, "http" | "https" | "ssh" | "git") {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        (scheme, authority, path)
    } else {
        let (authority, path) = remote.split_once(':')?;
        ("ssh", authority, path)
    };
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.is_empty() || path.is_empty() {
        return None;
    }
    let is_web = matches!(scheme, "http" | "https");
    let web_scheme = if is_web { scheme } else { "https" };
    let mut base = reqwest::Url::parse(&format!("{web_scheme}://{authority}/{path}")).ok()?;
    if !is_web {
        // A Git transport port is not the web port.
        base.set_port(None).ok()?;
    }
    Some(base)
}
