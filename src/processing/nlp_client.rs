use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct TokenizeRequest {
    pub text: String,
    pub language: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenInfo {
    pub surface: String,
    pub lemma: String,
    pub pos: String,
    pub is_stop: bool,
}

#[derive(Debug, Deserialize)]
pub struct TokenizeResponse {
    pub tokens: Vec<TokenInfo>,
    pub sentences: Vec<String>,
}

/// Call the spaCy NLP service to tokenize text.
pub async fn tokenize(
    client: &reqwest::Client,
    base_url: &str,
    text: &str,
    language: &str,
) -> Result<TokenizeResponse, reqwest::Error> {
    let url = format!("{}/tokenize", base_url);
    let req = TokenizeRequest {
        text: text.to_string(),
        language: language.to_string(),
    };

    client
        .post(&url)
        .json(&req)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
}
