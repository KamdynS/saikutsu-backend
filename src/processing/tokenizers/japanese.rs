use super::Token;
use lindera::tokenizer::Tokenizer;
use serde_json::json;

pub struct JapaneseTokenizer {
    tokenizer: Tokenizer,
}

impl JapaneseTokenizer {
    pub fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Lindera 2.x config: dictionary is a string URI, not nested object
        let config = json!({
            "segmenter": {
                "dictionary": "embedded://ipadic",
                "mode": "normal"
            }
        });

        let tokenizer = Tokenizer::from_config(&config)?;
        Ok(Self { tokenizer })
    }

    pub fn tokenize(&self, text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();

        if let Ok(mut result) = self.tokenizer.tokenize(text) {
            for (i, token) in result.iter_mut().enumerate() {
                let surface = token.surface.to_string();

                // Call details() method to populate token details from dictionary
                let details: Vec<String> = token.details()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();

                // Debug first few tokens
                if i < 5 {
                    tracing::info!("Raw token {}: surface={}, details={:?}", i, surface, details);
                }

                let pos = details.first().cloned().unwrap_or_default();
                let lemma = details
                    .get(6)
                    .cloned()
                    .unwrap_or_else(|| surface.clone());
                let reading = details
                    .get(7)
                    .map(|s| Self::katakana_to_hiragana(s))
                    .unwrap_or_default();

                let is_content = Self::is_content_word(&pos, &details);

                tokens.push(Token {
                    surface,
                    lemma,
                    reading,
                    pos,
                    is_content,
                });
            }
        }

        tokens
    }

    pub fn get_content_words(&self, text: &str) -> Vec<Token> {
        self.tokenize(text)
            .into_iter()
            .filter(|t| t.is_content)
            .collect()
    }

    fn is_content_word(pos: &str, details: &[String]) -> bool {
        let content_pos = ["名詞", "動詞", "形容詞", "副詞"];

        if !content_pos.contains(&pos) {
            return false;
        }

        if let Some(pos2) = details.get(1) {
            let exclude = ["非自立", "接尾", "数", "代名詞"];
            if exclude.contains(&pos2.as_str()) {
                return false;
            }
        }

        true
    }

    fn katakana_to_hiragana(text: &str) -> String {
        text.chars()
            .map(|c| {
                if c >= 'ァ' && c <= 'ン' {
                    char::from_u32(c as u32 - 0x60).unwrap_or(c)
                } else {
                    c
                }
            })
            .collect()
    }
}

impl Default for JapaneseTokenizer {
    fn default() -> Self {
        Self::new().expect("Failed to initialize Japanese tokenizer")
    }
}
