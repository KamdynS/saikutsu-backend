use std::collections::HashMap;
use std::io::BufRead;
use std::sync::OnceLock;

/// Global lemma dictionaries, loaded once at startup
static LEMMA_DICTS: OnceLock<HashMap<String, HashMap<String, String>>> = OnceLock::new();

/// Load lemmatization dictionaries for all supported European languages.
/// Each file is TSV: lemma\tword_form
/// We build a reverse map: lowercase(word_form) → lemma
pub fn load_lemma_dictionaries() -> Result<(), String> {
    let mut all_dicts: HashMap<String, HashMap<String, String>> = HashMap::new();

    for lang in &["es", "fr", "de", "it", "pt"] {
        let path = format!("data/lemmas/lemmatization-{}.txt", lang);
        let dict = load_single_dict(&path)
            .map_err(|e| format!("Failed to load {} lemma dict: {}", lang, e))?;
        tracing::info!("Loaded {} lemma entries for {}", dict.len(), lang);
        all_dicts.insert(lang.to_string(), dict);
    }

    LEMMA_DICTS
        .set(all_dicts)
        .map_err(|_| "Lemma dictionaries already loaded".to_string())
}

fn load_single_dict(path: &str) -> Result<HashMap<String, String>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open {}: {}", path, e))?;
    let reader = std::io::BufReader::new(file);
    parse_dict(reader)
}

/// Parse a lemmatization dictionary from a reader.
/// Format: lemma\tword_form per line (TSV).
/// Returns a map of lowercase(word_form) → lowercase(lemma).
fn parse_dict<R: BufRead>(reader: R) -> Result<HashMap<String, String>, String> {
    let mut dict: HashMap<String, String> = HashMap::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Read error: {}", e))?;
        let line = line.trim_start_matches('\u{FEFF}'); // strip BOM
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: lemma\tword_form
        if let Some((lemma, word_form)) = line.split_once('\t') {
            let word_form_lower = word_form.to_lowercase();
            let lemma_lower = lemma.to_lowercase();
            // Map word_form → lemma (first entry wins for ambiguous forms)
            dict.entry(word_form_lower).or_insert(lemma_lower.clone());
            // Also map lemma → itself so base forms are recognized
            dict.entry(lemma_lower).or_insert_with(|| lemma.to_lowercase());
        }
    }

    Ok(dict)
}

/// Look up the lemma for a word form in the given language.
/// Returns the lemma if found, or None.
pub fn lookup_lemma(word: &str, language: &str) -> Option<String> {
    let dicts = LEMMA_DICTS.get()?;
    let dict = dicts.get(language)?;
    dict.get(&word.to_lowercase()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_reader(data: &str) -> Cursor<Vec<u8>> {
        Cursor::new(data.as_bytes().to_vec())
    }

    #[test]
    fn test_parse_basic_entries() {
        let data = "caminar\tcaminando\ncaminar\tcaminé\ncaminar\tcamina\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        assert_eq!(dict.get("caminando"), Some(&"caminar".to_string()));
        assert_eq!(dict.get("caminé"), Some(&"caminar".to_string()));
        assert_eq!(dict.get("camina"), Some(&"caminar".to_string()));
    }

    #[test]
    fn test_base_form_maps_to_itself() {
        let data = "importante\timportantes\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        // The inflected form maps to the lemma
        assert_eq!(dict.get("importantes"), Some(&"importante".to_string()));
        // The lemma itself also maps to itself
        assert_eq!(dict.get("importante"), Some(&"importante".to_string()));
    }

    #[test]
    fn test_case_insensitive() {
        let data = "Casa\tCasas\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        assert_eq!(dict.get("casas"), Some(&"casa".to_string()));
        assert_eq!(dict.get("casa"), Some(&"casa".to_string()));
    }

    #[test]
    fn test_bom_stripped() {
        let data = "\u{FEFF}andar\tandando\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        assert_eq!(dict.get("andando"), Some(&"andar".to_string()));
    }

    #[test]
    fn test_windows_line_endings() {
        let data = "correr\tcorriendo\r\ncorrer\tcorrí\r\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        assert_eq!(dict.get("corriendo"), Some(&"correr".to_string()));
        assert_eq!(dict.get("corrí"), Some(&"correr".to_string()));
    }

    #[test]
    fn test_empty_lines_skipped() {
        let data = "\n\nhablar\thablando\n\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        assert_eq!(dict.len(), 2); // "hablando" → "hablar", "hablar" → "hablar"
        assert_eq!(dict.get("hablando"), Some(&"hablar".to_string()));
    }

    #[test]
    fn test_first_lemma_wins_for_ambiguous_forms() {
        // "influencias" could be noun plural or verb form
        let data = "influencia\tinfluencias\ninfluenciar\tinfluencias\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        // First entry wins: "influencias" → "influencia" (not "influenciar")
        assert_eq!(dict.get("influencias"), Some(&"influencia".to_string()));
    }

    #[test]
    fn test_lines_without_tab_ignored() {
        let data = "no_tab_here\nhablar\thablando\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        assert_eq!(dict.get("hablando"), Some(&"hablar".to_string()));
        assert!(dict.get("no_tab_here").is_none());
    }

    #[test]
    fn test_unknown_word_returns_none() {
        let data = "hablar\thablando\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        assert!(dict.get("xyznotaword").is_none());
    }

    #[test]
    fn test_accented_characters_preserved() {
        let data = "está\testán\nestar\testá\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        assert_eq!(dict.get("están"), Some(&"está".to_string()));
        // "está" first appears as a lemma (from line 1), maps to itself
        assert_eq!(dict.get("está"), Some(&"está".to_string()));
    }

    #[test]
    fn test_multiple_inflections_same_lemma() {
        let data = "edificio\tedificios\nedificio\tedificito\n";
        let dict = parse_dict(make_reader(data)).unwrap();

        assert_eq!(dict.get("edificios"), Some(&"edificio".to_string()));
        assert_eq!(dict.get("edificito"), Some(&"edificio".to_string()));
        assert_eq!(dict.get("edificio"), Some(&"edificio".to_string()));
    }

    // Integration tests that load the actual dictionary files
    // These require running from the backend/ directory
    #[test]
    fn test_load_spanish_dict() {
        let dict = load_single_dict("data/lemmas/lemmatization-es.txt");
        // Skip test gracefully if file not found (e.g., in CI without data)
        let dict = match dict {
            Ok(d) => d,
            Err(_) => return,
        };

        // Verify the problem words from the original bug report
        assert_eq!(dict.get("arquitectura"), Some(&"arquitectura".to_string()));
        assert_eq!(dict.get("arquitecturas"), Some(&"arquitectura".to_string()));
        assert_eq!(dict.get("importantes"), Some(&"importante".to_string()));
        assert_eq!(dict.get("importante"), Some(&"importante".to_string()));
        assert_eq!(dict.get("influencias"), Some(&"influencia".to_string()));
        assert_eq!(dict.get("edificios"), Some(&"edificio".to_string()));
    }

    #[test]
    fn test_load_french_dict() {
        let dict = match load_single_dict("data/lemmas/lemmatization-fr.txt") {
            Ok(d) => d,
            Err(_) => return,
        };

        assert_eq!(dict.get("maisons"), Some(&"maison".to_string()));
        assert_eq!(dict.get("parlons"), Some(&"parler".to_string()));
    }

    #[test]
    fn test_load_german_dict() {
        let dict = match load_single_dict("data/lemmas/lemmatization-de.txt") {
            Ok(d) => d,
            Err(_) => return,
        };

        assert_eq!(dict.get("häuser"), Some(&"haus".to_string()));
    }

    #[test]
    fn test_spanish_verb_conjugations() {
        let dict = match load_single_dict("data/lemmas/lemmatization-es.txt") {
            Ok(d) => d,
            Err(_) => return,
        };

        // Common verbs should lemmatize to infinitive
        assert_eq!(dict.get("hablando"), Some(&"hablar".to_string()));
        assert_eq!(dict.get("comiendo"), Some(&"comer".to_string()));
        assert_eq!(dict.get("vivimos"), Some(&"vivir".to_string()));
    }

    #[test]
    fn test_dict_has_reasonable_size() {
        let dict = match load_single_dict("data/lemmas/lemmatization-es.txt") {
            Ok(d) => d,
            Err(_) => return,
        };

        // Spanish dict should have hundreds of thousands of entries
        assert!(dict.len() > 100_000, "Spanish dict too small: {} entries", dict.len());
    }
}
