use unicode_normalization::UnicodeNormalization;

/// Normalize a lemma for consistent dedup/lookup.
/// NFC normalizes Unicode (handles PDF accent encoding differences like e + combining accent vs e-with-accent).
/// Lowercases for non-Japanese languages since they are case-insensitive for vocabulary matching.
pub fn normalize_lemma(lemma: &str, language: &str) -> String {
    let nfc: String = lemma.nfc().collect();
    let result = if language == "ja" {
        nfc.clone()
    } else {
        nfc.to_lowercase()
    };
    // Log when normalization actually changes the input (accent/case differences)
    if result != lemma {
        tracing::debug!(
            original = lemma,
            normalized = result,
            nfc_changed = nfc != lemma,
            case_changed = nfc != result,
            language = language,
            "normalization: lemma was transformed"
        );
    }
    result
}

/// Strip all bracketed content from subtitle text: （...）, (...), [...], 【...】
/// Used to remove speaker tags, sound effect labels, etc. before tokenization/display.
pub fn strip_brackets(text: &str) -> String {
    let pairs: &[(char, char)] = &[
        ('（', '）'),
        ('(', ')'),
        ('[', ']'),
        ('【', '】'),
    ];
    let mut result = text.to_string();
    for &(open, close) in pairs {
        while let Some(start) = result.find(open) {
            if let Some(end_offset) = result[start..].find(close) {
                let end = start + end_offset + close.len_utf8();
                result = format!("{}{}", &result[..start], result[end..].trim_start());
            } else {
                break;
            }
        }
    }
    result.trim().to_string()
}
