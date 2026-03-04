use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictEntry {
    pub reading: String,
    pub definitions: Vec<String>,
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
