use regex::Regex;

/// Normalize a game name for search/matching
pub fn normalize_name(name: &str) -> String {
    let mut normalized = name.to_lowercase();

    // Remove common suffixes
    let suffixes = [
        " game",
        " goty",
        " edition",
        " definitive",
        " remastered",
        " deluxe",
        " complete",
        " enhanced",
        " ultimate",
    ];
    for suffix in &suffixes {
        if normalized.ends_with(suffix) {
            normalized = normalized[..normalized.len() - suffix.len()].to_string();
        }
    }

    // Remove non-alphanumeric (keep spaces)
    let re = Regex::new(r"[^a-z0-9\s]").unwrap();
    normalized = re.replace_all(&normalized, "").to_string();

    // Collapse whitespace
    let re_ws = Regex::new(r"\s+").unwrap();
    normalized = re_ws.replace_all(&normalized, " ").trim().to_string();

    normalized
}

/// Build a search query from a game name
pub fn build_search_query(name: &str) -> String {
    normalize_name(name)
}

/// Calculate similarity between two strings (simple Jaccard on words)
pub fn similarity(a: &str, b: &str) -> f64 {
    let set_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let set_b: std::collections::HashSet<&str> = b.split_whitespace().collect();

    if set_a.is_empty() && set_b.is_empty() {
        return 1.0;
    }

    let intersection = set_a.intersection(&set_b).count() as f64;
    let union = set_a.union(&set_b).count() as f64;

    if union == 0.0 {
        return 0.0;
    }

    intersection / union
}
