//! Deterministic post-processing. No LLM, no network, sub-millisecond.
//!
//! Everything here runs inside the key-up critical path, so it is plain string
//! work over a single pass: spoken commands, spacing, capitalisation, and the
//! user dictionary. Anything that needs a model belongs in an opt-in "polish"
//! step outside this path.

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Formatter {
    /// Spoken-to-written replacements applied on whole words, case-insensitive
    /// on the left, verbatim on the right.
    dictionary: Vec<(String, String)>,
    pub capitalise_sentences: bool,
    pub spoken_punctuation: bool,
    pub trailing_space: bool,
}

impl Default for Formatter {
    fn default() -> Self {
        Formatter {
            dictionary: Vec::new(),
            capitalise_sentences: true,
            spoken_punctuation: true,
            trailing_space: true,
        }
    }
}

/// Multi-word spoken commands, longest first so "new paragraph" wins over
/// "new". The replacement is a marker the spacing pass understands.
const COMMANDS: &[(&str, &str)] = &[
    ("new paragraph", "\n\n"),
    ("new line", "\n"),
    ("full stop", "."),
    ("question mark", "?"),
    ("exclamation mark", "!"),
    ("open bracket", "("),
    ("close bracket", ")"),
    ("open quote", "\""),
    ("close quote", "\""),
    ("semicolon", ";"),
    ("colon", ":"),
    ("comma", ","),
    ("period", "."),
    ("dash", ","),
    ("hyphen", "-"),
];

impl Formatter {
    pub fn with_dictionary(entries: &HashMap<String, String>) -> Formatter {
        let mut dictionary: Vec<(String, String)> = entries
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
        // Longest source first, so "lift off consulting" beats "lift off".
        dictionary.sort_by(|a, b| b.0.split_whitespace().count().cmp(&a.0.split_whitespace().count()));
        Formatter {
            dictionary,
            ..Default::default()
        }
    }

    pub fn set_dictionary(&mut self, entries: &HashMap<String, String>) {
        *self = Formatter {
            capitalise_sentences: self.capitalise_sentences,
            spoken_punctuation: self.spoken_punctuation,
            trailing_space: self.trailing_space,
            ..Formatter::with_dictionary(entries)
        };
    }

    /// The comma-separated keyterm list handed to the decoder. Biasing the
    /// model beats correcting it afterwards, so the same dictionary feeds both.
    pub fn keyterms(&self) -> String {
        self.dictionary
            .iter()
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn format(&self, raw: &str) -> String {
        let mut text = raw.trim().to_string();
        if text.is_empty() {
            return text;
        }

        if self.spoken_punctuation {
            text = apply_phrases(&text, COMMANDS);
        }
        if !self.dictionary.is_empty() {
            let pairs: Vec<(&str, &str)> = self
                .dictionary
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            text = apply_phrases(&text, &pairs);
        }

        text = fix_spacing(&text);
        if self.capitalise_sentences {
            text = capitalise(&text);
        }
        if self.trailing_space && !text.ends_with(['\n', ' ']) {
            text.push(' ');
        }
        text
    }
}

/// Word-boundary phrase replacement, case-insensitive, first match wins.
/// Single pass over the token stream rather than a regex engine.
fn apply_phrases(text: &str, phrases: &[(&str, &str)]) -> String {
    let tokens: Vec<&str> = text.split_inclusive(char::is_whitespace).collect();
    // Bare words with punctuation stripped, for matching.
    let bare: Vec<String> = tokens
        .iter()
        .map(|t| {
            t.trim()
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_lowercase()
        })
        .collect();

    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    'outer: while i < tokens.len() {
        for (from, to) in phrases {
            let words: Vec<&str> = from.split_whitespace().collect();
            if words.is_empty() || i + words.len() > tokens.len() {
                continue;
            }
            if (0..words.len()).all(|k| bare[i + k] == words[k]) {
                out.push_str(to);
                // Preserve whatever whitespace followed the last token.
                if let Some(last) = tokens[i + words.len() - 1].chars().last() {
                    if last.is_whitespace() && !to.ends_with('\n') {
                        out.push(last);
                    }
                }
                i += words.len();
                continue 'outer;
            }
        }
        out.push_str(tokens[i]);
        i += 1;
    }
    out
}

/// Punctuation hugs the word before it, one space after, no space before a
/// newline. Collapses the double spaces that phrase substitution leaves behind.
fn fix_spacing(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                // Collapse runs, and drop the space entirely before closing
                // punctuation or a newline.
                while matches!(chars.peek(), Some(' ') | Some('\t')) {
                    chars.next();
                }
                match chars.peek() {
                    Some(&next)
                        if matches!(next, '.' | ',' | '!' | '?' | ';' | ':' | ')' | '\n') =>
                    {
                        continue
                    }
                    None => continue,
                    _ => {
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push(' ');
                        }
                    }
                }
            }
            '\n' => {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('\n');
                // Swallow whitespace directly after a newline.
                while matches!(chars.peek(), Some(' ') | Some('\t')) {
                    chars.next();
                }
            }
            _ => {
                // A sentence mark immediately followed by a letter needs a gap.
                out.push(c);
                if matches!(c, '.' | '!' | '?' | ',' | ';' | ':') {
                    if let Some(&next) = chars.peek() {
                        if next.is_alphanumeric() {
                            out.push(' ');
                        }
                    }
                }
            }
        }
    }
    out.trim_end_matches(' ').to_string()
}

/// Capitalise the first letter of the text and of each sentence. Leaves words
/// that are already capitalised alone, so proper nouns from the dictionary and
/// acronyms survive.
fn capitalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start_of_sentence = true;
    for c in text.chars() {
        if start_of_sentence && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            start_of_sentence = false;
        } else {
            out.push(c);
            if matches!(c, '.' | '!' | '?' | '\n') {
                start_of_sentence = true;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f() -> Formatter {
        Formatter::default()
    }

    #[test]
    fn capitalises_and_adds_trailing_space() {
        assert_eq!(f().format("hello there"), "Hello there ");
    }

    #[test]
    fn spoken_punctuation_becomes_marks() {
        assert_eq!(
            f().format("we ship on friday comma then we review full stop"),
            "We ship on friday, then we review. "
        );
    }

    #[test]
    fn new_line_and_paragraph() {
        // A line break starts a new sentence, so the next word is capitalised.
        assert_eq!(f().format("first new line second"), "First\nSecond ");
        assert_eq!(f().format("one new paragraph two"), "One\n\nTwo ");
    }

    #[test]
    fn capitalises_after_a_full_stop() {
        assert_eq!(
            f().format("that is done full stop next item"),
            "That is done. Next item "
        );
    }

    #[test]
    fn dictionary_replaces_whole_phrases_only() {
        let mut d = HashMap::new();
        d.insert("lift off".to_string(), "Lift-Off".to_string());
        d.insert("dddm".to_string(), "DDDM".to_string());
        let fmt = Formatter::with_dictionary(&d);
        assert_eq!(fmt.format("lift off consulting"), "Lift-Off consulting ");
        assert_eq!(fmt.format("dddm limited"), "DDDM limited ");
        // A longer word containing the key must not be touched.
        assert_eq!(fmt.format("liftoff pad"), "Liftoff pad ");
    }

    #[test]
    fn dictionary_feeds_the_decoder_keyterms() {
        let mut d = HashMap::new();
        d.insert("lift off".to_string(), "Lift-Off".to_string());
        let fmt = Formatter::with_dictionary(&d);
        assert_eq!(fmt.keyterms(), "Lift-Off");
    }

    #[test]
    fn no_space_before_punctuation() {
        assert_eq!(f().format("wait comma what"), "Wait, what ");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(f().format("   "), "");
    }

    #[test]
    fn preserves_existing_capitals() {
        assert_eq!(f().format("the NHS trust"), "The NHS trust ");
    }
}
