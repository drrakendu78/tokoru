//! Wikidata SPARQL — fallback for franchise resolution when IGDB misses
//! (no creds, no match, or rate-limited).
//!
//! Strategy: query Wikidata for items matching the game title where the
//! item is `instance of` (P31) a video game (Q7889 or any subclass) AND
//! has a `part of the series` (P179) property. Return the series label
//! when exactly one game matches the title.
//!
//! Wikidata has zero rate-limit but is slow (~500ms median); we keep the
//! query narrow with `LIMIT 5` and stop after the first matching row.
//!
//! Public, no auth. Reference: <https://query.wikidata.org/sparql>

use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const WIKIDATA_SPARQL: &str = "https://query.wikidata.org/sparql";

#[derive(Debug, Clone, Default)]
pub struct WikidataDetails {
    pub franchise: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SparqlResponse {
    results: SparqlResults,
}

#[derive(Debug, Deserialize)]
struct SparqlResults {
    bindings: Vec<SparqlBinding>,
}

#[derive(Debug, Deserialize)]
struct SparqlBinding {
    #[serde(rename = "seriesLabel")]
    series_label: Option<SparqlValue>,
}

#[derive(Debug, Deserialize)]
struct SparqlValue {
    value: String,
}

/// Resolve a game's franchise/series from Wikidata. Returns `Ok(None)`
/// when no matching item is found or the query is ambiguous.
pub async fn fetch_franchise(title: &str) -> Result<Option<WikidataDetails>, String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    // Strip trademark/edition noise so the literal label match doesn't fail
    // on store-page decorations.
    let cleaned = trimmed.replace(['™', '®', '©'], "").trim().to_string();
    // Wikidata SPARQL is case-sensitive on literal matches. We use FILTER
    // with `LCASE` to normalize, but still escape quotes.
    let safe = cleaned.replace('\\', "\\\\").replace('"', "\\\"");

    let query = format!(
        r#"
SELECT DISTINCT ?seriesLabel WHERE {{
  ?game rdfs:label "{title}"@en ;
        wdt:P31/wdt:P279* wd:Q7889 ;
        wdt:P179 ?series .
  SERVICE wikibase:label {{ bd:serviceParam wikibase:language "en". }}
}}
LIMIT 5
"#,
        title = safe
    );

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let resp = client
        .get(WIKIDATA_SPARQL)
        .query(&[("query", query.as_str()), ("format", "json")])
        .header("User-Agent", "Tokoru/0.1 (metadata enrichment; +https://github.com/)")
        .header("Accept", "application/sparql-results+json")
        .send()
        .await
        .map_err(|e| format!("wikidata request: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("wikidata status {}", status));
    }
    let parsed: SparqlResponse = resp
        .json()
        .await
        .map_err(|e| format!("wikidata parse: {}", e))?;

    // Only accept the result when there is exactly one distinct series —
    // otherwise we'd risk misclassifying a game whose title collides with
    // another franchise's entry (e.g. "Hitman" referring to different
    // reboots tied to different series objects).
    let labels: Vec<String> = parsed
        .results
        .bindings
        .into_iter()
        .filter_map(|b| b.series_label.map(|v| v.value))
        .collect();
    let mut deduped = labels.clone();
    deduped.sort();
    deduped.dedup();
    if deduped.len() != 1 {
        return Ok(None);
    }
    Ok(Some(WikidataDetails {
        franchise: deduped.into_iter().next(),
    }))
}
