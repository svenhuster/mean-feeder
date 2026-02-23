use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_FEEDS: &[&str] = &[
    "https://lobste.rs/rss",
];

const DEFAULT_NOISY_FEEDS: &[&str] = &[
    "https://hnrss.org/frontpage",
];

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
    let defaults = match env_var {
        "FEEDS_FILE" => DEFAULT_FEEDS,
        "NOISY_FEEDS_FILE" => DEFAULT_NOISY_FEEDS,
        _ => return Vec::new(),
    };
    if !defaults.is_empty() {
        eprintln!("Using {} default {env_var} feeds", defaults.len());
        return defaults.iter().map(|s| s.to_string()).collect();
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
                    if twoline.len() > 200 {
                        format!("{}...", &twoline[..200])
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
    let mut all_entries = Vec::new();

    for url in feeds {
        let entries = fetch_feed(agent, url);
        eprintln!("Fetched {} entries from {url}", entries.len());
        all_entries.extend(entries);
    }

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
