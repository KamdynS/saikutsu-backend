use super::Token;
use lindera::tokenizer::Tokenizer;
use serde_json::json;
use std::sync::OnceLock;

/// Cached global tokenizer — Lindera+IPADIC init is expensive, only do it once
static JAPANESE_TOKENIZER: OnceLock<JapaneseTokenizer> = OnceLock::new();

pub struct JapaneseTokenizer {
    tokenizer: Tokenizer,
}

// Safety: Lindera's Tokenizer uses &self for tokenize, so sharing across threads is fine
unsafe impl Sync for JapaneseTokenizer {}

impl JapaneseTokenizer {
    pub fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let config = json!({
            "segmenter": {
                "dictionary": "embedded://ipadic",
                "mode": "normal"
            }
        });

        let tokenizer = Tokenizer::from_config(&config)?;
        Ok(Self { tokenizer })
    }

    /// Get or initialize the global cached tokenizer
    pub fn global() -> &'static JapaneseTokenizer {
        JAPANESE_TOKENIZER.get_or_init(|| {
            Self::new().expect("Failed to initialize Japanese tokenizer")
        })
    }

    pub fn tokenize(&self, text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();

        if let Ok(mut result) = self.tokenizer.tokenize(text) {
            tokens.reserve(result.len());
            for token in result.iter_mut() {
                let surface = token.surface.to_string();

                let details: Vec<String> = token.details()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();

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
                let universal_pos = Self::to_universal_pos(&pos, &details);

                tokens.push(Token {
                    surface,
                    lemma,
                    reading,
                    pos: universal_pos,
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

    /// Map IPADIC POS tags to Universal POS tags for consistent display.
    fn to_universal_pos(pos: &str, details: &[String]) -> String {
        let sub_pos = details.get(1).map(|s| s.as_str()).unwrap_or("");
        match pos {
            "名詞" => match sub_pos {
                "固有名詞" => "PROPN",
                "代名詞" => "PRON",
                "数" => "NUM",
                "接尾" => "NOUN",
                _ => "NOUN",
            },
            "動詞" => "VERB",
            "形容詞" => "ADJ",
            "副詞" => "ADV",
            "助詞" => "ADP",
            "助動詞" => "AUX",
            "接続詞" => "CCONJ",
            "感動詞" => "INTJ",
            "連体詞" => "DET",
            "記号" => "PUNCT",
            _ => "X",
        }.to_string()
    }

    fn katakana_to_hiragana(text: &str) -> String {
        text.chars()
            .map(|c| match c {
                'ヴ' => 'ゔ',       // U+30F4 → U+3094
                'ヵ' => 'か',       // U+30F5 → か
                'ヶ' => 'け',       // U+30F6 → け
                'ァ'..='ン' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
                _ => c,
            })
            .collect()
    }
}

impl Default for JapaneseTokenizer {
    fn default() -> Self {
        Self::new().expect("Failed to initialize Japanese tokenizer")
    }
}
