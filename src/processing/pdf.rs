use std::path::Path;
use std::sync::LazyLock;

static RE_WHITESPACE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[ \t]+").unwrap());
static RE_HYPHEN_BREAK: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(\w)-\s*\n\s*(\w)").unwrap());

#[derive(Debug)]
pub struct ExtractedPage {
    pub page_num: usize,
    pub text: String,
}

pub fn extract_text(pdf_path: &Path) -> Result<Vec<ExtractedPage>, Box<dyn std::error::Error>> {
    let text = pdf_extract::extract_text(pdf_path)?;

    Ok(vec![ExtractedPage {
        page_num: 1,
        text: clean_text(&text),
    }])
}

pub fn extract_text_from_bytes(
    bytes: &[u8],
) -> Result<Vec<ExtractedPage>, Box<dyn std::error::Error>> {
    let text = pdf_extract::extract_text_from_mem(bytes)?;

    Ok(vec![ExtractedPage {
        page_num: 1,
        text: clean_text(&text),
    }])
}

fn clean_text(text: &str) -> String {
    let text = RE_WHITESPACE.replace_all(text, " ");
    let text = RE_HYPHEN_BREAK.replace_all(&text, "$1$2");

    text.replace("\r\n", "\n").replace('\r', "\n")
}
