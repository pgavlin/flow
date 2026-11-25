//! Wikipedia-specific URL acquisition belongs to this example host, not an importer.
use super::*;

pub struct UrlRequest {
    pub request_url: String,
    /// Article URL, not the API URL: relative links and note references use it.
    pub source_url: String,
    pub wikipedia: bool,
}

/// Plan one host request without networking. Only article/view URLs on the
/// actual Wikipedia domain are rewritten; edit/diff/search/API URLs are not.
pub fn url_request(reference: &str) -> Result<UrlRequest> {
    let url = external_url(reference)?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Credentials in URLs are not supported"
    );
    let unchanged = || UrlRequest {
        request_url: url.to_string(),
        source_url: url.to_string(),
        wikipedia: false,
    };
    let host = url.host_str().unwrap();
    if !(host == "wikipedia.org" || host.ends_with(".wikipedia.org")) || url.port().is_some() {
        return Ok(unchanged());
    }
    let query: Vec<_> = url.query_pairs().collect();
    if query.iter().any(|(key, value)| {
        !matches!(key.as_ref(), "title" | "oldid" | "action")
            || (key == "action" && value != "view")
    }) {
        return Ok(unchanged());
    }
    let api_title = url
        .path()
        .strip_prefix("/api/rest_v1/page/html/")
        .and_then(|path| path.split('/').next());
    let title = if let Some(title) = api_title {
        percent_encoding::percent_decode_str(title)
            .decode_utf8()?
            .into_owned()
    } else if let Some(title) = url.path().strip_prefix("/wiki/") {
        percent_encoding::percent_decode_str(title)
            .decode_utf8()?
            .into_owned()
    } else if url.path() == "/w/index.php" {
        let Some((_, title)) = query.iter().find(|(key, _)| key == "title") else {
            return Ok(unchanged());
        };
        title.to_string()
    } else {
        return Ok(unchanged());
    };
    if title.is_empty() || title.starts_with("Special:") {
        return Ok(unchanged());
    }
    let revisions: Vec<_> = query
        .iter()
        .filter(|(key, _)| key == "oldid")
        .map(|(_, value)| value.as_ref())
        .collect();
    ensure!(revisions.len() <= 1, "Ambiguous Wikipedia revision");
    let revision = revisions.first().copied();
    if let Some(revision) = revision {
        ensure!(
            !revision.is_empty()
                && revision.bytes().all(|byte| byte.is_ascii_digit())
                && revision.parse::<u64>().is_ok_and(|value| value > 0),
            "Invalid Wikipedia revision"
        );
    }
    // Mobile article URLs use the same canonical article and REST endpoints.
    let host = host.strip_suffix(".m.wikipedia.org").map_or_else(
        || host.to_owned(),
        |language| format!("{language}.wikipedia.org"),
    );
    let mut endpoint = Url::parse(&format!("https://{host}/"))?;
    {
        let mut path = endpoint.path_segments_mut().unwrap();
        path.clear()
            .extend(["api", "rest_v1", "page", "html", &title]);
        if let Some(revision) = revision {
            path.push(revision);
        }
    }
    let mut source = Url::parse(&format!("https://{host}/"))?;
    source
        .path_segments_mut()
        .unwrap()
        .clear()
        .push("wiki")
        .extend(title.split('/'));
    source.set_fragment(url.fragment());
    Ok(UrlRequest {
        request_url: if api_title.is_some() {
            url.to_string()
        } else {
            endpoint.into()
        },
        source_url: source.into(),
        wikipedia: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikipedia_articles_revisions_and_fragments_are_host_requests() {
        let request = url_request("https://en.wikipedia.org/wiki/Article/Subpage#Details").unwrap();
        assert!(request.wikipedia);
        assert_eq!(
            request.request_url,
            "https://en.wikipedia.org/api/rest_v1/page/html/Article%2FSubpage"
        );
        assert_eq!(
            request.source_url,
            "https://en.wikipedia.org/wiki/Article/Subpage#Details"
        );
        let request =
            url_request("https://de.m.wikipedia.org/w/index.php?title=Title&oldid=123#Notes")
                .unwrap();
        assert_eq!(
            request.request_url,
            "https://de.wikipedia.org/api/rest_v1/page/html/Title/123"
        );
        assert_eq!(
            request.source_url,
            "https://de.wikipedia.org/wiki/Title#Notes"
        );
        let request =
            url_request("https://en.wikipedia.org/api/rest_v1/page/html/Article/123").unwrap();
        assert!(request.wikipedia);
        assert_eq!(
            request.request_url,
            "https://en.wikipedia.org/api/rest_v1/page/html/Article/123"
        );
        assert_eq!(request.source_url, "https://en.wikipedia.org/wiki/Article");
        let request = url_request("https://fr.wikipedia.org/wiki/%C3%89nergie").unwrap();
        assert_eq!(
            request.request_url,
            "https://fr.wikipedia.org/api/rest_v1/page/html/%C3%89nergie"
        );
    }

    #[test]
    fn other_hosts_actions_and_api_urls_are_unchanged() {
        for url in [
            "https://wikipedia.org.attacker.test/wiki/Article",
            "https://notwikipedia.org/wiki/Article",
            "https://example.test/wiki/Article",
            "https://en.wikipedia.org/w/index.php?title=Article&action=edit",
            "https://en.wikipedia.org/wiki/Article?diff=123",
            "https://en.wikipedia.org/wiki/Special:Search",
            "https://en.wikipedia.org:8443/wiki/Article",
        ] {
            let request = url_request(url).unwrap();
            assert!(!request.wikipedia, "{url}");
            assert_eq!(request.request_url, url);
        }
        assert!(url_request("https://user@en.wikipedia.org/wiki/Article").is_err());
        assert!(url_request("https://en.wikipedia.org/wiki/Article?oldid=invalid").is_err());
    }
}
