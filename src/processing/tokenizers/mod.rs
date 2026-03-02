pub mod japanese;
pub mod european;

pub use japanese::JapaneseTokenizer;
pub use european::EuropeanTokenizer;

#[derive(Debug, Clone)]
pub struct Token {
    pub surface: String,
    pub lemma: String,
    pub reading: String,
    pub pos: String,
    pub is_content: bool,
}
