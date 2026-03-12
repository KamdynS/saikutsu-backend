use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Dictionary entry with POS-grouped definitions.
/// European languages use `pos_definitions` (POS → defs), loaded from POS-aware Wiktionary files.
/// Japanese uses `definitions` (flat list), loaded from JMdict.
/// At load time, one of these will be populated based on the JSON format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictEntry {
    pub reading: String,
    /// Flat definitions (JMdict format, or legacy Wiktionary)
    #[serde(default)]
    pub definitions: Vec<String>,
    /// POS-grouped definitions (new Wiktionary format: {"noun": [...], "verb": [...]})
    #[serde(default)]
    pub pos_definitions: HashMap<String, Vec<String>>,
}

impl DictEntry {
    /// Get definitions for a specific POS tag.
    /// Maps spaCy Universal POS tags to dictionary keys.
    pub fn definitions_for_pos(&self, pos: &str) -> Option<Vec<String>> {
        // If we have POS-grouped definitions, use them
        if !self.pos_definitions.is_empty() {
            let key = map_pos_to_dict_key(pos)?;
            return self.pos_definitions.get(key).cloned();
        }
        None
    }

    /// Get all definitions regardless of POS (flat list or all POS groups combined).
    pub fn all_definitions(&self) -> Vec<String> {
        if !self.definitions.is_empty() {
            return self.definitions.clone();
        }
        // Combine all POS-grouped definitions
        self.pos_definitions.values().flatten().cloned().collect()
    }

    /// Get definitions with POS preference: try exact POS match first, fall back to all.
    pub fn definitions_with_pos_preference(&self, pos: &str) -> Vec<String> {
        self.definitions_for_pos(pos)
            .unwrap_or_else(|| self.all_definitions())
    }
}

/// Map spaCy Universal POS tags to dictionary definition keys.
fn map_pos_to_dict_key(pos: &str) -> Option<&'static str> {
    match pos {
        "VERB" | "AUX" => Some("verb"),
        "NOUN" | "PROPN" => Some("noun"),
        "ADJ" => Some("adj"),
        "ADV" => Some("adv"),
        _ => None,
    }
}

/// Language code → (word → entry)
static DICTIONARIES: OnceLock<HashMap<String, HashMap<String, DictEntry>>> = OnceLock::new();

/// Load all dictionaries: JMdict for Japanese + Wiktionary for European languages.
/// Call once at startup.
pub fn load_dictionaries() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if DICTIONARIES.get().is_some() {
        return Ok(());
    }

    let mut all_dicts: HashMap<String, HashMap<String, DictEntry>> = HashMap::new();

    // Load JMdict for Japanese
    match load_dict_file("jmdict-lookup.json") {
        Ok(dict) => {
            tracing::info!("Loaded {} Japanese (JMdict) dictionary entries", dict.len());
            all_dicts.insert("ja".to_string(), dict);
        }
        Err(e) => {
            tracing::warn!("Failed to load JMdict: {}. Japanese definitions will be unavailable.", e);
        }
    }

    // Load Wiktionary dictionaries for European languages
    let european_langs = ["es", "fr", "de", "it", "pt"];
    for lang in european_langs {
        let filename = format!("wiktionary-{}-lookup.json", lang);
        match load_dict_file(&filename) {
            Ok(dict) => {
                tracing::info!("Loaded {} {} (Wiktionary) dictionary entries", dict.len(), lang);
                all_dicts.insert(lang.to_string(), dict);
            }
            Err(e) => {
                tracing::warn!("Failed to load {} dictionary: {}. {} definitions will be unavailable.", lang, e, lang);
            }
        }
    }

    DICTIONARIES
        .set(all_dicts)
        .map_err(|_| "Dictionaries already initialized")?;

    Ok(())
}

/// Load a single dictionary JSON file, trying multiple paths.
fn load_dict_file(filename: &str) -> Result<HashMap<String, DictEntry>, Box<dyn std::error::Error + Send + Sync>> {
    let paths = [
        format!("data/dictionaries/{}", filename),
        format!("/app/data/dictionaries/{}", filename), // Docker path
    ];

    let mut dict_data = None;
    for path in &paths {
        if let Ok(data) = std::fs::read_to_string(path) {
            tracing::info!("Loading dictionary from {}", path);
            dict_data = Some(data);
            break;
        }
    }

    let data = dict_data.ok_or_else(|| format!("{} not found", filename))?;
    let dict: HashMap<String, DictEntry> = serde_json::from_str(&data)?;
    Ok(dict)
}

/// Look up a word in the dictionary for a specific language.
/// Returns a reference to avoid cloning on every lookup.
pub fn lookup(word: &str, language: &str) -> Option<&'static DictEntry> {
    DICTIONARIES.get()?.get(language)?.get(word)
}

/// Look up multiple words and return a map, for a specific language.
pub fn lookup_many<'a>(words: &'a [String], language: &str) -> HashMap<&'a str, &'static DictEntry> {
    let lang_dict = match DICTIONARIES.get().and_then(|d| d.get(language)) {
        Some(d) => d,
        None => return HashMap::new(),
    };

    words
        .iter()
        .filter_map(|w| lang_dict.get(w.as_str()).map(|e| (w.as_str(), e)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_missing_language_returns_none() {
        // Before initialization, lookups should return None gracefully
        assert!(lookup("hello", "xx").is_none());
    }

    #[test]
    fn test_lookup_many_empty() {
        let result = lookup_many(&[], "ja");
        assert!(result.is_empty());
    }
}
