//! Standards-compliant validation for web targets.
//!
//! The URL crate performs RFC/WHATWG parsing and IDNA conversion.  We retain a
//! deliberately narrow product policy on top: absolute HTTP(S), no credentials,
//! a DNS name (not an arbitrary browser-search string), and bounded input.

use serde::{Deserialize, Serialize};
use url::Url;

const MAX_URL_BYTES: usize = 2_048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedHttpUrl(String);

impl ValidatedHttpUrl {
    /// Parse a spoken or explicit URL. Unicode domains are accepted and
    /// canonicalized to their ASCII IDNA form, so platform executors receive
    /// one stable, standards-compliant representation.
    pub fn parse_spoken(input: &str) -> Result<Self, String> {
        let value = normalize_spoken_url(input)?;
        let url = Url::parse(&value).map_err(|_| "URL is malformed")?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("only http and https URLs are supported".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("URL credentials are not supported".into());
        }
        if url.host_str().is_none() || url.port().is_some_and(|port| port == 0) {
            return Err("URL must contain a valid host and port".into());
        }
        let Some(host) = url.host_str() else {
            unreachable!()
        };
        if !valid_dns_name(host) {
            return Err("URL must contain a valid DNS host name".into());
        }
        Ok(Self(url.into()))
    }

    pub fn web_search(query: &str) -> Result<Self, String> {
        let query = query.trim();
        if query.is_empty() || query.len() > 200 || query.chars().any(char::is_control) {
            return Err("search query must be between 1 and 200 single-line characters".into());
        }
        let mut url =
            Url::parse("https://www.google.com/search").expect("static search base URL is valid");
        url.query_pairs_mut().append_pair("q", query);
        Ok(Self(url.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn normalize_spoken_url(input: &str) -> Result<String, String> {
    let value = input.trim();
    if value.is_empty() || value.len() > MAX_URL_BYTES || value.chars().any(char::is_control) {
        return Err("URL must be a non-empty, bounded single-line value".into());
    }
    // Bounded ASR normalization only recognizes explicit spoken punctuation.
    // It never converts arbitrary prose into a URL host.
    let value = value.replace(" dot ", ".").replace(" slash ", "/");
    if value.contains(char::is_whitespace) || value.contains('\\') {
        return Err("URL must not contain whitespace or backslashes".into());
    }
    Ok(if value.contains("://") {
        value
    } else {
        format!("https://{value}")
    })
}

fn valid_dns_name(host: &str) -> bool {
    // `url` has already converted Unicode through IDNA and rejected malformed
    // authorities/ports. This product policy requires an ordinary dotted DNS
    // host rather than IP literals or a single-label local hostname.
    if host.len() > 253 || !host.contains('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_spoken_explicit_and_idna_domains() {
        let cases = [
            ("google dot com", "https://google.com/"),
            (
                "HTTP://Example.COM:8080/a?x=1#top",
                "http://example.com:8080/a?x=1#top",
            ),
            ("https://example.com?x=1", "https://example.com/?x=1"),
            (
                "münchen.de/path?ü=✓",
                "https://xn--mnchen-3ya.de/path?%C3%BC=%E2%9C%93",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                ValidatedHttpUrl::parse_spoken(input).unwrap().as_str(),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_unsafe_or_malformed_urls() {
        let oversized = format!("example.com/{}", "a".repeat(MAX_URL_BYTES));
        for input in [
            "file:///tmp/x",
            "javascript:alert(1)",
            "https://",
            "https://user@example.com",
            "https://example..com",
            "https://-example.com",
            "https://example.com:abc",
            "https://example.com\\path",
            "https://example.com\nfoo",
            "localhost",
            &oversized,
        ] {
            assert!(ValidatedHttpUrl::parse_spoken(input).is_err(), "{input}");
        }
    }

    #[test]
    fn encodes_search_as_data() {
        assert_eq!(
            ValidatedHttpUrl::web_search("Rust async & traits ✓")
                .unwrap()
                .as_str(),
            "https://www.google.com/search?q=Rust+async+%26+traits+%E2%9C%93"
        );
    }
}
