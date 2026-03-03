use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use reqwest::multipart;

const MAX_CHUNK_SIZE: u64 = 24 * 1024 * 1024; // 24MB to stay under OpenAI's 25MB limit

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "mov"];
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "m4a", "wav", "ogg", "flac"];

pub fn is_video(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    VIDEO_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

pub fn is_audio(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    AUDIO_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

pub fn is_media(filename: &str) -> bool {
    is_video(filename) || is_audio(filename)
}

/// Get media duration in seconds using ffprobe
pub async fn get_duration(path: &Path) -> Result<f64> {
    let output = tokio::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .await
        .context("Failed to run ffprobe")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffprobe failed: {}", stderr);
    }

    let duration_str = String::from_utf8_lossy(&output.stdout);
    duration_str
        .trim()
        .parse::<f64>()
        .context("Failed to parse duration")
}

/// Extract audio from video file, converting to 16kHz mono mp3
pub async fn extract_audio(video_path: &Path, output_path: &Path) -> Result<()> {
    let status = tokio::process::Command::new("ffmpeg")
        .args([
            "-i",
        ])
        .arg(video_path)
        .args([
            "-vn",
            "-acodec", "libmp3lame",
            "-ar", "16000",
            "-ac", "1",
            "-q:a", "4",
            "-y",
        ])
        .arg(output_path)
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .status()
        .await
        .context("Failed to run ffmpeg")?;

    if !status.success() {
        anyhow::bail!("ffmpeg audio extraction failed");
    }

    Ok(())
}

/// Split audio file into chunks under 25MB
pub async fn split_audio(audio_path: &Path, temp_dir: &Path) -> Result<Vec<PathBuf>> {
    let file_size = tokio::fs::metadata(audio_path).await?.len();

    if file_size <= MAX_CHUNK_SIZE {
        return Ok(vec![audio_path.to_path_buf()]);
    }

    let duration = get_duration(audio_path).await?;
    let num_chunks = (file_size as f64 / MAX_CHUNK_SIZE as f64).ceil() as u64;
    let chunk_duration = duration / num_chunks as f64;

    let mut chunks = Vec::new();
    for i in 0..num_chunks {
        let start = i as f64 * chunk_duration;
        let chunk_path = temp_dir.join(format!("chunk_{:03}.mp3", i));

        let status = tokio::process::Command::new("ffmpeg")
            .args([
                "-i",
            ])
            .arg(audio_path)
            .args([
                "-ss", &format!("{:.3}", start),
                "-t", &format!("{:.3}", chunk_duration),
                "-acodec", "libmp3lame",
                "-ar", "16000",
                "-ac", "1",
                "-q:a", "4",
                "-y",
            ])
            .arg(&chunk_path)
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .status()
            .await
            .context("Failed to run ffmpeg for chunking")?;

        if !status.success() {
            anyhow::bail!("ffmpeg chunking failed for chunk {}", i);
        }

        chunks.push(chunk_path);
    }

    Ok(chunks)
}

/// Transcribe a single audio chunk using OpenAI's API
pub async fn transcribe_chunk(
    client: &reqwest::Client,
    api_key: &str,
    audio_path: &Path,
    language: Option<&str>,
) -> Result<String> {
    let file_bytes = tokio::fs::read(audio_path).await?;
    let file_name = audio_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let file_part = multipart::Part::bytes(file_bytes)
        .file_name(file_name)
        .mime_str("audio/mpeg")?;

    let mut form = multipart::Form::new()
        .text("model", "gpt-4o-mini-transcribe")
        .part("file", file_part);

    if let Some(lang) = language {
        // Map our language codes to OpenAI's expected codes
        let openai_lang = match lang {
            "ja" => "ja",
            "es" => "es",
            "fr" => "fr",
            "de" => "de",
            "it" => "it",
            "pt" => "pt",
            _ => lang,
        };
        form = form.text("language", openai_lang.to_string());
    }

    let response = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await
        .context("Failed to call OpenAI transcription API")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI API error ({}): {}", status, body);
    }

    #[derive(serde::Deserialize)]
    struct TranscriptionResponse {
        text: String,
    }

    let result: TranscriptionResponse = response.json().await?;
    Ok(result.text)
}

/// Full transcription pipeline: save → extract audio (if video) → chunk → transcribe → concatenate
pub async fn transcribe_media(
    file_bytes: &[u8],
    filename: &str,
    language: Option<&str>,
    api_key: &str,
) -> Result<(String, f64)> {
    let temp_dir = tempfile::tempdir()?;
    let input_path = temp_dir.path().join(filename);
    tokio::fs::write(&input_path, file_bytes).await?;

    // Get duration
    let duration = get_duration(&input_path).await?;

    // Extract audio if video
    let audio_path = if is_video(filename) {
        let extracted = temp_dir.path().join("extracted.mp3");
        extract_audio(&input_path, &extracted).await?;
        extracted
    } else {
        input_path.clone()
    };

    // Split into chunks if needed
    let chunks = split_audio(&audio_path, temp_dir.path()).await?;

    // Transcribe each chunk
    let client = reqwest::Client::new();
    let mut transcript_parts = Vec::new();

    for chunk_path in &chunks {
        let text = transcribe_chunk(&client, api_key, chunk_path, language).await?;
        transcript_parts.push(text);
    }

    let transcript = transcript_parts.join(" ");

    Ok((transcript, duration))
}
