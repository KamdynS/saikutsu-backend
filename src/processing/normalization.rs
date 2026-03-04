use unicode_normalization::UnicodeNormalization;

/// Normalize a lemma for consistent dedup/lookup.
/// NFC normalizes Unicode (handles PDF accent encoding differences like e + combining accent vs e-with-accent).
/// Lowercases for non-Japanese languages since they are case-insensitive for vocabulary matching.
pub fn normalize_lemma(lemma: &str, language: &str) -> String {
    let nfc: String = lemma.nfc().collect();
    if language == "ja" {
        nfc
    } else {
        nfc.to_lowercase()
    }
}
