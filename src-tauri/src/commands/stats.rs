//! Playtime / stats commands powering GameDetail + Stats pages.

use serde::Serialize;
use tauri::State;

use crate::services::db::Database;

const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Clone, Serialize)]
pub struct PlaytimeSummary {
    pub total_seconds: i64,
    pub sessions: u32,
    pub last_played: Option<i64>,
    pub last_2weeks_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeatmapBucket {
    /// YYYY-MM-DD in UTC.
    pub date: String,
    pub total_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopPlayedRow {
    pub game_id: String,
    pub title: String,
    pub total_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalStats {
    pub total_hours: f64,
    pub hours_this_month: f64,
    pub longest_session_seconds: i64,
    pub longest_streak_days: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionsDayBucket {
    /// YYYY-MM-DD UTC
    pub date: String,
    pub sessions_count: u32,
    pub avg_seconds: i64,
    pub total_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UntouchedGame {
    pub game_id: String,
    pub title: String,
    pub source: String,
    pub artwork_url: Option<String>,
    pub last_played: Option<i64>,
    pub total_seconds: i64,
}

#[tauri::command]
pub async fn get_playtime_summary(
    game_id: String,
    db: State<'_, Database>,
) -> Result<PlaytimeSummary, String> {
    let sessions = db
        .get_playtime_sessions(&game_id)
        .map_err(|e| e.to_string())?;

    let mut total: i64 = 0;
    let mut last_played: Option<i64> = None;
    let mut sessions_count = 0u32;
    let mut last_2weeks: i64 = 0;

    let now = chrono::Utc::now().timestamp();
    let cutoff = now - 14 * SECONDS_PER_DAY;

    for s in &sessions {
        sessions_count += 1;
        if let Some(d) = s.duration_seconds {
            total += d;
            if s.started_at >= cutoff {
                last_2weeks += d;
            }
        }
        let end = s.ended_at.unwrap_or(s.started_at);
        last_played = match last_played {
            Some(prev) if prev > end => Some(prev),
            _ => Some(end),
        };
    }

    // Fold in platform-imported playtime (Steam API `playtime_forever`,
    // GOG gameplay endpoint). It's coarse — no start/end timestamps — so
    // it only contributes to `total_seconds`, not to `last_2weeks_seconds`
    // or session count.
    let imported = db
        .total_playtime_seconds(&game_id)
        .map_err(|e| e.to_string())?
        .saturating_sub(total);
    if imported > 0 {
        total += imported;
    }

    // Cross-source aggregation: when the same game is owned on multiple
    // stores (Steam + GOG + Epic ...), fold the OTHER rows' playtime in
    // here too. Title-matched via `normalize_title` so "Cyberpunk 2077"
    // on Steam and "Cyberpunk 2077" on GOG sum into a single number on
    // the GameDetail page.
    if let Ok(Some(this)) = db.get_game_by_id(&game_id) {
        let target_key = normalize_title(&this.title);
        if !target_key.is_empty() {
            let games = db.get_all_games().map_err(|e| e.to_string())?;
            for g in games {
                let Some(other_id) = g.id.clone() else { continue };
                if other_id == game_id {
                    continue;
                }
                if normalize_title(&g.title) != target_key {
                    continue;
                }
                let other_total = db
                    .total_playtime_seconds(&other_id)
                    .map_err(|e| e.to_string())?;
                total = total.saturating_add(other_total);
            }
        }
    }

    Ok(PlaytimeSummary {
        total_seconds: total,
        sessions: sessions_count,
        last_played,
        last_2weeks_seconds: last_2weeks,
    })
}

#[tauri::command]
pub async fn get_playtime_heatmap(
    days: u32,
    db: State<'_, Database>,
) -> Result<Vec<HeatmapBucket>, String> {
    let games = db.get_all_games().map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp();
    let cutoff = now - (days as i64) * SECONDS_PER_DAY;

    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<String, i64> = BTreeMap::new();

    for g in &games {
        let Some(id) = &g.id else { continue };
        let sessions = db
            .get_playtime_sessions(id)
            .map_err(|e| e.to_string())?;
        for s in &sessions {
            if s.started_at < cutoff {
                continue;
            }
            let Some(duration) = s.duration_seconds else {
                continue;
            };
            let date = chrono::DateTime::<chrono::Utc>::from_timestamp(s.started_at, 0)
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            *buckets.entry(date).or_insert(0) += duration;
        }
    }

    Ok(buckets
        .into_iter()
        .map(|(date, total_seconds)| HeatmapBucket {
            date,
            total_seconds,
        })
        .collect())
}

/// Normalise a game title for cross-source matching. Lowercased,
/// trademark / edition / regional noise stripped, then reduced to
/// alphanumerics only. Two games reduce to the same key iff they're the
/// same base game on Steam vs GOG vs Epic — that's what makes "sum
/// playtime across sources" honest.
fn normalize_title(title: &str) -> String {
    // 1. Lowercase + drop typographic noise that comes from store listings.
    let mut s = title.to_lowercase();

    // 2. Replace common separators with spaces so suffix-and-keyword
    //    matching works whether the title uses " - " or ": " or "—".
    for sep in [':', '-', '–', '—', '_', '·'] {
        s = s.replace(sep, " ");
    }

    // 3. Strip edition / region / language qualifiers wherever they
    //    appear. Order matters: longest first so e.g. "game of the year
    //    edition" matches before just "edition".
    for noise in [
        " game of the year edition",
        " game of the year",
        " goty edition",
        " goty",
        " definitive edition",
        " definitive",
        " enhanced edition",
        " enhanced",
        " complete edition",
        " complete",
        " ultimate edition",
        " ultimate",
        " deluxe edition",
        " deluxe",
        " gold edition",
        " gold",
        " director's cut",
        " directors cut",
        " remastered edition",
        " remastered",
        " hd edition",
        " hd",
        " special edition",
        " anniversary edition",
        " anniversary",
        " legendary edition",
        " legendary",
        " redux",
        " (pc)",
        " (steam)",
        " (gog)",
    ] {
        s = s.replace(noise, " ");
    }

    // 4. Strip trademark / registered / copyright symbols that survive
    //    the case-fold (they're not ascii alphanumeric so they'd be
    //    dropped anyway, but doing it explicitly keeps the next steps
    //    cleaner).
    for symbol in ['™', '®', '©', '\u{2122}', '\u{00ae}', '\u{00a9}'] {
        s = s.replace(symbol, "");
    }

    // 5. Drop everything that isn't ascii-alphanumeric. "The Witcher 3:
    //    Wild Hunt" and "the_witcher 3 - wild hunt" collapse to the
    //    same key.
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

#[tauri::command]
pub async fn get_top_played(
    limit: u32,
    db: State<'_, Database>,
) -> Result<Vec<TopPlayedRow>, String> {
    let games = db.get_all_games().map_err(|e| e.to_string())?;

    // Group by normalized title so a game owned on both Steam and GOG
    // shows up once with both playtimes summed. We keep the game_id +
    // title of whichever row had the most playtime — that's the one the
    // user "really plays" through, useful for the click-through link.
    use std::collections::HashMap;
    struct Bucket {
        game_id: String,
        title: String,
        total_seconds: i64,
        best_single: i64,
    }
    let mut grouped: HashMap<String, Bucket> = HashMap::new();

    for g in games {
        let Some(id) = g.id.clone() else { continue };
        let total = db
            .total_playtime_seconds(&id)
            .map_err(|e| e.to_string())?;
        if total == 0 {
            continue;
        }
        let key = normalize_title(&g.title);
        if key.is_empty() {
            continue;
        }
        grouped
            .entry(key)
            .and_modify(|b| {
                b.total_seconds = b.total_seconds.saturating_add(total);
                if total > b.best_single {
                    b.best_single = total;
                    b.game_id = id.clone();
                    b.title = g.title.clone();
                }
            })
            .or_insert(Bucket {
                game_id: id,
                title: g.title,
                total_seconds: total,
                best_single: total,
            });
    }

    let mut rows: Vec<TopPlayedRow> = grouped
        .into_values()
        .map(|b| TopPlayedRow {
            game_id: b.game_id,
            title: b.title,
            total_seconds: b.total_seconds,
        })
        .collect();
    rows.sort_by(|a, b| b.total_seconds.cmp(&a.total_seconds));
    rows.truncate(limit as usize);
    Ok(rows)
}

#[tauri::command]
pub async fn get_global_stats(db: State<'_, Database>) -> Result<GlobalStats, String> {
    let games = db.get_all_games().map_err(|e| e.to_string())?;

    let now = chrono::Utc::now();
    let now_ts = now.timestamp();
    let month_start = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
        chrono::NaiveDate::from_ymd_opt(now.format("%Y").to_string().parse::<i32>().unwrap_or(1970), now.format("%m").to_string().parse::<u32>().unwrap_or(1), 1)
            .unwrap_or_default()
            .and_hms_opt(0, 0, 0)
            .unwrap_or_default(),
        chrono::Utc,
    )
    .timestamp();

    let mut total_secs: i64 = 0;
    let mut month_secs: i64 = 0;
    let mut longest: i64 = 0;

    // Build a set of all unique session dates to compute longest streak.
    use std::collections::BTreeSet;
    let mut active_days: BTreeSet<i64> = BTreeSet::new();

    for g in &games {
        let Some(id) = &g.id else { continue };
        let sessions = db
            .get_playtime_sessions(id)
            .map_err(|e| e.to_string())?;
        for s in &sessions {
            if let Some(d) = s.duration_seconds {
                total_secs += d;
                if s.started_at >= month_start {
                    month_secs += d;
                }
                if d > longest {
                    longest = d;
                }
            }
            // Day bucket (UTC) for streak tracking.
            let day = s.started_at / SECONDS_PER_DAY;
            active_days.insert(day);
        }
    }
    // Add platform-imported playtime (Steam API, Galaxy DB) to the total
    // so the global counter reflects ALL playtime, not just what
    // Tokoru's session watcher caught. Imported values are coarse
    // (no start/end timestamps), so they don't influence month-to-date /
    // longest-session / streak metrics — only `total_secs`.
    let imported_total = db
        .total_imported_playtime_seconds()
        .map_err(|e| e.to_string())?;
    total_secs += imported_total;

    let _ = now_ts;
    let mut longest_streak: u32 = 0;
    let mut current_streak: u32 = 0;
    let mut prev_day: Option<i64> = None;
    for day in &active_days {
        match prev_day {
            Some(prev) if *day == prev + 1 => {
                current_streak += 1;
            }
            _ => {
                current_streak = 1;
            }
        }
        if current_streak > longest_streak {
            longest_streak = current_streak;
        }
        prev_day = Some(*day);
    }

    Ok(GlobalStats {
        total_hours: total_secs as f64 / 3600.0,
        hours_this_month: month_secs as f64 / 3600.0,
        longest_session_seconds: longest,
        longest_streak_days: longest_streak,
    })
}

/// Daily session counts + average duration over the last `days` days.
/// Powers the "Sessions over time" line chart on the Stats page. We only
/// look at tracked sessions (Tokoru's watcher) — imported playtime is
/// coarse (no per-session timestamps) so it doesn't fit a per-day bucket.
#[tauri::command]
pub async fn get_sessions_over_time(
    days: u32,
    db: State<'_, Database>,
) -> Result<Vec<SessionsDayBucket>, String> {
    use std::collections::BTreeMap;

    let cutoff = chrono::Utc::now().timestamp() - (days as i64) * SECONDS_PER_DAY;
    let sessions = db
        .get_all_sessions_since(cutoff)
        .map_err(|e| e.to_string())?;

    // Bucket by UTC date — count sessions + sum durations per day.
    let mut by_day: BTreeMap<String, (u32, i64)> = BTreeMap::new();
    for s in &sessions {
        let duration = s.duration_seconds.unwrap_or_else(|| {
            s.ended_at.unwrap_or(s.started_at).saturating_sub(s.started_at)
        });
        if duration <= 0 {
            continue;
        }
        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(s.started_at, 0)
            .unwrap_or_else(chrono::Utc::now);
        let date = dt.format("%Y-%m-%d").to_string();
        let entry = by_day.entry(date).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += duration;
    }

    // Emit every day in the window so the chart has a steady X axis even
    // for days with zero sessions — the front draws a flat segment.
    let mut out: Vec<SessionsDayBucket> = Vec::with_capacity(days as usize);
    let today_utc = chrono::Utc::now().date_naive();
    for i in (0..days as i64).rev() {
        let d = today_utc - chrono::Duration::days(i);
        let key = d.format("%Y-%m-%d").to_string();
        let (count, total) = by_day.remove(&key).unwrap_or((0, 0));
        let avg = if count > 0 { total / count as i64 } else { 0 };
        out.push(SessionsDayBucket {
            date: key,
            sessions_count: count,
            avg_seconds: avg,
            total_seconds: total,
        });
    }
    Ok(out)
}

/// Games the user hasn't touched in a while — used by the Stats page's
/// "Haven't been touched" re-engagement strip. Returns games sorted by
/// `last_played` ascending (oldest first), filtered to games that DO have
/// some playtime recorded (otherwise we'd return the whole unplayed
/// backlog, which isn't the same intent).
#[tauri::command]
pub async fn get_untouched_games(
    limit: u32,
    db: State<'_, Database>,
) -> Result<Vec<UntouchedGame>, String> {
    let games = db.get_all_games().map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp();
    // Minimum gap to count as "untouched" — 14 days. Games played this
    // week are obviously not "haven't been touched in a while".
    let min_gap = 14 * SECONDS_PER_DAY;

    let mut rows: Vec<UntouchedGame> = Vec::new();
    for g in games {
        let Some(id) = g.id.clone() else { continue };
        let total = db.total_playtime_seconds(&id).map_err(|e| e.to_string())?;
        if total == 0 {
            continue;
        }
        // Last-played is the max of session ended_at across all sessions.
        let last_played = db.last_played_at(&id).map_err(|e| e.to_string())?;
        if let Some(lp) = last_played {
            if now - lp < min_gap {
                continue;
            }
        }
        rows.push(UntouchedGame {
            game_id: id,
            title: g.title,
            source: g.source,
            artwork_url: g.artwork_url,
            last_played,
            total_seconds: total,
        });
    }

    // Oldest-played first; rows without a last_played fall to the bottom.
    rows.sort_by(|a, b| match (a.last_played, b.last_played) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    rows.truncate(limit as usize);
    Ok(rows)
}
