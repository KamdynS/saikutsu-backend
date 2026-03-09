use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};

/// Truncate a string to at most `max_bytes`, snapping to a char boundary.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ============================================================================
// Jimaku rate limit helpers
// ============================================================================

/// Rate limit info extracted from Jimaku response headers.
#[derive(Debug, Clone)]
pub struct JimakuRateLimit {
    pub remaining: u32,
    pub reset_after_secs: f64,
}

/// Parse Jimaku rate limit headers from a response.
fn parse_jimaku_rate_limit(headers: &HeaderMap) -> Option<JimakuRateLimit> {
    let remaining = headers
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())?;
    let reset_after = headers
        .get("x-ratelimit-reset-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0);
    Some(JimakuRateLimit { remaining, reset_after_secs: reset_after })
}

/// If rate limit headers indicate we're low on remaining requests, sleep
/// until the bucket resets. Call this after every Jimaku API response.
pub async fn jimaku_respect_rate_limit(rate_limit: Option<&JimakuRateLimit>) {
    if let Some(rl) = rate_limit {
        tracing::debug!(remaining = rl.remaining, reset_after = rl.reset_after_secs, "jimaku: rate limit status");
        if rl.remaining <= 2 {
            let wait = rl.reset_after_secs + 0.1; // small buffer
            tracing::info!(wait_secs = wait, "jimaku: rate limit low, sleeping until reset");
            tokio::time::sleep(tokio::time::Duration::from_secs_f64(wait)).await;
        }
    }
}

/// Maximum retries when we hit a 429.
const JIMAKU_MAX_RETRIES: u32 = 3;

/// Build the standard Jimaku auth headers.
fn jimaku_auth_headers(api_key: &str) -> anyhow::Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(api_key)?);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(headers)
}

// ============================================================================
// Jimaku API client (https://jimaku.cc/api)
// ============================================================================

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct JimakuEntry {
    pub id: u64,
    pub name: String,
    pub english_name: Option<String>,
    pub anilist_id: Option<u64>,
    pub flags: Option<JimakuFlags>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct JimakuFlags {
    pub movie: Option<bool>,
    pub unverified: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct JimakuFile {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
}

pub async fn jimaku_search(api_key: &str, query: &str) -> anyhow::Result<Vec<JimakuEntry>> {
    let client = reqwest::Client::new();
    let headers = jimaku_auth_headers(api_key)?;

    let url = format!("https://jimaku.cc/api/entries/search?query={}", urlencoding::encode(query));
    tracing::info!(url = %url, "jimaku: searching");

    let response = client
        .get(&url)
        .headers(headers)
        .send()
        .await?;

    let status = response.status();
    tracing::info!(status = %status, "jimaku: search response");

    if status == 429 {
        let rl = parse_jimaku_rate_limit(response.headers());
        let wait = rl.as_ref().map(|r| r.reset_after_secs).unwrap_or(5.0);
        return Err(anyhow::anyhow!(
            "Jimaku API rate limit exceeded. Try again in {:.0} seconds.", wait
        ));
    }

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        tracing::error!(status = %status, body = %body, "jimaku: search failed");
        return Err(anyhow::anyhow!("Jimaku API error ({}): {}", status, body));
    }

    let rate_limit = parse_jimaku_rate_limit(response.headers());
    let body_text = response.text().await?;
    tracing::debug!(body_len = body_text.len(), body_preview = %truncate_str(&body_text, 500), "jimaku: search response body");

    let entries: Vec<JimakuEntry> = serde_json::from_str(&body_text)
        .map_err(|e| {
            tracing::error!(error = %e, body_preview = %truncate_str(&body_text, 200), "jimaku: failed to parse search response");
            anyhow::anyhow!("Failed to parse Jimaku search response: {}", e)
        })?;

    tracing::info!(count = entries.len(), "jimaku: search returned entries");
    for entry in &entries {
        tracing::debug!(
            id = entry.id,
            name = %entry.name,
            english_name = ?entry.english_name,
            anilist_id = ?entry.anilist_id,
            is_movie = ?entry.flags.as_ref().and_then(|f| f.movie),
            "jimaku: entry"
        );
    }

    jimaku_respect_rate_limit(rate_limit.as_ref()).await;
    Ok(entries)
}

pub async fn jimaku_get_files(api_key: &str, entry_id: u64, episode: Option<u32>) -> anyhow::Result<Vec<JimakuFile>> {
    let client = reqwest::Client::new();
    let headers = jimaku_auth_headers(api_key)?;

    let mut url = format!("https://jimaku.cc/api/entries/{}/files", entry_id);
    if let Some(ep) = episode {
        url.push_str(&format!("?episode={}", ep));
    }

    tracing::info!(url = %url, entry_id = entry_id, episode = ?episode, "jimaku: fetching files");

    let response = client
        .get(&url)
        .headers(headers)
        .send()
        .await?;

    let status = response.status();
    tracing::info!(status = %status, "jimaku: files response");

    if status == 429 {
        let rl = parse_jimaku_rate_limit(response.headers());
        let wait = rl.as_ref().map(|r| r.reset_after_secs).unwrap_or(5.0);
        return Err(anyhow::anyhow!(
            "Jimaku API rate limit exceeded. Try again in {:.0} seconds.", wait
        ));
    }

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        tracing::error!(status = %status, body = %body, "jimaku: files request failed");
        return Err(anyhow::anyhow!("Jimaku API error ({}): {}", status, body));
    }

    let rate_limit = parse_jimaku_rate_limit(response.headers());
    let body_text = response.text().await?;
    tracing::debug!(body_len = body_text.len(), body_preview = %truncate_str(&body_text, 500), "jimaku: files response body");

    let files: Vec<JimakuFile> = serde_json::from_str(&body_text)
        .map_err(|e| {
            tracing::error!(error = %e, body_preview = %truncate_str(&body_text, 200), "jimaku: failed to parse files response");
            anyhow::anyhow!("Failed to parse Jimaku files response: {}", e)
        })?;

    tracing::info!(count = files.len(), "jimaku: files returned");
    for file in &files {
        tracing::debug!(name = %file.name, size = ?file.size, "jimaku: file");
    }

    jimaku_respect_rate_limit(rate_limit.as_ref()).await;
    Ok(files)
}

/// Download a single Jimaku file with automatic retry on 429.
/// Returns the file content and the rate limit info from the successful response.
pub async fn jimaku_download_file(api_key: &str, url: &str) -> anyhow::Result<(String, Option<JimakuRateLimit>)> {
    let client = reqwest::Client::new();

    for attempt in 0..JIMAKU_MAX_RETRIES {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(api_key)?);

        tracing::info!(url = %url, attempt = attempt + 1, "jimaku: downloading file");

        let response = client
            .get(url)
            .headers(headers)
            .send()
            .await?;

        let status = response.status();

        if status == 429 {
            let rl = parse_jimaku_rate_limit(response.headers());
            let wait = rl.as_ref().map(|r| r.reset_after_secs).unwrap_or(5.0) + 0.1;
            tracing::warn!(
                url = %url,
                attempt = attempt + 1,
                wait_secs = wait,
                "jimaku: 429 rate limited, sleeping before retry"
            );
            tokio::time::sleep(tokio::time::Duration::from_secs_f64(wait)).await;
            continue;
        }

        if !status.is_success() {
            tracing::error!(status = %status, url = %url, "jimaku: file download failed");
            return Err(anyhow::anyhow!("Failed to download subtitle file ({})", status));
        }

        let rate_limit = parse_jimaku_rate_limit(response.headers());
        let content = response.text().await?;
        tracing::info!(url = %url, content_len = content.len(), "jimaku: file downloaded");
        return Ok((content, rate_limit));
    }

    Err(anyhow::anyhow!("Jimaku download failed after {} retries (rate limited)", JIMAKU_MAX_RETRIES))
}

// ============================================================================
// OpenSubtitles API client (https://api.opensubtitles.com/api/v1)
// ============================================================================

#[derive(Debug, Deserialize)]
struct OpenSubSearchResponse {
    data: Vec<OpenSubSearchItem>,
    #[serde(default)]
    total_count: Option<u64>,
    #[serde(default)]
    total_pages: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OpenSubSearchItem {
    id: String,
    attributes: OpenSubAttributes,
}

#[derive(Debug, Deserialize)]
struct OpenSubAttributes {
    #[serde(default)]
    feature_details: Option<OpenSubFeatureDetails>,
    files: Vec<OpenSubFileInfo>,
    #[serde(default)]
    season_number: Option<u32>,
    #[serde(default)]
    episode_number: Option<u32>,
    #[serde(default)]
    language: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenSubFeatureDetails {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    movie_name: Option<String>,
    #[serde(default)]
    parent_title: Option<String>,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    season_number: Option<u32>,
    #[serde(default)]
    episode_number: Option<u32>,
    #[serde(default)]
    feature_type: Option<String>,
    #[serde(default)]
    imdb_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OpenSubFileInfo {
    file_id: u64,
    file_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenSubEntry {
    pub id: String,
    pub title: String,
    pub year: Option<u32>,
    pub season_number: Option<u32>,
    pub episode_number: Option<u32>,
    pub file_id: u64,
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenSubDownloadResponse {
    link: String,
}

pub async fn opensub_search(
    api_key: &str,
    query: &str,
    language: &str,
) -> anyhow::Result<Vec<OpenSubEntry>> {
    let client = reqwest::Client::new();

    let url = format!(
        "https://api.opensubtitles.com/api/v1/subtitles?query={}&languages={}",
        urlencoding::encode(query),
        language
    );
    tracing::info!(url = %url, query = %query, language = %language, "opensub: searching");

    let response = client
        .get(&url)
        .header("Api-Key", api_key)
        .header(USER_AGENT, "Saikutsu/1.0")
        .header(CONTENT_TYPE, "application/json")
        .send()
        .await?;

    let status = response.status();
    tracing::info!(status = %status, "opensub: search response");

    if status == 429 {
        return Err(anyhow::anyhow!("OpenSubtitles API rate limit exceeded. Please wait and try again."));
    }

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        tracing::error!(status = %status, body = %body, "opensub: search failed");
        return Err(anyhow::anyhow!("OpenSubtitles API error ({}): {}", status, body));
    }

    let body_text = response.text().await?;
    tracing::debug!(body_len = body_text.len(), body_preview = %truncate_str(&body_text, 1000), "opensub: search response body");

    let search_response: OpenSubSearchResponse = serde_json::from_str(&body_text)
        .map_err(|e| {
            tracing::error!(error = %e, body_preview = %truncate_str(&body_text, 500), "opensub: failed to parse search response");
            anyhow::anyhow!("Failed to parse OpenSubtitles search response: {}", e)
        })?;

    tracing::info!(
        raw_items = search_response.data.len(),
        total_count = ?search_response.total_count,
        total_pages = ?search_response.total_pages,
        "opensub: search returned items"
    );

    let mut entries = Vec::new();
    for item in search_response.data {
        let fd = &item.attributes.feature_details;
        let title = fd
            .as_ref()
            .and_then(|fd| fd.parent_title.clone().or(fd.title.clone()).or(fd.movie_name.clone()))
            .unwrap_or_else(|| "Unknown".to_string());

        let year = fd.as_ref().and_then(|fd| fd.year);
        let season = item.attributes.season_number
            .or(fd.as_ref().and_then(|fd| fd.season_number));
        let episode = item.attributes.episode_number
            .or(fd.as_ref().and_then(|fd| fd.episode_number));

        tracing::debug!(
            item_id = %item.id,
            title = %title,
            year = ?year,
            season = ?season,
            episode = ?episode,
            feature_type = ?fd.as_ref().and_then(|f| f.feature_type.as_deref()),
            imdb_id = ?fd.as_ref().and_then(|f| f.imdb_id),
            language = ?item.attributes.language,
            files = item.attributes.files.len(),
            "opensub: search item"
        );

        for file in &item.attributes.files {
            entries.push(OpenSubEntry {
                id: item.id.clone(),
                title: title.clone(),
                year,
                season_number: season,
                episode_number: episode,
                file_id: file.file_id,
                file_name: file.file_name.clone(),
            });
        }
    }

    tracing::info!(total_entries = entries.len(), "opensub: flattened entries (after expanding files)");
    Ok(entries)
}

pub async fn opensub_download(api_key: &str, file_id: u64) -> anyhow::Result<String> {
    let client = reqwest::Client::new();

    tracing::info!(file_id = file_id, "opensub: requesting download link");

    // Request download link
    let response = client
        .post("https://api.opensubtitles.com/api/v1/download")
        .header("Api-Key", api_key)
        .header(USER_AGENT, "Saikutsu/1.0")
        .header(CONTENT_TYPE, "application/json")
        .json(&serde_json::json!({ "file_id": file_id }))
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        tracing::error!(status = %status, file_id = file_id, body = %body, "opensub: download link request failed");
        return Err(anyhow::anyhow!("OpenSubtitles download error ({}): {}", status, body));
    }

    let download_response: OpenSubDownloadResponse = response.json().await?;
    tracing::info!(file_id = file_id, link = %download_response.link, "opensub: got download link");

    // Download the actual file
    let file_response = client
        .get(&download_response.link)
        .send()
        .await?;

    let file_status = file_response.status();
    if !file_status.is_success() {
        tracing::error!(status = %file_status, file_id = file_id, "opensub: file download failed");
        return Err(anyhow::anyhow!("Failed to download subtitle file"));
    }

    let content = file_response.text().await?;
    tracing::info!(file_id = file_id, content_len = content.len(), "opensub: file downloaded");
    Ok(content)
}
