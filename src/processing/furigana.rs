use regex::Regex;
use std::sync::OnceLock;

/// Returns true if the character is a CJK kanji.
fn is_kanji(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// Returns true if the character is hiragana or katakana.
fn is_kana(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}')
}

/// Returns true if the character is kanji or kana.
#[allow(dead_code)]
fn is_japanese(c: char) -> bool {
    is_kanji(c) || is_kana(c)
}

/// Returns true if the line (trimmed) is purely kana (+ prolonged sound mark ー)
/// and at most 12 characters long — hallmark of PDF-extracted furigana.
fn is_kana_only_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let char_count = trimmed.chars().count();
    if char_count > 12 {
        return false;
    }
    trimmed.chars().all(|c| is_kana(c) || c == 'ー')
}

/// Strip furigana annotations from Japanese text.
/// Handles PDF-extracted kana lines, HTML ruby tags, delimiter-based readings,
/// and inline spacing artifacts.
pub fn strip_furigana(text: &str) -> String {
    let text = strip_isolated_kana_lines(text);
    let text = strip_ruby_tags(&text);
    let text = strip_delimiter_furigana(&text);
    clean_inline_spacing(&text)
}

/// Phase A: Remove isolated kana-only lines (PDF furigana) and collapse
/// resulting consecutive blank lines.
fn strip_isolated_kana_lines(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut prev_blank = false;

    for line in text.lines() {
        if is_kana_only_line(line) {
            // Skip this furigana line; treat as blank for collapse logic
            if !prev_blank {
                result.push('\n');
                prev_blank = true;
            }
            continue;
        }

        let is_blank = line.trim().is_empty();
        if is_blank && prev_blank {
            continue; // collapse consecutive blanks
        }
        prev_blank = is_blank;

        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line);
    }

    result
}

/// Phase B: Strip HTML ruby tags — remove <rt>...</rt>, <rp>...</rp> and
/// <ruby>/<\/ruby> wrappers.
fn strip_ruby_tags(text: &str) -> String {
    static RT_RE: OnceLock<Regex> = OnceLock::new();
    static RUBY_TAG_RE: OnceLock<Regex> = OnceLock::new();

    let rt_re = RT_RE.get_or_init(|| Regex::new(r"<r[tp]>[^<]*</r[tp]>").unwrap());
    let ruby_re = RUBY_TAG_RE.get_or_init(|| Regex::new(r"</?ruby>").unwrap());

    let text = rt_re.replace_all(text, "");
    ruby_re.replace_all(&text, "").into_owned()
}

/// Phase C: Strip delimiter-based furigana using a character-by-character scan.
/// Only strips content inside delimiters when preceded by kanji and containing
/// only kana.
fn strip_delimiter_furigana(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut result = String::with_capacity(text.len());
    let mut i = 0;

    let delimiters: &[(char, char)] = &[
        ('(', ')'),
        ('（', '）'),
        ('【', '】'),
        ('《', '》'),
    ];

    while i < len {
        let c = chars[i];

        // Check if this char is an opening delimiter
        if let Some(&(_, close)) = delimiters.iter().find(|&&(open, _)| open == c) {
            // Check if preceded by kanji
            let prev_is_kanji = result.chars().last().is_some_and(is_kanji);

            if prev_is_kanji {
                // Scan ahead for the closing delimiter
                let start = i + 1;
                let end = chars[start..].iter().position(|&ch| ch == close).map(|p| p + start);

                if let Some(end_idx) = end {
                    let content: String = chars[start..end_idx].iter().collect();
                    // Check if content is non-empty and purely kana
                    if !content.is_empty() && content.chars().all(|ch| is_kana(ch) || ch == 'ー') {
                        // Skip the delimiter and its content (furigana)
                        i = end_idx + 1;
                        continue;
                    }
                }
            }
        }

        result.push(c);
        i += 1;
    }

    result
}

/// Phase D: Clean inline spacing artifacts from PDF extraction.
/// Removes spaces around isolated kanji (1-3 chars) when surrounded by
/// Japanese characters.
fn clean_inline_spacing(text: &str) -> String {
    static SPACING_RE: OnceLock<Regex> = OnceLock::new();

    let re = SPACING_RE.get_or_init(|| {
        Regex::new(
            r"([\u{3040}-\u{309F}\u{30A0}-\u{30FF}\u{4E00}-\u{9FFF}\u{3400}-\u{4DBF}])\s+([\u{4E00}-\u{9FFF}\u{3400}-\u{4DBF}]{1,3})\s+([\u{3040}-\u{309F}\u{30A0}-\u{30FF}\u{4E00}-\u{9FFF}\u{3400}-\u{4DBF}\u{3000}-\u{303F}])"
        ).unwrap()
    });

    re.replace_all(text, "$1$2$3").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_isolated_kana_lines_removed() {
        let input = "すわ\n\n腕組をして枕元に坐っていると\n\nあおむき\n\n仰向に寝た女が";
        let result = strip_furigana(input);
        assert!(!result.contains("すわ\n"));
        assert!(!result.contains("あおむき"));
        assert!(result.contains("腕組をして枕元に坐っていると"));
        assert!(result.contains("仰向に寝た女が"));
    }

    #[test]
    fn test_consecutive_blank_lines_collapsed() {
        let input = "first line\n\nすわ\n\n\nsecond line";
        let result = strip_isolated_kana_lines(input);
        // Should not have more than one consecutive blank line
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn test_html_ruby_tags_stripped() {
        let input = "<ruby>漢字<rt>かんじ</rt></ruby>を勉強する";
        let result = strip_furigana(input);
        assert_eq!(result, "漢字を勉強する");
    }

    #[test]
    fn test_ruby_with_rp_tags() {
        let input = "<ruby>漢字<rp>(</rp><rt>かんじ</rt><rp>)</rp></ruby>";
        let result = strip_furigana(input);
        assert_eq!(result, "漢字");
    }

    #[test]
    fn test_parentheses_furigana_stripped() {
        let input = "私(わたし)は日本語(にほんご)を勉強(べんきょう)しています。";
        let result = strip_furigana(input);
        assert_eq!(result, "私は日本語を勉強しています。");
    }

    #[test]
    fn test_fullwidth_parentheses_furigana_stripped() {
        let input = "漢字（かんじ）を読む";
        let result = strip_furigana(input);
        assert_eq!(result, "漢字を読む");
    }

    #[test]
    fn test_bracket_furigana_stripped() {
        let input = "漢字【かんじ】を読む";
        let result = strip_furigana(input);
        assert_eq!(result, "漢字を読む");
    }

    #[test]
    fn test_aozora_furigana_stripped() {
        let input = "漢字《かんじ》を読む";
        let result = strip_furigana(input);
        assert_eq!(result, "漢字を読む");
    }

    #[test]
    fn test_inline_spacing_cleaned() {
        let input = "分も 確 にこれは死ぬなと思った";
        let result = strip_furigana(input);
        assert_eq!(result, "分も確にこれは死ぬなと思った");
    }

    #[test]
    fn test_multiple_inline_spacing() {
        let input = "大きな 潤 のある眼で、長い 睫 に包ま";
        let result = strip_furigana(input);
        assert_eq!(result, "大きな潤のある眼で、長い睫に包ま");
    }

    #[test]
    fn test_legitimate_parenthetical_preserved() {
        let input = "東京(日本の首都)は大きい";
        let result = strip_furigana(input);
        assert_eq!(result, "東京(日本の首都)は大きい");
    }

    #[test]
    fn test_non_japanese_preserved() {
        let input = "hello(world) test";
        let result = strip_furigana(input);
        assert_eq!(result, "hello(world) test");
    }

    #[test]
    fn test_empty_parens_preserved() {
        let input = "漢字()を読む";
        let result = strip_furigana(input);
        assert_eq!(result, "漢字()を読む");
    }

    #[test]
    fn test_mixed_content_parens_preserved() {
        let input = "漢字(かんじ123)を読む";
        let result = strip_furigana(input);
        assert_eq!(result, "漢字(かんじ123)を読む");
    }

    #[test]
    fn test_full_pipeline_yume_juuya() {
        let input = "\
すわ

あおむき

腕組をして枕元に坐っていると、仰向に寝た女が、

りんかく

静かな声でもう死にますと云った。女は長い髪を 枕 の上へ

う

分も 確 にこれは死ぬなと思った。";

        let result = strip_furigana(input);

        // Furigana lines removed
        assert!(!result.contains("\nすわ\n"));
        assert!(!result.contains("\nあおむき\n"));
        assert!(!result.contains("\nりんかく\n"));
        assert!(!result.contains("\nう\n"));

        // Real text preserved
        assert!(result.contains("腕組をして枕元に坐っていると、仰向に寝た女が、"));
        assert!(result.contains("静かな声でもう死にますと云った。"));

        // Spacing artifacts cleaned
        assert!(result.contains("分も確にこれは死ぬなと思った。"));
    }

    #[test]
    fn test_long_kana_line_preserved() {
        // 13+ chars of kana should be kept (likely a real sentence)
        let input = "これはとてもながいひらがなのぶんです\n\nother line";
        let result = strip_furigana(input);
        assert!(result.contains("これはとてもながいひらがなのぶんです"));
    }
}
