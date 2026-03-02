use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictEntry {
    pub reading: String,
    pub definitions: Vec<String>,
}

static DICTIONARY: OnceLock<HashMap<String, DictEntry>> = OnceLock::new();

/// Load the JMdict lookup dictionary (call once at startup)
pub fn load_dictionary() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if DICTIONARY.get().is_some() {
        return Ok(());
    }

    // Try multiple paths for the dictionary file
    let paths = [
        "data/dictionaries/jmdict-lookup.json",
        "/app/data/dictionaries/jmdict-lookup.json", // Docker path
    ];

    let mut dict_data = None;
    for path in paths {
        if let Ok(data) = std::fs::read_to_string(path) {
            tracing::info!("Loading JMdict from {}", path);
            dict_data = Some(data);
            break;
        }
    }

    let data = dict_data.ok_or("JMdict lookup file not found")?;
    let dict: HashMap<String, DictEntry> = serde_json::from_str(&data)?;

    tracing::info!("Loaded {} dictionary entries", dict.len());

    DICTIONARY
        .set(dict)
        .map_err(|_| "Dictionary already initialized")?;

    Ok(())
}

/// Look up a word in the dictionary
pub fn lookup(word: &str) -> Option<DictEntry> {
    DICTIONARY.get()?.get(word).cloned()
}

/// Look up multiple words and return a map
pub fn lookup_many(words: &[String]) -> HashMap<String, DictEntry> {
    let dict = match DICTIONARY.get() {
        Some(d) => d,
        None => return HashMap::new(),
    };

    words
        .iter()
        .filter_map(|w| dict.get(w).map(|e| (w.clone(), e.clone())))
        .collect()
}
