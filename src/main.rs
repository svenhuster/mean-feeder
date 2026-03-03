use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const DATA_FILE: &str = "entries.tsv";
const NOISY_DATA_FILE: &str = "noisy-entries.tsv";

fn utc_fetch_hour() -> u64 {
    std::env::var("UTC_FETCH_HOUR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(14)
}

fn secs_until_fetch() -> u64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let today_secs = now % 86400;
    let target = utc_fetch_hour() * 3600;
    if today_secs < target {
        target - today_secs
    } else {
        86400 - today_secs + target
    }
}

fn load_feeds(env_var: &str) -> Vec<String> {
    if let Ok(path) = std::env::var(env_var) {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let feeds: Vec<String> = contents
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if !feeds.is_empty() {
                eprintln!("Loaded {} feeds from {path}", feeds.len());
                return feeds;
            }
        }
    }
    Vec::new()
}

#[derive(Debug, Clone)]
struct Entry {
    id: String,
    title: String,
    link: String,
    published: Option<i64>,
    feed_title: String,
    summary: Option<String>,
}

struct FeedState {
    main: Vec<Entry>,
    noisy: Vec<Entry>,
}

type SharedState = Arc<RwLock<FeedState>>;

fn sanitize_field(s: &str) -> String {
    s.replace('\t', " ").replace('\n', " ")
}

fn load_entries(data_file: &str) -> Vec<Entry> {
    let contents = match std::fs::read_to_string(data_file) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    contents
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(6, '\t').collect();
            if f.len() < 6 {
                return None;
            }
            Some(Entry {
                id: f[0].to_string(),
                title: f[1].to_string(),
                link: f[2].to_string(),
                published: f[3].parse::<i64>().ok(),
                feed_title: f[4].to_string(),
                summary: if f[5].is_empty() { None } else { Some(f[5].to_string()) },
            })
        })
        .collect()
}

fn save_entries(entries: &[Entry], data_file: &str) {
    let mut out = String::new();
    for e in entries {
        out.push_str(&sanitize_field(&e.id));
        out.push('\t');
        out.push_str(&sanitize_field(&e.title));
        out.push('\t');
        out.push_str(&sanitize_field(&e.link));
        out.push('\t');
        out.push_str(&e.published.map(|t| t.to_string()).unwrap_or_default());
        out.push('\t');
        out.push_str(&sanitize_field(&e.feed_title));
        out.push('\t');
        out.push_str(&sanitize_field(e.summary.as_deref().unwrap_or("")));
        out.push('\n');
    }
    let _ = std::fs::write(data_file, out);
}

// Minimal date parser for RFC 3339 and RFC 2822 timestamps.
// Returns a unix timestamp or None.
fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    // Try RFC 3339: 2024-01-15T10:30:00Z or 2024-01-15T10:30:00+00:00
    if s.len() >= 19 && s.as_bytes()[4] == b'-' && s.as_bytes()[10] == b'T' {
        return parse_rfc3339(s);
    }
    // Try RFC 2822: Mon, 15 Jan 2024 10:30:00 +0000
    parse_rfc2822(s)
}

fn parse_rfc3339(s: &str) -> Option<i64> {
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;

    let ts = days_since_epoch(year, month, day) * 86400 + hour * 3600 + min * 60 + sec;

    // Parse timezone offset
    let rest = &s[19..];
    let offset = if rest.starts_with('Z') || rest.starts_with('z') {
        0
    } else if rest.len() >= 6 && (rest.starts_with('+') || rest.starts_with('-')) {
        let sign: i64 = if rest.starts_with('-') { -1 } else { 1 };
        let oh: i64 = rest[1..3].parse().ok()?;
        let om: i64 = rest[4..6].parse().ok()?;
        sign * (oh * 3600 + om * 60)
    } else {
        0
    };

    Some(ts - offset)
}

fn parse_rfc2822(s: &str) -> Option<i64> {
    // Skip optional day name
    let s = if let Some(pos) = s.find(',') {
        s[pos + 1..].trim()
    } else {
        s
    };

    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }

    let day: i64 = parts[0].parse().ok()?;
    let month = match parts[1].to_ascii_lowercase().as_str() {
        "jan" => 1, "feb" => 2, "mar" => 3, "apr" => 4,
        "may" => 5, "jun" => 6, "jul" => 7, "aug" => 8,
        "sep" => 9, "oct" => 10, "nov" => 11, "dec" => 12,
        _ => return None,
    };
    let year: i64 = parts[2].parse().ok()?;
    let time_parts: Vec<&str> = parts[3].split(':').collect();
    if time_parts.len() < 3 {
        return None;
    }
    let hour: i64 = time_parts[0].parse().ok()?;
    let min: i64 = time_parts[1].parse().ok()?;
    let sec: i64 = time_parts[2].parse().ok()?;

    let ts = days_since_epoch(year, month, day) * 86400 + hour * 3600 + min * 60 + sec;

    let offset = if parts.len() > 4 {
        parse_tz_offset(parts[4])
    } else {
        0
    };

    Some(ts - offset)
}

fn parse_tz_offset(s: &str) -> i64 {
    match s {
        "GMT" | "UTC" | "UT" | "Z" => 0,
        "EST" => -5 * 3600, "EDT" => -4 * 3600,
        "CST" => -6 * 3600, "CDT" => -5 * 3600,
        "MST" => -7 * 3600, "MDT" => -6 * 3600,
        "PST" => -8 * 3600, "PDT" => -7 * 3600,
        _ => {
            if s.len() >= 5 && (s.starts_with('+') || s.starts_with('-')) {
                let sign: i64 = if s.starts_with('-') { -1 } else { 1 };
                let h: i64 = s[1..3].parse().unwrap_or(0);
                let m: i64 = s[3..5].parse().unwrap_or(0);
                sign * (h * 3600 + m * 60)
            } else {
                0
            }
        }
    }
}

fn days_since_epoch(year: i64, month: i64, day: i64) -> i64 {
    // Compute days from 1970-01-01
    let mut y = year;
    let mut m = month;
    if m <= 2 {
        y -= 1;
        m += 9;
    } else {
        m -= 3;
    }
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

struct RawEntry {
    id: String,
    title: String,
    link: String,
    published: Option<String>,
    summary: Option<String>,
}

fn parse_feed(xml: &[u8]) -> (String, Vec<RawEntry>) {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut feed_title = String::new();
    let mut entries = Vec::new();
    let mut buf = Vec::new();

    let mut depth = 0;

    // Current entry being parsed
    let mut in_entry = false;
    let mut in_feed_title = false;
    let mut current_tag = String::new();
    let mut entry_id = String::new();
    let mut entry_title = String::new();
    let mut entry_link = String::new();
    let mut entry_published = Option::<String>::None;
    let mut entry_summary = Option::<String>::None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = local_name(e.name().as_ref());
                depth += 1;

                if !in_entry {
                    match local.as_slice() {
                        b"item" | b"entry" => {
                            in_entry = true;
                            entry_id.clear();
                            entry_title.clear();
                            entry_link.clear();
                            entry_published = None;
                            entry_summary = None;
                        }
                        b"title" if depth <= 3 => {
                            in_feed_title = true;
                            current_tag = "title".to_string();
                        }
                        _ => {}
                    }
                } else {
                    current_tag = String::from_utf8_lossy(&local).to_string();

                    if local == b"link" {
                        if let Some(href) = attr_value(e, b"href") {
                            if entry_link.is_empty() {
                                entry_link = href;
                            }
                        }
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = local_name(e.name().as_ref());
                if in_entry && local == b"link" {
                    if let Some(href) = attr_value(e, b"href") {
                        if entry_link.is_empty() {
                            entry_link = href;
                        }
                    }
                }
            }
            Ok(Event::Text(ref e)) => {
                let text = e.unescape().map(|s| s.to_string()).unwrap_or_default();
                if in_feed_title && !in_entry {
                    feed_title = text;
                    in_feed_title = false;
                } else if in_entry {
                    match current_tag.as_str() {
                        "title" => entry_title = text,
                        "link" => {
                            if entry_link.is_empty() {
                                entry_link = text;
                            }
                        }
                        "id" | "guid" => entry_id = text,
                        "published" | "pubDate" | "updated" | "date" => {
                            if entry_published.is_none() {
                                entry_published = Some(text);
                            }
                        }
                        "summary" | "description" | "content" | "encoded" => {
                            if entry_summary.is_none() {
                                entry_summary = Some(text);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::CData(ref e)) => {
                let text = String::from_utf8_lossy(e.as_ref()).to_string();
                if in_entry {
                    match current_tag.as_str() {
                        "summary" | "description" | "content" | "encoded" => {
                            if entry_summary.is_none() {
                                entry_summary = Some(text);
                            }
                        }
                        "title" => entry_title = text,
                        _ => {}
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name(e.name().as_ref());
                depth -= 1;

                if local.as_slice() == b"title" {
                    in_feed_title = false;
                }

                if in_entry && (local.as_slice() == b"item" || local.as_slice() == b"entry") {
                    in_entry = false;
                    entries.push(RawEntry {
                        id: entry_id.clone(),
                        title: entry_title.clone(),
                        link: entry_link.clone(),
                        published: entry_published.clone(),
                        summary: entry_summary.clone(),
                    });
                }

                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                eprintln!("XML parse error: {e}");
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    (feed_title, entries)
}

fn local_name(name: &[u8]) -> Vec<u8> {
    match name.iter().position(|&b| b == b':') {
        Some(pos) => name[pos + 1..].to_vec(),
        None => name.to_vec(),
    }
}

fn attr_value(e: &quick_xml::events::BytesStart, attr_name: &[u8]) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == attr_name {
            return Some(String::from_utf8_lossy(&attr.value).to_string());
        }
    }
    None
}

fn fetch_feed(agent: &ureq::Agent, url: &str) -> Vec<Entry> {
    let mut body = match agent.get(url).call() {
        Ok(r) => r.into_body(),
        Err(e) => {
            eprintln!("Failed to fetch {url}: {e}");
            return vec![];
        }
    };

    let bytes = match body.read_to_vec() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to read body from {url}: {e}");
            return vec![];
        }
    };

    let (feed_title, raw_entries) = parse_feed(&bytes);
    let feed_title = if feed_title.is_empty() {
        url.to_string()
    } else {
        feed_title
    };

    raw_entries
        .into_iter()
        .map(|raw| {
            let entry_id = if raw.id.is_empty() {
                raw.link.clone()
            } else {
                raw.id
            };
            let id = format!("{url}#{entry_id}");
            let title = if raw.title.is_empty() {
                "(untitled)".to_string()
            } else {
                raw.title
            };
            let published = raw.published.as_deref().and_then(parse_timestamp);
            let summary = raw
                .summary
                .map(|s| {
                    let stripped = strip_html(&s).trim().to_string();
                    let twoline: String = stripped.lines().take(2).collect::<Vec<_>>().join(" ");
                    if twoline.chars().count() > 200 {
                        let truncated: String = twoline.chars().take(200).collect();
                        format!("{truncated}...")
                    } else {
                        twoline
                    }
                })
                .filter(|s| !s.is_empty() && s != "Comments");

            Entry {
                id,
                title,
                link: raw.link,
                published,
                feed_title: feed_title.clone(),
                summary,
            }
        })
        .collect()
}

fn strip_html(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    decode_entities(&result)
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
}

fn fetch_and_save(agent: &ureq::Agent, feeds: &[String], data_file: &str) -> Vec<Entry> {
    let results: Vec<Vec<Entry>> = std::thread::scope(|s| {
        let handles: Vec<_> = feeds
            .iter()
            .map(|url| {
                s.spawn(move || {
                    let entries = fetch_feed(agent, url);
                    eprintln!("Fetched {} entries from {url}", entries.len());
                    entries
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let all_entries: Vec<Entry> = results.into_iter().flatten().collect();

    let mut seen = HashMap::new();
    let mut deduped = Vec::new();
    for entry in all_entries {
        if seen.insert(entry.id.clone(), ()).is_none() {
            deduped.push(entry);
        }
    }

    deduped.sort_by(|a, b| b.published.cmp(&a.published));

    save_entries(&deduped, data_file);

    deduped
}

fn refresh_all(state: &SharedState, main_feeds: &[String], noisy_feeds: &[String]) {
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build(),
    );

    let main = fetch_and_save(&agent, main_feeds, DATA_FILE);
    let noisy = fetch_and_save(&agent, noisy_feeds, NOISY_DATA_FILE);

    let mut state = state.write().unwrap();
    *state = FeedState { main, noisy };
}

fn render_entries(html: &mut String, entries: &[Entry], now: i64, page_size: Option<usize>) {
    let chunks: Vec<&[Entry]> = match page_size {
        Some(n) => entries.chunks(n).collect(),
        None => vec![entries],
    };

    for (i, chunk) in chunks.iter().enumerate() {
        if page_size.is_some() {
            html.push_str(&format!("<div class=\"page\" data-page=\"{}\">\n", i + 1));
        }
        for entry in *chunk {
            let ago = entry
                .published
                .map(|ts| format_relative(now, ts))
                .unwrap_or_else(|| "unknown".to_string());

            html.push_str("<div class=\"entry\">\n");
            html.push_str(&format!(
                "  <div class=\"header\"><a href=\"{}\">{}</a><span class=\"meta\">{} &mdash; {}</span></div>\n",
                escape_html(&entry.link),
                escape_html(&entry.title),
                escape_html(&ago),
                escape_html(&entry.feed_title),
            ));
            html.push_str("</div>\n");
        }
        if page_size.is_some() {
            html.push_str("</div>\n");
        }
    }
}

fn render_page(main_entries: &[Entry], noisy_entries: &[Entry]) -> String {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>mean-feeder</title>
<style>
  body { max-width: 800px; margin: 0 auto; padding: 1rem; font-family: system-ui, sans-serif; background: #fafafa; color: #222; }
  .entry { margin-bottom: 0.5rem; }
  .header { display: flex; justify-content: space-between; align-items: baseline; gap: 1rem; }
  .header a { color: #1a0dab; text-decoration: none; }
  .header a:visited { color: #609; }
  .header a:hover { text-decoration: underline; }
  .meta { color: #888; font-size: 0.8rem; white-space: nowrap; text-align: right; }
  @media (max-width: 600px) {
    .header { flex-direction: column; align-items: flex-start; gap: 0; }
    .meta { text-align: left; white-space: normal; }
  }
  .summary { color: #555; font-size: 0.85rem; line-height: 1.3; margin-top: 0.15rem; }
  .empty { color: #888; font-style: italic; }
  .section-separator { border: none; border-top: 1px solid #ddd; margin: 2rem 0 1.5rem; }
  .section-heading { color: #888; font-size: 0.85rem; font-weight: normal; }
</style>
</head>
<body>
"#,
    );

    if main_entries.is_empty() && noisy_entries.is_empty() {
        html.push_str("<p class=\"empty\">No entries yet. Feeds are being fetched...</p>");
    } else {
        let now = now_secs();
        let page_size = std::env::var("PAGE_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        html.push_str("<div id=\"main-entries\">\n");
        render_entries(&mut html, main_entries, now, Some(page_size));
        html.push_str("</div>\n");
        html.push_str("<div id=\"pager\"></div>\n");

        if !noisy_entries.is_empty() {
            html.push_str("<hr class=\"section-separator\">\n");
            html.push_str("<h2 class=\"section-heading\">Firehose</h2>\n");
            html.push_str("<div id=\"noisy-entries\">\n");
            render_entries(&mut html, noisy_entries, now, Some(page_size));
            html.push_str("</div>\n");
            html.push_str("<div id=\"noisy-pager\"></div>\n");
        }

        html.push_str(
            r##"<script>
(function(){
  function parseHash() {
    var h = {};
    location.hash.replace(/^#/,'').split('&').forEach(function(s){
      var p = s.split('='); if (p.length===2) h[p[0]] = parseInt(p[1],10)||1;
    });
    return h;
  }
  function setHash(h) {
    var parts = [];
    for (var k in h) parts.push(k+'='+h[k]);
    location.hash = parts.join('&');
  }
  function paginate(containerId, pagerId, hashKey) {
    var container = document.getElementById(containerId);
    if (!container) return;
    var pages = container.querySelectorAll('.page');
    if (!pages.length) return;
    var total = pages.length;
    function show(p) {
      p = Math.max(1, Math.min(p, total));
      for (var i = 0; i < pages.length; i++)
        pages[i].style.display = (i === p - 1) ? '' : 'none';
      var h = parseHash(); h[hashKey] = p; setHash(h);
      var pager = document.getElementById(pagerId);
      pager.innerHTML = '';
      if (p > 1) {
        var prev = document.createElement('a');
        prev.href = '#'; prev.textContent = '\u2190 Prev';
        prev.onclick = function(e){ e.preventDefault(); show(p - 1); };
        pager.appendChild(prev);
      }
      if (total > 1) {
        var span = document.createElement('span');
        span.textContent = ' Page ' + p + ' of ' + total + ' ';
        pager.appendChild(span);
      }
      if (p < total) {
        var next = document.createElement('a');
        next.href = '#'; next.textContent = 'Next \u2192';
        next.onclick = function(e){ e.preventDefault(); show(p + 1); };
        pager.appendChild(next);
      }
    }
    show(parseHash()[hashKey] || 1);
    window.addEventListener('hashchange', function(){ show(parseHash()[hashKey] || 1); });
  }
  paginate('main-entries','pager','page');
  paginate('noisy-entries','noisy-pager','noisy');
})();
</script>
"##,
        );
    }

    html.push_str("</body>\n</html>");
    html
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn format_relative(now: i64, ts: i64) -> String {
    let secs = (now - ts).max(0);
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;

    if mins < 1 {
        "just now".to_string()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if hours < 24 {
        format!("{hours}h ago")
    } else {
        format!("{days}d ago")
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Handle incoming connection. We deliberetly do not parse the request as this
/// is a local-first personal project.
fn handle_connection(mut stream: std::net::TcpStream, state: &SharedState) {
    let mut buf = [0u8; 1024];
    let _ = stream.read(&mut buf);
    let feed_state = state.read().unwrap();
    let body = render_page(&feed_state.main, &feed_state.noisy);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn main() {
    let main_feeds = load_feeds("FEEDS_FILE");
    let noisy_feeds = load_feeds("NOISY_FEEDS_FILE");
    let main_entries = load_entries(DATA_FILE);
    let noisy_entries = load_entries(NOISY_DATA_FILE);
    eprintln!(
        "Loaded {} main + {} noisy existing entries",
        main_entries.len(),
        noisy_entries.len()
    );
    let state: SharedState = Arc::new(RwLock::new(FeedState {
        main: main_entries,
        noisy: noisy_entries,
    }));

    // Background fetcher thread
    let bg_state = state.clone();
    std::thread::spawn(move || {
        refresh_all(&bg_state, &main_feeds, &noisy_feeds);
        loop {
            let wait = secs_until_fetch();
            eprintln!("Next fetch in {wait}s (at {:02}:00 UTC)", utc_fetch_hour());
            std::thread::sleep(std::time::Duration::from_secs(wait));
            eprintln!("Refreshing feeds...");
            refresh_all(&bg_state, &main_feeds, &noisy_feeds);
        }
    });

    // HTTP server on main thread
    let port = std::env::var("PORT").unwrap_or_else(|_| "3102".to_string());
    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr).unwrap();
    eprintln!("Listening on http://localhost:{port}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_connection(stream, &state),
            Err(e) => eprintln!("Connection error: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- days_since_epoch ---

    #[test]
    fn days_since_epoch_at_epoch() {
        assert_eq!(days_since_epoch(1970, 1, 1), 0);
    }

    #[test]
    fn days_since_epoch_leap_year() {
        // 2000-02-29: 31 (Jan) + 29 (Feb 1-29) = 60 days into 2000
        // From 1970 to 2000 = 10957 days, plus 59 more
        assert_eq!(days_since_epoch(2000, 2, 29), 11016);
    }

    #[test]
    fn days_since_epoch_known_date() {
        // 2024-03-01
        assert_eq!(days_since_epoch(2024, 3, 1), 19783);
    }

    // --- parse_tz_offset ---

    #[test]
    fn tz_offset_named_zones() {
        assert_eq!(parse_tz_offset("GMT"), 0);
        assert_eq!(parse_tz_offset("UTC"), 0);
        assert_eq!(parse_tz_offset("UT"), 0);
        assert_eq!(parse_tz_offset("Z"), 0);
        assert_eq!(parse_tz_offset("EST"), -5 * 3600);
        assert_eq!(parse_tz_offset("EDT"), -4 * 3600);
        assert_eq!(parse_tz_offset("CST"), -6 * 3600);
        assert_eq!(parse_tz_offset("CDT"), -5 * 3600);
        assert_eq!(parse_tz_offset("MST"), -7 * 3600);
        assert_eq!(parse_tz_offset("MDT"), -6 * 3600);
        assert_eq!(parse_tz_offset("PST"), -8 * 3600);
        assert_eq!(parse_tz_offset("PDT"), -7 * 3600);
    }

    #[test]
    fn tz_offset_numeric() {
        assert_eq!(parse_tz_offset("+0000"), 0);
        assert_eq!(parse_tz_offset("+0530"), 5 * 3600 + 30 * 60);
        assert_eq!(parse_tz_offset("-0500"), -(5 * 3600));
    }

    #[test]
    fn tz_offset_unknown_defaults_to_zero() {
        assert_eq!(parse_tz_offset("XYZ"), 0);
        assert_eq!(parse_tz_offset(""), 0);
    }

    // --- parse_rfc3339 ---

    #[test]
    fn rfc3339_z_suffix() {
        assert_eq!(parse_rfc3339("2024-01-15T10:30:00Z"), Some(1705314600));
    }

    #[test]
    fn rfc3339_lowercase_z() {
        assert_eq!(parse_rfc3339("2024-01-15T10:30:00z"), Some(1705314600));
    }

    #[test]
    fn rfc3339_positive_offset() {
        // +05:30 means local time is 5:30 ahead of UTC, so UTC = local - offset
        assert_eq!(
            parse_rfc3339("2024-01-15T10:30:00+05:30"),
            Some(1705314600 - (5 * 3600 + 30 * 60))
        );
    }

    #[test]
    fn rfc3339_negative_offset() {
        // -05:00 means local time is 5h behind UTC, so UTC = local + 5h
        assert_eq!(
            parse_rfc3339("2024-01-15T10:30:00-05:00"),
            Some(1705314600 + 5 * 3600)
        );
    }

    // --- parse_rfc2822 ---

    #[test]
    fn rfc2822_with_day_name() {
        assert_eq!(
            parse_rfc2822("Mon, 15 Jan 2024 10:30:00 +0000"),
            Some(1705314600)
        );
    }

    #[test]
    fn rfc2822_without_day_name() {
        assert_eq!(
            parse_rfc2822("15 Jan 2024 10:30:00 +0000"),
            Some(1705314600)
        );
    }

    #[test]
    fn rfc2822_named_timezone_est() {
        // EST = -5h, so UTC = local - (-5h) = local + 5h
        assert_eq!(
            parse_rfc2822("15 Jan 2024 10:30:00 EST"),
            Some(1705314600 + 5 * 3600)
        );
    }

    #[test]
    fn rfc2822_named_timezone_pst() {
        // PST = -8h
        assert_eq!(
            parse_rfc2822("15 Jan 2024 10:30:00 PST"),
            Some(1705314600 + 8 * 3600)
        );
    }

    #[test]
    fn rfc2822_month_abbreviations() {
        // Just verify a few months parse without error
        assert!(parse_rfc2822("1 Feb 2024 00:00:00 +0000").is_some());
        assert!(parse_rfc2822("1 Jun 2024 00:00:00 +0000").is_some());
        assert!(parse_rfc2822("1 Dec 2024 00:00:00 +0000").is_some());
    }

    // --- parse_timestamp (dispatch) ---

    #[test]
    fn parse_timestamp_dispatches_rfc3339() {
        assert_eq!(parse_timestamp("2024-01-15T10:30:00Z"), Some(1705314600));
    }

    #[test]
    fn parse_timestamp_dispatches_rfc2822() {
        assert_eq!(
            parse_timestamp("Mon, 15 Jan 2024 10:30:00 +0000"),
            Some(1705314600)
        );
    }

    #[test]
    fn parse_timestamp_trims_whitespace() {
        assert_eq!(
            parse_timestamp("  2024-01-15T10:30:00Z  "),
            Some(1705314600)
        );
    }

    // --- parse_feed: RSS ---

    #[test]
    fn parse_feed_rss() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <item>
      <title>Test Item</title>
      <link>https://example.com/1</link>
      <guid>item-1</guid>
      <pubDate>Mon, 15 Jan 2024 10:30:00 +0000</pubDate>
    </item>
  </channel>
</rss>"#;
        let (title, entries) = parse_feed(xml);
        assert_eq!(title, "Test Feed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Test Item");
        assert_eq!(entries[0].link, "https://example.com/1");
        assert_eq!(entries[0].id, "item-1");
        assert_eq!(entries[0].published.as_deref(), Some("Mon, 15 Jan 2024 10:30:00 +0000"));
    }

    // --- parse_feed: Atom ---

    #[test]
    fn parse_feed_atom() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test Atom Feed</title>
  <entry>
    <title>Atom Entry</title>
    <link href="https://example.com/atom/1"/>
    <id>atom-1</id>
    <published>2024-01-15T10:30:00Z</published>
  </entry>
</feed>"#;
        let (title, entries) = parse_feed(xml);
        assert_eq!(title, "Test Atom Feed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Atom Entry");
        assert_eq!(entries[0].link, "https://example.com/atom/1");
        assert_eq!(entries[0].id, "atom-1");
        assert_eq!(entries[0].published.as_deref(), Some("2024-01-15T10:30:00Z"));
    }

    // --- parse_feed: missing fields ---

    #[test]
    fn parse_feed_missing_fields() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Sparse Feed</title>
    <item>
      <link>https://example.com/no-title</link>
    </item>
  </channel>
</rss>"#;
        let (title, entries) = parse_feed(xml);
        assert_eq!(title, "Sparse Feed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "");
        assert_eq!(entries[0].id, "");
        assert_eq!(entries[0].link, "https://example.com/no-title");
        assert!(entries[0].published.is_none());
    }

    // --- parse_feed: CDATA ---

    #[test]
    fn parse_feed_cdata() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>CDATA Feed</title>
    <item>
      <title><![CDATA[CDATA Title]]></title>
      <link>https://example.com/cdata</link>
      <guid>cdata-1</guid>
      <description><![CDATA[<p>HTML content</p>]]></description>
    </item>
  </channel>
</rss>"#;
        let (title, entries) = parse_feed(xml);
        assert_eq!(title, "CDATA Feed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "CDATA Title");
        assert_eq!(entries[0].summary.as_deref(), Some("<p>HTML content</p>"));
    }

    // --- local_name ---

    #[test]
    fn local_name_without_prefix() {
        assert_eq!(local_name(b"title"), b"title");
    }

    #[test]
    fn local_name_with_prefix() {
        assert_eq!(local_name(b"dc:creator"), b"creator");
    }

    // --- format_relative ---

    #[test]
    fn format_relative_just_now() {
        assert_eq!(format_relative(1000, 1000), "just now");
        assert_eq!(format_relative(1000, 970), "just now");
    }

    #[test]
    fn format_relative_minutes() {
        assert_eq!(format_relative(1000, 700), "5m ago");
    }

    #[test]
    fn format_relative_hours() {
        assert_eq!(format_relative(10000, 0), "2h ago");
    }

    #[test]
    fn format_relative_days() {
        assert_eq!(format_relative(100000, 0), "1d ago");
    }

    // --- strip_html ---

    #[test]
    fn strip_html_removes_tags() {
        assert_eq!(strip_html("<p>Hello <b>world</b></p>"), "Hello world");
    }

    #[test]
    fn strip_html_decodes_entities() {
        assert_eq!(strip_html("foo &amp; bar"), "foo & bar");
    }

    // --- decode_entities ---

    #[test]
    fn decode_entities_all() {
        assert_eq!(decode_entities("&amp;"), "&");
        assert_eq!(decode_entities("&lt;"), "<");
        assert_eq!(decode_entities("&gt;"), ">");
        assert_eq!(decode_entities("&quot;"), "\"");
        assert_eq!(decode_entities("&#39;"), "'");
        assert_eq!(decode_entities("&apos;"), "'");
        assert_eq!(decode_entities("&#x27;"), "'");
        assert_eq!(decode_entities("&nbsp;"), " ");
    }

    // --- escape_html ---

    #[test]
    fn escape_html_special_chars() {
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html("<tag>"), "&lt;tag&gt;");
        assert_eq!(escape_html("say \"hi\""), "say &quot;hi&quot;");
    }
}
