use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};

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
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(api_key)?);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let url = format!("https://jimaku.cc/api/entries/search?query={}", urlencoding::encode(query));

    let response = client
        .get(&url)
        .headers(headers)
        .send()
        .await?;

    if response.status() == 429 {
        return Err(anyhow::anyhow!("Jimaku API rate limit exceeded. Please wait a few seconds and try again."));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("Jimaku API error ({}): {}", status, body));
    }

    let entries: Vec<JimakuEntry> = response.json().await?;
    Ok(entries)
}

pub async fn jimaku_get_files(api_key: &str, entry_id: u64, episode: Option<u32>) -> anyhow::Result<Vec<JimakuFile>> {
    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(api_key)?);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let mut url = format!("https://jimaku.cc/api/entries/{}/files", entry_id);
    if let Some(ep) = episode {
        url.push_str(&format!("?episode={}", ep));
    }

    let response = client
        .get(&url)
        .headers(headers)
        .send()
        .await?;

    if response.status() == 429 {
        return Err(anyhow::anyhow!("Jimaku API rate limit exceeded. Please wait a few seconds and try again."));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("Jimaku API error ({}): {}", status, body));
    }

    let files: Vec<JimakuFile> = response.json().await?;
    Ok(files)
}

pub async fn jimaku_download_file(api_key: &str, url: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(api_key)?);

    let response = client
        .get(url)
        .headers(headers)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        return Err(anyhow::anyhow!("Failed to download subtitle file ({})", status));
    }

    let content = response.text().await?;
    Ok(content)
}

// ============================================================================
// OpenSubtitles API client (https://api.opensubtitles.com/api/v1)
// ============================================================================

#[derive(Debug, Deserialize)]
struct OpenSubSearchResponse {
    data: Vec<OpenSubSearchItem>,
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
}

#[derive(Debug, Deserialize)]
struct OpenSubFeatureDetails {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    movie_name: Option<String>,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    season_number: Option<u32>,
    #[serde(default)]
    episode_number: Option<u32>,
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

    let response = client
        .get(&url)
        .header("Api-Key", api_key)
        .header(USER_AGENT, "Saikutsu/1.0")
        .header(CONTENT_TYPE, "application/json")
        .send()
        .await?;

    if response.status() == 429 {
        return Err(anyhow::anyhow!("OpenSubtitles API rate limit exceeded. Please wait and try again."));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("OpenSubtitles API error ({}): {}", status, body));
    }

    let search_response: OpenSubSearchResponse = response.json().await?;

    let mut entries = Vec::new();
    for item in search_response.data {
        let title = item.attributes.feature_details
            .as_ref()
            .and_then(|fd| fd.title.clone().or(fd.movie_name.clone()))
            .unwrap_or_else(|| "Unknown".to_string());

        let year = item.attributes.feature_details.as_ref().and_then(|fd| fd.year);
        let season = item.attributes.season_number
            .or(item.attributes.feature_details.as_ref().and_then(|fd| fd.season_number));
        let episode = item.attributes.episode_number
            .or(item.attributes.feature_details.as_ref().and_then(|fd| fd.episode_number));

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

    Ok(entries)
}

pub async fn opensub_download(api_key: &str, file_id: u64) -> anyhow::Result<String> {
    let client = reqwest::Client::new();

    // Request download link
    let response = client
        .post("https://api.opensubtitles.com/api/v1/download")
        .header("Api-Key", api_key)
        .header(USER_AGENT, "Saikutsu/1.0")
        .header(CONTENT_TYPE, "application/json")
        .json(&serde_json::json!({ "file_id": file_id }))
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("OpenSubtitles download error ({}): {}", status, body));
    }

    let download_response: OpenSubDownloadResponse = response.json().await?;

    // Download the actual file
    let file_response = client
        .get(&download_response.link)
        .send()
        .await?;

    if !file_response.status().is_success() {
        return Err(anyhow::anyhow!("Failed to download subtitle file"));
    }

    let content = file_response.text().await?;
    Ok(content)
}
