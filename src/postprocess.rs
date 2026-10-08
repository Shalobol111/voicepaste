//! Cleanup of recognized text before it is pasted. Pure functions only.

/// What `finalize` should do besides the always-on cleanup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Add a space after the text so the next dictation does not stick to it.
    pub trailing_space: bool,
    /// Drop a lone final period (handy for chats).
    pub strip_final_period: bool,
    /// Start the text with a lowercase letter.
    pub lowercase_first: bool,
}

/// Bracketed or starred fragments Whisper emits for non-speech audio.
const NOISE_MARKERS: &[&str] = &[
    "blank_audio",
    "blank audio",
    "music",
    "музыка",
    "applause",
    "аплодисменты",
    "silence",
    "тишина",
    "laughter",
    "смех",
    "noise",
    "шум",
    "inaudible",
    "неразборчиво",
];

/// Whole results that Whisper invents on silence or noise (YouTube-subtitle leftovers).
/// Compared after `normalize`, so case and punctuation do not matter.
const HALLUCINATIONS: &[&str] = &[
    "продолжение следует",
    "субтитры сделал dimatorzok",
    "субтитры создавал dimatorzok",
    "субтитры подогнал",
    "редактор субтитров а синецкая",
    "корректор а егорова",
    "спасибо за просмотр",
    "спасибо за внимание",
    "подписывайтесь на канал",
    "thank you for watching",
    "thanks for watching",
    "please subscribe",
    "subtitles by the amaraorg community",
];

/// Lowercase, letters and digits only, single spaces.
fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// True if the whole text is a known hallucination.
pub fn is_hallucination(text: &str) -> bool {
    let n = normalize(text);
    n.is_empty() || n.contains("dimatorzok") || HALLUCINATIONS.iter().any(|h| normalize(h) == n)
}

/// Removes `[Music]`, `(музыка)`, `*applause*`, `♪` and similar markers. Brackets with
/// real content, such as "(see below)", are kept.
fn remove_markers(text: &str) -> String {
    let chars: Vec<char> = text.chars().filter(|&c| c != '♪').collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let close = match chars[i] {
            '[' => Some(']'),
            '(' => Some(')'),
            '*' => Some('*'),
            _ => None,
        };
        if let Some(close) = close {
            if let Some(len) = chars[i + 1..].iter().position(|&c| c == close) {
                let inner: String = chars[i + 1..i + 1 + len].iter().collect();
                if NOISE_MARKERS.contains(&inner.trim().to_lowercase().as_str()) {
                    i += len + 2;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Lowercases the first letter of the text, unless that would be wrong:
/// acronyms and mixed-case words ("GPU", "McDonald"), the English "I" and "I'm",
/// and texts that start with a digit.
pub fn lowercase_first(text: &str) -> String {
    let Some((idx, ch)) = text.char_indices().find(|(_, c)| c.is_alphanumeric()) else {
        return text.to_string();
    };
    if !ch.is_alphabetic() || !ch.is_uppercase() {
        return text.to_string();
    }
    let word = text[idx..].split_whitespace().next().unwrap_or("");
    let core: String = word
        .chars()
        .take_while(|c| c.is_alphabetic() || *c == '\'' || *c == '\u{2019}')
        .collect();
    let is_acronym_or_mixed = core
        .chars()
        .filter(|c| c.is_alphabetic())
        .skip(1)
        .any(|c| c.is_uppercase());
    let is_english_i = core == "I" || core.starts_with("I'") || core.starts_with("I\u{2019}");
    if is_acronym_or_mixed || is_english_i {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..idx]);
    out.extend(ch.to_lowercase());
    out.push_str(&text[idx + ch.len_utf8()..]);
    out
}

/// Cleans raw Whisper output. Returns an empty string when there is nothing worth
/// pasting (empty, only noise markers, or a known hallucination).
pub fn finalize(raw: &str, opts: &Options) -> String {
    let cleaned = remove_markers(raw);
    if is_hallucination(&cleaned) {
        return String::new();
    }

    let mut out = String::with_capacity(cleaned.len());
    for word in cleaned.split_whitespace() {
        let starts_with_punct = word.starts_with(['.', ',', '!', '?', ':', ';']);
        if !out.is_empty() && !starts_with_punct {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        return out;
    }
    if opts.lowercase_first {
        out = lowercase_first(&out);
    }
    // Keep "..." intact; only a lone final period is dropped.
    if opts.strip_final_period && out.ends_with('.') && !out.ends_with("..") {
        out.pop();
    }
    if opts.trailing_space {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Options = Options {
        trailing_space: false,
        strip_final_period: false,
        lowercase_first: false,
    };

    fn run(raw: &str, opts: Options) -> String {
        finalize(raw, &opts)
    }

    #[test]
    fn collapses_spaces_and_trims() {
        assert_eq!(run("  Привет,   мир.  ", PLAIN), "Привет, мир.");
    }

    #[test]
    fn removes_space_before_punctuation() {
        assert_eq!(run("Привет , мир !", PLAIN), "Привет, мир!");
    }

    #[test]
    fn adds_trailing_space_only_for_non_empty() {
        let o = Options {
            trailing_space: true,
            ..PLAIN
        };
        assert_eq!(run("Hello", o), "Hello ");
        assert_eq!(run("   ", o), "");
        assert_eq!(run("", o), "");
    }

    #[test]
    fn strips_only_a_lone_final_period() {
        let o = Options {
            strip_final_period: true,
            ..PLAIN
        };
        assert_eq!(run("Привет, мир.", o), "Привет, мир");
        assert_eq!(run("Ну...", o), "Ну...");
        assert_eq!(run("Правда?", o), "Правда?");
        let both = Options {
            trailing_space: true,
            ..o
        };
        assert_eq!(run("Привет.", both), "Привет ");
    }

    #[test]
    fn lowercases_the_first_letter() {
        assert_eq!(lowercase_first("Привет, мир"), "привет, мир");
        assert_eq!(lowercase_first("Hello world"), "hello world");
        assert_eq!(lowercase_first("«Привет» сказал он"), "«привет» сказал он");
        assert_eq!(lowercase_first("Ёлка"), "ёлка");
        assert_eq!(lowercase_first("А я думал"), "а я думал");
    }

    #[test]
    fn lowercase_first_leaves_acronyms_and_names_of_letters_alone() {
        assert_eq!(lowercase_first("GPU работает"), "GPU работает");
        assert_eq!(lowercase_first("VS Code открыт"), "VS Code открыт");
        assert_eq!(lowercase_first("McDonald's"), "McDonald's");
        assert_eq!(lowercase_first("I think so"), "I think so");
        assert_eq!(lowercase_first("I'm here"), "I'm here");
        assert_eq!(lowercase_first("I’ll go"), "I’ll go");
        assert_eq!(lowercase_first("I, however, agree"), "I, however, agree");
    }

    #[test]
    fn lowercase_first_does_nothing_when_text_starts_with_a_number_or_lowercase() {
        assert_eq!(lowercase_first("123 Привет"), "123 Привет");
        assert_eq!(lowercase_first("привет"), "привет");
        assert_eq!(lowercase_first("..."), "...");
        assert_eq!(lowercase_first(""), "");
    }

    #[test]
    fn lowercase_option_is_applied_by_finalize() {
        let o = Options {
            lowercase_first: true,
            ..PLAIN
        };
        assert_eq!(run("  Привет, мир.  ", o), "привет, мир.");
    }

    #[test]
    fn noise_markers_are_removed() {
        assert_eq!(run("[BLANK_AUDIO]", PLAIN), "");
        assert_eq!(run("(музыка)", PLAIN), "");
        assert_eq!(run("*Music*", PLAIN), "");
        assert_eq!(run("♪ ♪", PLAIN), "");
        assert_eq!(run("Привет [Music] мир", PLAIN), "Привет мир");
    }

    #[test]
    fn real_brackets_are_kept() {
        assert_eq!(run("Смотри (см. ниже) тут", PLAIN), "Смотри (см. ниже) тут");
        assert_eq!(run("a*b*c", PLAIN), "a*b*c");
    }

    #[test]
    fn known_hallucinations_give_an_empty_result() {
        for h in [
            "Продолжение следует...",
            "Субтитры сделал DimaTorzok",
            "Субтитры создавал DimaTorzok",
            "Редактор субтитров А.Синецкая",
            "Спасибо за просмотр!",
            "Thank you for watching",
            "Thanks for watching!",
            "Подписывайтесь на канал",
            "  продолжение   следует  ",
        ] {
            assert_eq!(run(h, PLAIN), "", "{h}");
        }
    }

    #[test]
    fn hallucination_phrases_inside_real_speech_are_kept() {
        let t = "Скажи спасибо за просмотр моего видео";
        assert_eq!(run(t, PLAIN), t);
        assert_eq!(run("Thank you", PLAIN), "Thank you");
    }
}
