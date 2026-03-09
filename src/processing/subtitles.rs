use regex::Regex;

/// Parse an SRT subtitle file into plain text.
/// Strips sequence numbers, timestamps, and HTML tags.
pub fn parse_srt(content: &str) -> String {
    let timestamp_re = Regex::new(r"^\d{2}:\d{2}:\d{2}[,.]\d{3}\s*-->").unwrap();
    let html_re = Regex::new(r"<[^>]+>").unwrap();
    let sequence_re = Regex::new(r"^\d+\s*$").unwrap();

    let mut lines = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip empty lines, sequence numbers, and timestamps
        if trimmed.is_empty() || sequence_re.is_match(trimmed) || timestamp_re.is_match(trimmed) {
            continue;
        }

        // Strip HTML tags
        let cleaned = html_re.replace_all(trimmed, "").to_string();
        let cleaned = cleaned.trim().to_string();

        if !cleaned.is_empty() {
            lines.push(cleaned);
        }
    }

    // Join dialogue lines, adding periods where sentences don't end with punctuation
    let mut result = String::new();
    for line in &lines {
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(line);
    }

    result
}

/// Parse an ASS/SSA subtitle file into plain text.
/// Extracts dialogue lines and strips style override tags.
pub fn parse_ass(content: &str) -> String {
    let style_tag_re = Regex::new(r"\{[^}]*\}").unwrap();
    let mut lines = Vec::new();
    let mut in_events = false;
    let mut text_index: Option<usize> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "[Events]" {
            in_events = true;
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_events = false;
            continue;
        }

        if !in_events {
            continue;
        }

        // Parse Format line to find the Text column index
        if trimmed.starts_with("Format:") {
            let fields: Vec<&str> = trimmed
                .trim_start_matches("Format:")
                .split(',')
                .map(|s| s.trim())
                .collect();
            text_index = fields.iter().position(|&f| f == "Text");
            continue;
        }

        // Parse Dialogue lines
        if trimmed.starts_with("Dialogue:") {
            let after_prefix = trimmed.trim_start_matches("Dialogue:");
            let idx = text_index.unwrap_or(9); // Default: Text is the 10th field (index 9)

            // Split on commas, but only up to the text field index
            // (the text field itself may contain commas)
            let parts: Vec<&str> = after_prefix.splitn(idx + 1, ',').collect();

            if let Some(text) = parts.get(idx) {
                let text = text.trim();
                // Strip ASS style override tags like {\i1}, {\b1}, {\pos(x,y)}, etc.
                let cleaned = style_tag_re.replace_all(text, "").to_string();
                // Replace \N and \n (ASS line breaks) with spaces
                let cleaned = cleaned.replace("\\N", " ").replace("\\n", " ");
                let cleaned = cleaned.trim().to_string();

                if !cleaned.is_empty() {
                    lines.push(cleaned);
                }
            }
        }
    }

    // Join dialogue lines
    let mut result = String::new();
    for line in &lines {
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(line);
    }

    result
}

/// Parse a subtitle file based on its extension.
pub fn parse_subtitle_file(filename: &str, content: &str) -> anyhow::Result<String> {
    let lower = filename.to_lowercase();
    if lower.ends_with(".srt") {
        Ok(parse_srt(content))
    } else if lower.ends_with(".ass") || lower.ends_with(".ssa") {
        Ok(parse_ass(content))
    } else {
        // Try SRT first (most common), fall back to raw text
        let result = parse_srt(content);
        if result.trim().is_empty() {
            Ok(content.to_string())
        } else {
            Ok(result)
        }
    }
}
