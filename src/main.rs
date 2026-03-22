use clap::Parser;
use scraper::{Html, Selector};
use std::collections::{HashMap, HashSet};
use std::fs;

#[derive(Parser)]
#[command(name = "nba-odds")]
#[command(about = "Scrape NBA betting odds from ESPN")]
struct Args {
    #[arg(default_value = "https://www.espn.com/nba/odds")]
    url: String,
}

#[derive(Debug)]
struct TeamOdds {
    name: String,
    moneyline: String,
}

#[derive(Debug)]
struct Game {
    away: TeamOdds,
    home: TeamOdds,
    game_url: String,
    time: String,
    away_predictor: Option<f32>,
    home_predictor: Option<f32>,
}

fn fetch_html(url: &str) -> Result<String, reqwest::Error> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .build()?;
    client.get(url).send()?.text()
}

/// ESPN team link text looks like "Denver NuggetsNuggetsDEN"
/// (full name + nickname + abbreviation all concatenated).
/// We find where the last word of the full name starts repeating and cut there.
fn clean_team_name(raw: &str) -> String {
    let raw = raw.trim();
    for split in 1..raw.len() {
        let (left, right) = raw.split_at(split);
        if let Some(last_word) = left.split_whitespace().last() {
            if right.starts_with(last_word) && left.contains(' ') {
                return left.trim().to_string();
            }
        }
    }
    raw.to_string()
}

fn parse_ml(link: Option<&(String, String)>) -> String {
    let (_, text) = match link { Some(l) => l, None => return "-".into() };
    let t = text.trim().to_string();
    if t.is_empty() { "-".into() } else { t }
}

/// Zeller-based day-of-week: 0=Sunday … 6=Saturday
fn day_of_week(year: i32, month: u32, day: u32) -> u32 {
    let t: [i64; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 { year as i64 - 1 } else { year as i64 };
    ((y + y/4 - y/100 + y/400 + t[(month-1) as usize] + day as i64).rem_euclid(7)) as u32
}

/// Subtract one calendar day.
fn prev_day(yr: i32, mo: u32, dy: u32) -> (i32, u32, u32) {
    if dy > 1 { return (yr, mo, dy - 1); }
    if mo > 1 {
        let pm = mo - 1;
        let last = match pm {
            1|3|5|7|8|10|12 => 31,
            4|6|9|11 => 30,
            2 => if (yr % 4 == 0 && yr % 100 != 0) || yr % 400 == 0 { 29 } else { 28 },
            _ => 30,
        };
        return (yr, pm, last);
    }
    (yr - 1, 12, 31)
}

/// Returns the US Eastern UTC offset: -4 (EDT) or -5 (EST)
fn et_offset(year: i32, month: u32, day: u32) -> i32 {
    if month > 3 && month < 11 { return -4; }
    if month < 3 || month > 11 { return -5; }
    let transition = if month == 3 {
        // DST starts: second Sunday of March
        let first_sun = (1u32..=7).find(|&d| day_of_week(year, 3, d) == 0).unwrap_or(7);
        first_sun + 7
    } else {
        // DST ends: first Sunday of November
        (1u32..=7).find(|&d| day_of_week(year, 11, d) == 0).unwrap_or(7)
    };
    if month == 3 && day >= transition { -4 }
    else if month == 11 && day < transition { -4 }
    else { -5 }
}

/// "2026-03-09T23:00Z" -> "3/9/2026" (ET date, not UTC)
fn short_date_from_iso(raw: &str) -> String {
    let up = raw.trim().to_uppercase();
    if let Some(t_pos) = up.find('T') {
        let time_str = up[t_pos + 1..].trim_end_matches('Z');
        let parts: Vec<&str> = up[..t_pos].split('-').collect();
        if parts.len() == 3 {
            if let (Ok(yr), Ok(mo), Ok(dy)) = (
                parts[0].parse::<i32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            ) {
                // Adjust UTC date to ET date
                let (yr, mo, dy) = if let Some(colon) = time_str.find(':') {
                    if let Ok(h_utc) = time_str[..colon].parse::<i32>() {
                        if h_utc + et_offset(yr, mo, dy) < 0 { prev_day(yr, mo, dy) }
                        else { (yr, mo, dy) }
                    } else { (yr, mo, dy) }
                } else { (yr, mo, dy) };
                return format!("{}/{}/{}", mo, dy, yr);
            }
        }
    }
    String::new()
}

/// Converts "2026-03-09T23:00Z" -> "7:00pm" in US Eastern time.
/// Falls back to reformatting "7:00 PM" -> "7:00pm" for plain time strings.
fn format_game_time(raw: &str) -> String {
    let raw = raw.trim();
    let up = raw.to_uppercase();
    if let (Some(t_pos), true) = (up.find('T'), up.ends_with('Z')) {
        let date = &up[..t_pos];
        let time = up[t_pos + 1..].trim_end_matches('Z');
        let parts: Vec<&str> = date.split('-').collect();
        if parts.len() == 3 {
            if let (Ok(yr), Ok(mo), Ok(dy), Some(colon)) = (
                parts[0].parse::<i32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
                time.find(':'),
            ) {
                if let (Ok(h_utc), Ok(min)) = (
                    time[..colon].parse::<i32>(),
                    time[colon + 1..colon + 3].parse::<u32>(),
                ) {
                    let h_et = ((h_utc + et_offset(yr, mo, dy)).rem_euclid(24)) as u32;
                    let period = if h_et < 12 { "am" } else { "pm" };
                    let h12 = match h_et % 12 { 0 => 12, h => h };
                    return format!("{}:{:02}{}", h12, min, period);
                }
            }
        }
    }
    // Plain "7:00 PM" fallback
    if let Some(idx) = raw.rfind(' ') {
        format!("{}{}", &raw[..idx], raw[idx + 1..].to_lowercase())
    } else {
        raw.to_lowercase()
    }
}

fn parse_odds(html: &str) -> Vec<Game> {
    let document = Html::parse_document(html);

    let game_link_sel = Selector::parse("a[data-game-link='true']").unwrap();
    let team_link_sel = Selector::parse("a[href*='/nba/team/']").unwrap();
    let dk_link_sel   = Selector::parse("a[href*='draftkings.com']").unwrap();

    let team_names: Vec<String> = document
        .select(&team_link_sel)
        .map(|el| clean_team_name(&el.text().collect::<String>()))
        .filter(|s| s.contains(' '))
        .collect();

    // Each game has 6 DK links: away_spread, away_total, away_ml, home_spread, home_total, home_ml
    let dk_links: Vec<(String, String)> = document
        .select(&dk_link_sel)
        .map(|el| (
            el.value().attr("href").unwrap_or("").to_string(),
            el.text().collect::<String>().trim().to_string(),
        ))
        .collect();

    let game_links: Vec<(String, String)> = document
        .select(&game_link_sel)
        .filter_map(|el| {
            let href = el.value().attr("href")?.to_string();
            let time = el.text().collect::<String>().trim().to_string();
            Some((href, time))
        })
        .collect();
    let game_urls: Vec<String> = game_links.iter().map(|(href, _)| href.clone()).collect();

    // Day-of-week headers appear in the raw HTML as "Monday, March 9" etc.
    // For each game URL, find the last such header that appears before the URL in the HTML.
    let day_names = ["Sunday","Monday","Tuesday","Wednesday","Thursday","Friday","Saturday"];
    let mut day_positions: Vec<(usize, &str)> = Vec::new();
    for day in &day_names {
        let search = format!("{},", day);
        let mut pos = 0;
        while let Some(idx) = html[pos..].find(search.as_str()) {
            day_positions.push((pos + idx, day));
            pos += idx + 1;
        }
    }
    day_positions.sort_by_key(|&(p, _)| p);

    let game_times: Vec<String> = game_links.iter().map(|(href, raw_time)| {
        let date = short_date_from_iso(raw_time);
        let time = format_game_time(raw_time);
        if date.is_empty() {
            // No ISO date available; fall back to HTML header scraping
            let game_pos = html.find(href.as_str()).unwrap_or(0);
            let day = day_positions.iter()
                .filter(|&&(p, _)| p < game_pos)
                .last()
                .map(|&(_, d)| d)
                .unwrap_or("");
            format!("{} {}", day, time).trim().to_string()
        } else {
            // Compute day name from the actual ET date — never trust HTML headers
            let parts: Vec<&str> = date.split('/').collect();
            let day = if parts.len() == 3 {
                if let (Ok(mo), Ok(dy), Ok(yr)) = (
                    parts[0].parse::<u32>(),
                    parts[1].parse::<u32>(),
                    parts[2].parse::<i32>(),
                ) { day_names[day_of_week(yr, mo, dy) as usize] } else { "" }
            } else { "" };
            format!("{} {} {}", day, date, time).trim().to_string()
        }
    }).collect();

    let game_count = game_urls.len();
    let mut games = Vec::new();

    for i in 0..game_count {
        let away_name = team_names.get(i * 2).cloned().unwrap_or_default();
        let home_name = team_names.get(i * 2 + 1).cloned().unwrap_or_default();
        if away_name.is_empty() || home_name.is_empty() { break; }

        let base = i * 6;
        games.push(Game {
            away: TeamOdds {
                name: away_name,
                moneyline: parse_ml(dk_links.get(base + 2)),
            },
            home: TeamOdds {
                name: home_name,
                moneyline: parse_ml(dk_links.get(base + 5)),
            },
            game_url: game_urls.get(i).cloned().unwrap_or_default(),
            time: game_times.get(i).cloned().unwrap_or_default(),
            away_predictor: None,
            home_predictor: None,
        });
    }

    games
}

/// Fetch the matchup predictor percentages from an individual game page.
/// Returns (away_pct, home_pct) if found.
///
/// ESPN embeds predictor data as `"gameProjection":"XX.X"` inside a JSON
/// blob in the page source.  Away team projection comes first.
fn fetch_matchup_predictor(game_url: &str) -> Option<(f32, f32)> {
    let url = if game_url.starts_with("http") {
        game_url.to_string()
    } else {
        format!("https://www.espn.com{}", game_url)
    };

    let html = fetch_html(&url).ok()?;

    // Strategy 1 – JSON blob: "gameProjection":"75.8"
    let values = extract_game_projections(&html);
    if values.len() >= 2 {
        return Some((values[0], values[1]));
    }

    // Strategy 2 – DOM text: look for decimal numbers whose pair sums to ~100
    extract_dom_predictor(&html)
}

/// Search the raw HTML for `"gameProjection":"<value>"` pairs.
fn extract_game_projections(html: &str) -> Vec<f32> {
    let marker = "\"gameProjection\":\"";
    let mut values = Vec::new();
    let mut pos = 0;
    while let Some(idx) = html[pos..].find(marker) {
        let start = pos + idx + marker.len();
        if let Some(end) = html[start..].find('"') {
            if let Ok(v) = html[start..start + end].parse::<f32>() {
                values.push(v);
            }
        }
        pos = start + 1;
        if values.len() >= 2 {
            break;
        }
    }
    values
}

/// ESPN renders the matchup predictor as:
///   <div class="matchupPredictor__teamValue ...">
///     <div>24.2<div class="matchupPredictor__suffix">%</div></div>
///     ...
///   </div>
/// Target .matchupPredictor__suffix, get its parent's text ("24.2%"), strip "%".
fn extract_dom_predictor(html: &str) -> Option<(f32, f32)> {
    let document = Html::parse_document(html);
    let suffix_sel = Selector::parse(".matchupPredictor__suffix").unwrap();

    let mut values: Vec<f32> = Vec::new();
    for el in document.select(&suffix_sel) {
        if let Some(parent) = el.parent() {
            if let Some(parent_el) = scraper::ElementRef::wrap(parent) {
                let text: String = parent_el.text().collect();
                let num_str = text.replace('%', "");
                let num_str = num_str.trim();
                if let Ok(v) = num_str.parse::<f32>() {
                    if v > 0.0 && v < 100.0 {
                        values.push(v);
                        if values.len() == 2 { break; }
                    }
                }
            }
        }
    }

    if values.len() == 2 && (values[0] + values[1] - 100.0).abs() < 1.0 {
        Some((values[0], values[1]))
    } else {
        None
    }
}

/// Profit on a winning $100 bet for a given moneyline.
/// +390 -> $390 profit; -220 -> $45.45 profit.
fn ml_win_amount(ml: &str) -> Option<f64> {
    let n: i32 = ml.trim().parse().ok()?;
    if n > 0 { Some(n as f64) }
    else if n < 0 { Some(10_000.0 / (-n as f64)) }
    else { None }
}

/// EV = (win_prob * profit_if_win) - (lose_prob * 100)
fn calc_ev(win_pct: f32, ml: &str) -> Option<f64> {
    let win_amount = ml_win_amount(ml)?;
    let win_prob  = win_pct as f64 / 100.0;
    let lose_prob = 1.0 - win_prob;
    Some((win_prob * win_amount) - (lose_prob * 100.0))
}

fn print_games(games: &[Game]) {
    if games.is_empty() {
        println!("No games found.");
        return;
    }

    let width = 68;
    println!("\n{:-<width$}", "", width = width);
    println!("{:<26} {:>10} {:>10} {:>14}", "Team", "ML", "Pred%", "EV");
    println!("{:-<width$}", "", width = width);

    for game in games {
        let away_ev = game.away_predictor.and_then(|p| calc_ev(p, &game.away.moneyline));
        let home_ev = game.home_predictor.and_then(|p| calc_ev(p, &game.home.moneyline));

        let fmt_ev_away = |ev: Option<f64>| -> String {
            match ev {
                None => "-".into(),
                Some(v) if v >= 8.1 && v <= 19.9 => format!("{:+.1} PICK", v),
                Some(v) => format!("{:+.1}", v),
            }
        };
        let fmt_ev_home = |ev: Option<f64>| -> String {
            match ev { None => "-".into(), Some(v) => format!("{:+.1}", v) }
        };

        let away_pred = game.away_predictor.map(|p| format!("{:.1}%", p)).unwrap_or_else(|| "-".into());
        let home_pred = game.home_predictor.map(|p| format!("{:.1}%", p)).unwrap_or_else(|| "-".into());

        println!("  {}", game.time);
        println!(
            "{:<26} {:>10} {:>10} {:>14}",
            truncate(&game.away.name, 26),
            game.away.moneyline, away_pred, fmt_ev_away(away_ev),
        );
        println!(
            "{:<26} {:>10} {:>10} {:>14}",
            truncate(&game.home.name, 26),
            game.home.moneyline, home_pred, fmt_ev_home(home_ev),
        );
        println!("{:-<width$}", "", width = width);
    }

    println!("\n{} game(s)\n", games.len());
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

// ── Persistence ─────────────────────────────────────────────────────────────

struct Record {
    day: String,   // e.g. "Monday"
    url: String,   // game URL, used as unique key
    block: String, // two formatted team lines
    picks: usize,  // 0, 1, or 2
}

fn game_to_record(game: &Game) -> Record {
    let away_ev = game.away_predictor.and_then(|p| calc_ev(p, &game.away.moneyline));
    let home_ev = game.home_predictor.and_then(|p| calc_ev(p, &game.home.moneyline));

    let fmt_ev_away = |ev: Option<f64>| -> String {
        match ev {
            None => "-".into(),
            Some(v) if v >= 8.1 && v <= 19.9 => format!("{:+.1} PICK", v),
            Some(v) => format!("{:+.1}", v),
        }
    };
    let fmt_ev_home = |ev: Option<f64>| -> String {
        match ev { None => "-".into(), Some(v) => format!("{:+.1}", v) }
    };

    let away_pred = game.away_predictor.map(|p| format!("{:.1}%", p)).unwrap_or_else(|| "-".into());
    let home_pred = game.home_predictor.map(|p| format!("{:.1}%", p)).unwrap_or_else(|| "-".into());

    let block = format!(
        "{:<26} {:>10} {:>10} {:>14}\n{:<26} {:>10} {:>10} {:>14}",
        truncate(&game.away.name, 26), game.away.moneyline, away_pred, fmt_ev_away(away_ev),
        truncate(&game.home.name, 26), game.home.moneyline, home_pred, fmt_ev_home(home_ev),
    );

    let picks = block.lines().filter(|l| l.contains("PICK")).count();
    // day key = "Monday 3/9/2026" (first two words when second looks like a date)
    let day = {
        let mut parts = game.time.splitn(3, ' ');
        let first = parts.next().unwrap_or("").to_string();
        let second = parts.next().unwrap_or("");
        if second.contains('/') { format!("{} {}", first, second) } else { first }
    };

    Record { day, url: game.game_url.clone(), block, picks }
}

/// Parse records out of an existing picks.txt.
/// File format:
///   Total PICKs: N
///   === Monday ===
///   [/nba/game/...]
///   Team A ...
///   Team B ...
fn load_records(path: &str) -> Vec<Record> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut records: Vec<Record> = Vec::new();
    let mut current_day = String::new();
    let mut current_url = String::new();
    let mut current_lines: Vec<String> = Vec::new();

    let flush = |day: &str, url: &str, lines: &mut Vec<String>, records: &mut Vec<Record>| {
        if !url.is_empty() && !lines.is_empty() {
            let block = lines.join("\n");
            let picks = block.lines().filter(|l| l.contains("PICK")).count();
            records.push(Record {
                day: day.to_string(),
                url: url.to_string(),
                block,
                picks,
            });
            lines.clear();
        }
    };

    for line in content.lines() {
        if line.starts_with("=== ") && line.ends_with(" ===") {
            flush(&current_day, &current_url, &mut current_lines, &mut records);
            current_url.clear();
            current_day = line[4..line.len() - 4].to_string();
        } else if line.starts_with('[') && line.ends_with(']') {
            flush(&current_day, &current_url, &mut current_lines, &mut records);
            current_url = line[1..line.len() - 1].to_string();
        } else if !current_url.is_empty() && !line.trim().is_empty() {
            current_lines.push(line.to_string());
        }
    }
    flush(&current_day, &current_url, &mut current_lines, &mut records);

    records
}

// ── Date utilities (no external crates) ─────────────────────────────────────

/// Julian Day Number for a Gregorian date.
fn ymd_to_jdn(y: i32, m: u32, d: u32) -> i64 {
    let y = y as i64; let m = m as i64; let d = d as i64;
    (1461 * (y + 4800 + (m - 14) / 12)) / 4
        + (367 * (m - 2 - 12 * ((m - 14) / 12))) / 12
        - (3 * ((y + 4900 + (m - 14) / 12) / 100)) / 4
        + d - 32075
}

/// How many days ago was the given date (positive = past, negative = future).
fn days_since(yr: i32, mo: u32, dy: u32) -> i64 {
    let now_days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64 / 86400;
    let today_jdn = 2_440_588 + now_days; // JDN of Unix epoch
    today_jdn - ymd_to_jdn(yr, mo, dy)
}

/// "Monday 3/9/2026" -> Some((3, 9, 2026))
fn parse_section_date(day_str: &str) -> Option<(u32, u32, i32)> {
    let date = day_str.split_whitespace().nth(1)?;
    let p: Vec<&str> = date.split('/').collect();
    if p.len() != 3 { return None; }
    Some((p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?))
}

// ── Game result lookup ───────────────────────────────────────────────────────

/// "/nba/game/_/gameId/401810786/..." -> "401810786"
fn extract_game_id(url: &str) -> Option<&str> {
    let after = url.split("/gameId/").nth(1)?;
    Some(after.split('/').next().unwrap_or(after))
}

/// Fetch the winning team's display name for a completed game, or None if
/// the game hasn't finished yet.
/// Uses ESPN's public summary API (no key required).
fn game_winner_name(game_id: &str) -> Option<String> {
    let url = format!(
        "https://site.api.espn.com/apis/site/v2/sports/basketball/nba/summary?event={}",
        game_id
    );
    let json = fetch_html(&url).ok()?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;

    let comp = v["header"]["competitions"].get(0)?;
    if !comp["status"]["type"]["completed"].as_bool().unwrap_or(false) {
        return None;
    }
    comp["competitors"].as_array()?
        .iter()
        .find(|c| c["winner"].as_bool() == Some(true))
        .and_then(|c| c["team"]["displayName"].as_str())
        .map(str::to_string)
}

/// Extract the team name that has a PICK in a record block.
fn pick_team_from_block(block: &str) -> Option<String> {
    block.lines()
        .find(|l| l.contains("PICK"))
        .map(|l| l[..26.min(l.len())].trim().to_string())
}

/// Extract the moneyline from the PICK line (e.g. "+390" or "-260").
fn pick_ml_from_block(block: &str) -> Option<String> {
    block.lines()
        .find(|l| l.contains("PICK"))
        .and_then(|l| {
            l.split_whitespace()
                .find(|t| !t.contains('%') && !t.contains('.') &&
                    (t.starts_with('+') || t.starts_with('-')))
                .map(str::to_string)
        })
}

fn save_results(output_dir: &str, games: &[Game]) {
    if let Err(e) = fs::create_dir_all(output_dir) {
        eprintln!("Failed to create output dir: {}", e);
        return;
    }

    let path = format!("{}/picks.txt", output_dir);

    let existing = load_records(&path);
    let seen_urls: HashSet<String> = existing.iter().map(|r| r.url.clone()).collect();

    let new_records: Vec<Record> = games.iter()
        .filter(|g| !g.game_url.is_empty() && !seen_urls.contains(&g.game_url))
        .map(game_to_record)
        .collect();

    let added = new_records.len();
    let mut all: Vec<Record> = existing;
    all.extend(new_records);

    if all.is_empty() {
        println!("No games to save.");
        return;
    }

    // Group by day
    let mut by_day: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in all.iter().enumerate() {
        by_day.entry(r.day.clone()).or_default().push(i);
    }

    // Sort days newest-first by calendar date
    let mut days: Vec<String> = by_day.keys().cloned().collect();
    days.sort_by(|a, b| {
        let jdn = |s: &str| parse_section_date(s)
            .map(|(mo, dy, yr)| ymd_to_jdn(yr, mo, dy))
            .unwrap_or(0);
        jdn(b).cmp(&jdn(a))
    });

    let total_picks: usize = all.iter().map(|r| r.picks).sum();

    // Collect owned data so we can mutably update `all` afterwards
    let bets_to_check: Vec<(String, String)> = all.iter()
        .filter(|r| r.picks > 0)
        .filter(|r| parse_section_date(&r.day)
            .map(|(mo, dy, yr)| { let age = days_since(yr, mo, dy); age >= 0 && age <= 14 })
            .unwrap_or(false))
        .map(|r| (r.url.clone(), r.block.clone()))
        .collect();

    // url -> (is_won, profit_delta)
    let mut result_map: HashMap<String, (bool, f64)> = HashMap::new();
    if !bets_to_check.is_empty() {
        println!("Checking results for {} bet(s)...", bets_to_check.len());
        for (url, block) in &bets_to_check {
            let Some(game_id) = extract_game_id(url) else { continue };
            let Some(winner) = game_winner_name(game_id) else { continue };
            let Some(pick_team) = pick_team_from_block(block) else { continue };
            let Some(ml) = pick_ml_from_block(block) else { continue };
            if winner.starts_with(&pick_team) || pick_team.starts_with(&winner) {
                let win_amt = ml_win_amount(&ml).unwrap_or(0.0);
                result_map.insert(url.clone(), (true, win_amt));
            } else {
                result_map.insert(url.clone(), (false, -100.0));
            }
        }
    }

    let mut won = 0usize;
    let mut lost = 0usize;
    let mut profit = 0.0f64;
    for (_, &(is_won, delta)) in &result_map {
        if is_won { won += 1; } else { lost += 1; }
        profit += delta;
    }

    // Stamp each PICK line with WON/LOST and per-bet profit
    for record in all.iter_mut() {
        if let Some(&(is_won, profit_delta)) = result_map.get(&record.url) {
            let tag = if is_won {
                format!("WON +${:.2}", profit_delta)
            } else {
                "LOST -$100.00".to_string()
            };
            record.block = record.block.lines().map(|line| {
                if line.contains("PICK") {
                    // Strip any existing WON/LOST tag before re-stamping
                    let base = line.find(" WON ").or_else(|| line.find(" LOST "))
                        .map(|p| &line[..p])
                        .unwrap_or(line);
                    format!("{} {}", base.trim_end(), tag)
                } else {
                    line.to_string()
                }
            }).collect::<Vec<_>>().join("\n");
        }
    }

    let profit_str = if profit >= 0.0 {
        format!("+${:.2}", profit)
    } else {
        format!("-${:.2}", profit.abs())
    };
    let mut out = format!("Total PICKs: {} | Won: {} | Lost: {} | Profit: {}\n", total_picks, won, lost, profit_str);
    for day in &days {
        out.push_str(&format!("\n=== {} ===\n", day));
        for &i in &by_day[day] {
            out.push_str(&format!("[{}]\n{}\n\n", all[i].url, all[i].block));
        }
    }

    match fs::write(&path, out.trim_end()) {
        Ok(_) => println!("Saved {} new game(s) to {} | Total PICKs: {}", added, path, total_picks),
        Err(e) => eprintln!("Failed to write {}: {}", path, e),
    }
}

fn main() {
    let args = Args::parse();
    println!("Fetching odds from: {}", args.url);

    let html = match fetch_html(&args.url) {
        Err(e) => { eprintln!("Failed to fetch: {}", e); return; }
        Ok(h) => h,
    };

    let mut games = parse_odds(&html);

    println!("Fetching matchup predictor for {} game(s)...", games.len());
    for game in &mut games {
        if !game.game_url.is_empty() {
            if let Some((away_pct, home_pct)) = fetch_matchup_predictor(&game.game_url) {
                game.away_predictor = Some(away_pct);
                game.home_predictor = Some(home_pct);
            }
        }
    }

    print_games(&games);
    save_results("output", &games);
}
