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
        tracing::info!(
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
