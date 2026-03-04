use super::Token;
use crate::processing::lemma_dict;
use unicode_segmentation::UnicodeSegmentation;

pub struct EuropeanTokenizer {
    language: String,
}

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
