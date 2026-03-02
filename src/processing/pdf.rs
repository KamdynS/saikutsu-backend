use std::path::Path;

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
    let re = regex::Regex::new(r"[ \t]+").unwrap();
    let text = re.replace_all(text, " ");

    let re = regex::Regex::new(r"(\w)-\s*\n\s*(\w)").unwrap();
    let text = re.replace_all(&text, "$1$2");

    text.replace("\r\n", "\n").replace('\r', "\n")
}
