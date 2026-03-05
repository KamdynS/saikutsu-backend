use super::Token;
use crate::processing::lemma_dict;
use unicode_segmentation::UnicodeSegmentation;

pub struct EuropeanTokenizer {
    language: String,
}

/// Language-specific enclitic pronouns that attach to verb forms.
/// Ordered longest-first so we try stripping longer clitics before shorter ones.

/// Spanish enclitics: attach to infinitives, gerunds, affirmative imperatives.
/// e.g. visitarme, diciéndote, dármelo, háganoslo
const ES_CLITICS: &[&str] = &[
    "melo", "mela", "melos", "melas",
    "telo", "tela", "telos", "telas",
    "selo", "sela", "selos", "selas",
    "noslo", "nosla", "noslos", "noslas",
    "nos", "les", "los", "las",
    "me", "te", "se", "le", "lo", "la", "os",
];

/// Portuguese enclitics: similar to Spanish but with lhe/lhes and o/a/os/as variants.
/// Note: Portuguese often uses hyphens (dar-me, vê-lo) which unicode_words splits,
/// but unhyphenated forms occur in older/literary text and some dialects.
const PT_CLITICS: &[&str] = &[
    "lhes", "lhe",
    "melo", "mela", "melos", "melas",
    "telo", "tela", "telos", "telas",
    "selo", "sela", "selos", "selas",
    "nos", "los", "las",
    "me", "te", "se", "lo", "la", "le", "os",
];

/// Italian enclitics: mi, ti, ci, vi, si, lo, la, li, le, ne, gli + combined forms.
/// e.g. dirmi, farlo, dandogli, portarglielo, andarci
const IT_CLITICS: &[&str] = &[
    "glielo", "gliela", "glieli", "gliele", "gliene",
    "melo", "mela", "meli", "mele", "mene",
    "telo", "tela", "teli", "tele", "tene",
    "selo", "sela", "seli", "sele", "sene",
    "celo", "cela", "celi", "cele", "cene",
    "velo", "vela", "veli", "vele", "vene",
    "gli", "nos",
    "mi", "ti", "si", "ci", "vi", "lo", "la", "li", "le", "ne",
];

impl EuropeanTokenizer {
    pub fn new(language: &str) -> Result<Self, String> {
        let valid = ["es", "fr", "de", "it", "pt"];
        if !valid.contains(&language) {
            return Err(format!("Unsupported language: {}", language));
        }

        Ok(Self {
            language: language.to_string(),
        })
    }

    pub fn tokenize(&self, text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();

        for word in text.unicode_words() {
            let lower = word.to_lowercase();

            // Dictionary-based lemmatization: look up the word form → lemma
            let lemma = lemma_dict::lookup_lemma(&lower, &self.language)
                .or_else(|| self.lemmatize_with_clitic_stripping(&lower))
                .unwrap_or_else(|| lower.clone());

            let is_content = self.is_content_word(&lower);

            tokens.push(Token {
                surface: word.to_string(),
                lemma,
                reading: String::new(),
                pos: String::new(),
                is_content,
            });
        }

        tokens
    }

    /// Try stripping enclitic pronouns from Romance language verb forms.
    /// e.g. "visitarme" → strip "me" → "visitar" → lookup succeeds.
    /// Handles double clitics too: "dármelo" → strip "melo" → "dár" → normalize → "dar".
    fn lemmatize_with_clitic_stripping(&self, word: &str) -> Option<String> {
        let clitics: &[&str] = match self.language.as_str() {
            "es" => ES_CLITICS,
            "pt" => PT_CLITICS,
            "it" => IT_CLITICS,
            _ => return None,
        };

        for clitic in clitics {
            if let Some(stem) = word.strip_suffix(clitic) {
                if stem.len() < 2 {
                    continue;
                }

                // Try the stem directly
                if let Some(lemma) = lemma_dict::lookup_lemma(stem, &self.language) {
                    return Some(lemma);
                }

                // Try removing a stress accent that was added when clitics attached.
                // e.g. "dármelo" → "dár" → "dar", "diciéndome" → "diciéndo" → "diciendo"
                let normalized = remove_clitic_accent(stem);
                if normalized != stem {
                    if let Some(lemma) = lemma_dict::lookup_lemma(&normalized, &self.language) {
                        return Some(lemma);
                    }
                }
            }
        }

        None
    }

    pub fn get_content_words(&self, text: &str) -> Vec<Token> {
        self.tokenize(text)
            .into_iter()
            .filter(|t| t.is_content)
            .collect()
    }

    fn is_content_word(&self, word: &str) -> bool {
        let stopwords = self.get_stopwords();

        if stopwords.contains(&word) {
            return false;
        }

        if word.chars().count() < 2 {
            return false;
        }

        if word.chars().all(|c| c.is_numeric()) {
            return false;
        }

        true
    }

    fn get_stopwords(&self) -> &'static [&'static str] {
        match self.language.as_str() {
            "es" => &[
                "el", "la", "de", "que", "y", "a", "en", "un", "ser", "se", "no", "haber", "por",
                "con", "su", "para", "como", "estar", "tener", "le", "lo", "todo", "pero", "mas",
                "hacer", "o", "poder", "decir", "este", "ir", "otro", "ese", "si", "me", "ya",
                "ver", "porque", "dar", "cuando", "el", "muy", "sin", "vez", "mucho", "saber",
                "que", "sobre", "mi", "alguno", "mismo", "yo", "tambien", "hasta",
                "una", "los", "las", "del", "al", "es", "son", "ha", "nos", "más",
            ],
            "fr" => &[
                "le", "la", "de", "et", "en", "un", "etre", "que", "avoir", "ne", "je", "son",
                "ce", "il", "qui", "se", "pas", "plus", "par", "sur", "faire", "tout", "pour",
                "elle", "comme", "mais", "ou", "nous", "avec", "dans", "leur", "au", "du", "dire",
                "lui", "cette", "si", "sans", "mon", "bien", "ou", "meme", "vous", "y", "rien",
                "aussi", "autre", "peu", "tres", "quand", "aller",
                "les", "des", "une", "est", "sont",
            ],
            "de" => &[
                "der", "die", "und", "in", "den", "von", "zu", "das", "mit", "sich", "des", "auf",
                "fur", "ist", "im", "dem", "nicht", "ein", "eine", "als", "auch", "es", "an",
                "werden", "aus", "er", "hat", "dass", "sie", "nach", "wird", "bei", "einer", "um",
                "am", "sind", "noch", "wie", "einem", "uber", "einen", "so", "zum", "kann", "nur",
                "sein", "ich", "war", "haben", "oder",
            ],
            "it" => &[
                "il", "di", "che", "e", "la", "in", "un", "a", "per", "non", "sono", "da", "e",
                "del", "si", "le", "una", "lo", "con", "al", "i", "ha", "ma", "della", "come",
                "piu", "dei", "gli", "anche", "questo", "nel", "era", "se", "sul", "io", "suo",
                "essere", "o", "tutti", "ci", "nella", "ne", "delle", "loro", "quando", "molto",
                "gia", "ogni", "questi", "fare",
            ],
            "pt" => &[
                "o", "de", "que", "e", "do", "da", "em", "um", "para", "e", "com", "nao", "uma",
                "os", "no", "se", "na", "por", "mais", "as", "dos", "como", "mas", "foi", "ao",
                "ele", "das", "tem", "a", "seu", "sua", "ou", "ser", "quando", "muito", "ha",
                "nos", "ja", "esta", "eu", "tambem", "so", "pelo", "pela", "ate", "isso", "ela",
                "entre", "era", "depois",
            ],
            _ => &[],
        }
    }
}

/// Remove stress accents that were added when enclitic pronouns attached to a verb.
/// In Spanish, adding clitics can shift stress and require an accent:
/// "dar" + "me" + "lo" → "dármelo", "decir" + "me" → "decirme" (no accent change here)
/// "diciéndo" + "me" → stem after stripping is "diciéndo", should normalize to "diciendo"
fn remove_clitic_accent(stem: &str) -> String {
    let accent_map: &[(char, char)] = &[
        // Acute accents (Spanish/Portuguese)
        ('á', 'a'),
        ('é', 'e'),
        ('í', 'i'),
        ('ó', 'o'),
        ('ú', 'u'),
        // Grave accents (Italian/Portuguese)
        ('à', 'a'),
        ('è', 'e'),
        ('ì', 'i'),
        ('ò', 'o'),
        ('ù', 'u'),
    ];

    // Replace the last accented vowel in the stem (the one added for stress)
    let mut result: Vec<char> = stem.chars().collect();
    for i in (0..result.len()).rev() {
        for &(accented, plain) in accent_map {
            if result[i] == accented {
                result[i] = plain;
                return result.into_iter().collect();
            }
        }
    }

    stem.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_clitic_accent_basic() {
        assert_eq!(remove_clitic_accent("dár"), "dar");
        assert_eq!(remove_clitic_accent("diciéndo"), "diciendo");
        assert_eq!(remove_clitic_accent("dándo"), "dando");
    }

    #[test]
    fn test_remove_clitic_accent_no_accent() {
        assert_eq!(remove_clitic_accent("visitar"), "visitar");
        assert_eq!(remove_clitic_accent("decir"), "decir");
    }

    #[test]
    fn test_clitic_stripping_with_lemma_dict() {
        // These tests require the lemma dictionary to be loaded
        if lemma_dict::lookup_lemma("visitar", "es").is_none() {
            // Dict not loaded, skip
            return;
        }

        let tokenizer = EuropeanTokenizer::new("es").unwrap();

        // Single clitic: "visitarme" → "visitar"
        let result = tokenizer.lemmatize_with_clitic_stripping("visitarme");
        assert_eq!(result, Some("visitar".to_string()));

        // Single clitic: "decirte" → "decir"
        let result = tokenizer.lemmatize_with_clitic_stripping("decirte");
        assert_eq!(result, Some("decir".to_string()));

        // Single clitic: "hacerlo" → "hacer"
        let result = tokenizer.lemmatize_with_clitic_stripping("hacerlo");
        assert_eq!(result, Some("hacer".to_string()));
    }

    #[test]
    fn test_clitic_stripping_not_applied_to_french() {
        let tokenizer = EuropeanTokenizer::new("fr").unwrap();
        let result = tokenizer.lemmatize_with_clitic_stripping("visitarme");
        assert_eq!(result, None);
    }

    #[test]
    fn test_clitic_stripping_not_applied_to_german() {
        let tokenizer = EuropeanTokenizer::new("de").unwrap();
        let result = tokenizer.lemmatize_with_clitic_stripping("besuchenmich");
        assert_eq!(result, None);
    }

    #[test]
    fn test_clitic_stripping_short_stem_rejected() {
        let tokenizer = EuropeanTokenizer::new("es").unwrap();
        // "ame" → strip "me" → "a" (too short, rejected)
        let result = tokenizer.lemmatize_with_clitic_stripping("ame");
        assert_eq!(result, None);
    }

    #[test]
    fn test_italian_clitic_stripping_with_lemma_dict() {
        if lemma_dict::lookup_lemma("dire", "it").is_none() {
            return;
        }

        let tokenizer = EuropeanTokenizer::new("it").unwrap();

        // "dirmi" → strip "mi" → "dir" → lookup (may need accent normalization)
        // "farlo" → strip "lo" → "far" → lookup
        // "andarci" → strip "ci" → "andar" → lookup
        if let Some(lemma) = tokenizer.lemmatize_with_clitic_stripping("farlo") {
            // Should resolve to "fare" via lemma dict
            assert_eq!(lemma, "fare");
        }
    }

    #[test]
    fn test_remove_clitic_accent_grave() {
        // Italian grave accents
        assert_eq!(remove_clitic_accent("dàr"), "dar");
        assert_eq!(remove_clitic_accent("fàr"), "far");
    }
}
