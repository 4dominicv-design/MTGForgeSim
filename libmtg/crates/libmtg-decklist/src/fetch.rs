//! Native-only URL resolvers (Moxfield, MTGGoldfish) with an on-disk cache.
//!
//! Gated behind the `fetch` feature so the wasm build (and the dependency-light
//! core) never pull in an HTTP stack. Raw upstream responses are cached on disk
//! (the `challenge_cache` pattern) keyed by source + deck id, so repeated runs
//! against the same deck hit the network once.

use std::path::{Path, PathBuf};

use crate::Decklist;

/// Browser-ish UA: MTGGoldfish (and Moxfield behind Cloudflare) reject the
/// default `ureq` agent.
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

#[derive(Debug)]
pub enum FetchError {
    /// URL host isn't a supported deck site, or no deck id could be extracted.
    UnsupportedUrl(String),
    /// Network / HTTP-status failure talking to the upstream site.
    Http(String),
    /// The upstream payload couldn't be parsed into a decklist.
    Parse(String),
    /// Reading or writing the on-disk cache failed.
    Io(std::io::Error),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::UnsupportedUrl(u) => write!(f, "unsupported deck URL: {u}"),
            FetchError::Http(e) => write!(f, "fetch failed: {e}"),
            FetchError::Parse(e) => write!(f, "could not parse decklist: {e}"),
            FetchError::Io(e) => write!(f, "cache I/O error: {e}"),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<std::io::Error> for FetchError {
    fn from(e: std::io::Error) -> Self {
        FetchError::Io(e)
    }
}

/// Resolve a decklist from a Moxfield or MTGGoldfish URL, caching the raw
/// upstream response under the default cache directory (`$DECKLIST_CACHE`, else
/// `./decklist_cache`).
pub fn from_url(url: &str) -> Result<Decklist, FetchError> {
    from_url_cached(url, &default_cache_dir())
}

/// Like [`from_url`], but caches into an explicit directory.
pub fn from_url_cached(url: &str, cache_dir: &Path) -> Result<Decklist, FetchError> {
    let source = DeckSource::detect(url)
        .ok_or_else(|| FetchError::UnsupportedUrl(url.to_string()))?;
    source.resolve(cache_dir)
}

/// Default cache directory: `$DECKLIST_CACHE` if set, else `./decklist_cache`
/// (the repo-local challenge_cache convention).
fn default_cache_dir() -> PathBuf {
    std::env::var_os("DECKLIST_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("decklist_cache"))
}

/// A recognized deck-hosting site and the deck id pulled out of its URL.
enum DeckSource {
    Moxfield(String),
    MtgGoldfish(String),
}

impl DeckSource {
    fn detect(url: &str) -> Option<DeckSource> {
        let host = host_of(url)?;
        if host.contains("moxfield.com") {
            id_after(url, "decks").map(DeckSource::Moxfield)
        } else if host.contains("mtggoldfish.com") {
            // `/deck/<id>` and `/deck/download/<id>` both supported.
            id_after(url, "deck")
                .filter(|s| s != "download")
                .or_else(|| id_after(url, "download"))
                .map(DeckSource::MtgGoldfish)
        } else {
            None
        }
    }

    /// Cache key for the raw upstream payload.
    fn cache_key(&self) -> String {
        match self {
            DeckSource::Moxfield(id) => format!("moxfield-{id}.json"),
            DeckSource::MtgGoldfish(id) => format!("mtggoldfish-{id}.txt"),
        }
    }

    fn resolve(&self, cache_dir: &Path) -> Result<Decklist, FetchError> {
        let body = cached(cache_dir, &self.cache_key(), || self.download())?;
        match self {
            DeckSource::Moxfield(_) => parse_moxfield_json(&body),
            DeckSource::MtgGoldfish(_) => Ok(Decklist::parse_text(&body)),
        }
    }

    /// Fetch the raw payload from the upstream site.
    fn download(&self) -> Result<String, FetchError> {
        let url = match self {
            // Moxfield's public v2 API returns the deck as JSON. (Cloudflare may
            // block automated access; the cache lets you drop in a manually
            // fetched response.)
            DeckSource::Moxfield(id) => format!("https://api.moxfield.com/v2/decks/all/{id}"),
            // MTGGoldfish's download endpoint returns a plain `[qty] [name]` list.
            DeckSource::MtgGoldfish(id) => {
                format!("https://www.mtggoldfish.com/deck/download/{id}")
            }
        };
        http_get(&url)
    }
}

/// GET `url` as text with a browser User-Agent, mapping failures to [`FetchError::Http`].
fn http_get(url: &str) -> Result<String, FetchError> {
    ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| FetchError::Http(e.to_string()))?
        .into_string()
        .map_err(|e| FetchError::Http(e.to_string()))
}

/// Return the cached body for `key`, or fetch (via `f`), store, and return it.
fn cached(
    cache_dir: &Path,
    key: &str,
    f: impl FnOnce() -> Result<String, FetchError>,
) -> Result<String, FetchError> {
    let path = cache_dir.join(key);
    if let Ok(body) = std::fs::read_to_string(&path) {
        return Ok(body);
    }
    let body = f()?;
    std::fs::create_dir_all(cache_dir)?;
    std::fs::write(&path, &body)?;
    Ok(body)
}

/// Parse Moxfield's v2 deck JSON into a [`Decklist`]. `mainboard`/`sideboard`
/// are objects keyed by card name; the card's canonical name comes from each
/// entry's `card.name`. Entries are sorted by name for a stable result.
fn parse_moxfield_json(body: &str) -> Result<Decklist, FetchError> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))?;

    let board = |field: &str| -> Vec<crate::DeckEntry> {
        let mut out: Vec<crate::DeckEntry> = v
            .get(field)
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|map| map.iter())
            .filter_map(|(key, entry)| {
                let qty = entry.get("quantity").and_then(Value::as_u64)? as u32;
                let name = entry
                    .get("card")
                    .and_then(|c| c.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(key)
                    .to_string();
                Some(crate::DeckEntry { name, qty })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    };

    let deck = Decklist {
        main: board("mainboard"),
        side: board("sideboard"),
    };
    if deck.is_empty() {
        return Err(FetchError::Parse(
            "no mainboard/sideboard cards in Moxfield response".into(),
        ));
    }
    Ok(deck)
}

/// Host portion of a URL (lowercased), without scheme or path.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// The path segment immediately following `marker`, ignoring the query/fragment.
/// e.g. `id_after("…/decks/abc123?x=1", "decks") == Some("abc123")`.
fn id_after(url: &str, marker: &str) -> Option<String> {
    let path = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['?', '#'])
        .next()?;
    let mut segs = path.split('/').filter(|s| !s.is_empty());
    while let Some(seg) = segs.next() {
        if seg.eq_ignore_ascii_case(marker) {
            return segs.next().map(|s| s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_moxfield_id() {
        match DeckSource::detect("https://www.moxfield.com/decks/AbCd123/some-slug") {
            Some(DeckSource::Moxfield(id)) => assert_eq!(id, "AbCd123"),
            _ => panic!("expected moxfield"),
        }
    }

    #[test]
    fn detects_mtggoldfish_id_and_download() {
        match DeckSource::detect("https://www.mtggoldfish.com/deck/6543210#paper") {
            Some(DeckSource::MtgGoldfish(id)) => assert_eq!(id, "6543210"),
            _ => panic!("expected goldfish"),
        }
        match DeckSource::detect("https://www.mtggoldfish.com/deck/download/6543210") {
            Some(DeckSource::MtgGoldfish(id)) => assert_eq!(id, "6543210"),
            _ => panic!("expected goldfish download"),
        }
    }

    #[test]
    fn rejects_unknown_host() {
        assert!(DeckSource::detect("https://example.com/decks/abc").is_none());
    }

    #[test]
    fn parses_moxfield_json_shape() {
        let body = r#"{
            "mainboard": {
                "Doomsday": { "quantity": 4, "card": { "name": "Doomsday" } },
                "Brainstorm": { "quantity": 4, "card": { "name": "Brainstorm" } }
            },
            "sideboard": {
                "Thoughtseize": { "quantity": 2, "card": { "name": "Thoughtseize" } }
            }
        }"#;
        let dl = parse_moxfield_json(body).unwrap();
        // Sorted by name.
        assert_eq!(dl.main[0].name, "Brainstorm");
        assert_eq!(dl.main_count(), 8);
        assert_eq!(dl.side_count(), 2);
    }
}
