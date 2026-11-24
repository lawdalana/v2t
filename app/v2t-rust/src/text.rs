use crate::config::{invalid, read_bounded, save_json};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io, path::Path};
use unicode_general_category::{GeneralCategory, get_general_category};

const DICTIONARY_FILE_LIMIT: u64 = 640 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Entry {
    pub spoken: String,
    pub replacement: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Corrections {
    #[serde(rename = "EnableAi")]
    pub enable_ai: bool,
    #[serde(rename = "Entries", default, deserialize_with = "entries_or_empty")]
    pub entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct DictionaryImport {
    #[serde(rename = "Entries")]
    entries: Vec<Entry>,
}

impl Default for Corrections {
    fn default() -> Self {
        Self {
            enable_ai: true,
            entries: Vec::new(),
        }
    }
}

impl Corrections {
    pub fn load(dir: &Path) -> io::Result<Self> {
        Self::load_file(&dir.join("correction.json"))
    }

    pub fn load_file(file: &Path) -> io::Result<Self> {
        let Some(bytes) = read_bounded(file, DICTIONARY_FILE_LIMIT)? else {
            return Ok(Self::default());
        };
        let value: Self = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("The saved dictionary is invalid."))?;
        validate_entries(&value.entries)?;
        Ok(value)
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        self.save_file(&dir.join("correction.json"))
    }

    pub fn save_file(&self, file: &Path) -> io::Result<()> {
        validate_entries(&self.entries)?;
        save_json(file, self)
    }

    pub fn export_dictionary(&self, file: &Path) -> io::Result<()> {
        self.save_file(file)
    }

    pub fn import_dictionary(&self, file: &Path) -> io::Result<Self> {
        if !file.metadata()?.is_file() {
            return Err(invalid("Choose a regular JSON dictionary file."));
        }
        let bytes = read_bounded(file, DICTIONARY_FILE_LIMIT)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "The dictionary file is missing.")
        })?;
        let json = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let imported: DictionaryImport = serde_json::from_slice(json)
            .map_err(|_| invalid("The import file must contain a valid JSON Entries list."))?;
        validate_entries(&imported.entries)?;

        let mut merged = self.clone();
        let mut known: HashSet<_> = self
            .entries
            .iter()
            .map(|entry| folded(&entry.spoken))
            .collect();
        merged.entries.extend(
            imported
                .entries
                .into_iter()
                .filter(|entry| known.insert(folded(&entry.spoken))),
        );
        validate_entries(&merged.entries)?;
        Ok(merged)
    }

    pub fn apply(&self, input: &str) -> (String, Vec<String>) {
        if self.entries.is_empty() {
            return (input.to_owned(), Vec::new());
        }

        let mut rules: Vec<_> = self
            .entries
            .iter()
            .map(|entry| {
                let left = needs_boundary(entry.spoken.chars());
                let right = needs_boundary(entry.spoken.chars().rev());
                (
                    entry,
                    folded(&entry.spoken),
                    left,
                    right,
                    is_english(&entry.replacement),
                )
            })
            .collect();
        rules.sort_by_key(|(entry, ..)| std::cmp::Reverse(entry.spoken.encode_utf16().count()));

        let mut output = String::with_capacity(input.len());
        let mut applied = Vec::new();
        let mut offset = 0;
        let mut previous_english = false;
        while offset < input.len() {
            let rest = &input[offset..];
            let matched = rules
                .iter()
                .find_map(|(entry, pattern, left, right, english)| {
                    if *left
                        && input[..offset]
                            .chars()
                            .next_back()
                            .is_some_and(english_word)
                    {
                        return None;
                    }
                    let end = matches_prefix(rest, pattern)?;
                    if *right && rest[end..].chars().next().is_some_and(english_word) {
                        return None;
                    }
                    Some((entry, end, *english))
                });
            let (piece, size, english) = if let Some((entry, end, english)) = matched {
                if !applied.contains(&entry.replacement) {
                    applied.push(entry.replacement.clone());
                }
                (entry.replacement.as_str(), end, english)
            } else {
                let ch = rest.chars().next().expect("nonempty remainder");
                (&rest[..ch.len_utf8()], ch.len_utf8(), false)
            };
            if (english || previous_english)
                && needs_english_space(&output, piece, previous_english, english)
            {
                output.push(' ');
            }
            output.push_str(piece);
            previous_english = english;
            offset += size;
        }
        (output, applied)
    }
}

fn validate_entries(entries: &[Entry]) -> io::Result<()> {
    if entries.len() > 300 {
        return Err(invalid("The dictionary can have up to 300 entries."));
    }
    let mut terms = HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        for field in [&entry.spoken, &entry.replacement] {
            if field.trim().is_empty()
                || field.encode_utf16().count() > 160
                || field.chars().any(char::is_control)
            {
                return Err(invalid(&format!(
                    "Dictionary row {} needs both terms, up to 160 characters each.",
                    index + 1
                )));
            }
        }
        if !terms.insert(folded(&entry.spoken)) {
            return Err(invalid(&format!(
                "Dictionary row {} repeats a spoken term.",
                index + 1
            )));
        }
    }
    Ok(())
}

fn fold(ch: char) -> char {
    let mut upper = ch.to_uppercase();
    let first = upper.next().unwrap_or(ch);
    if upper.next().is_none() { first } else { ch }
}

fn folded(input: &str) -> Vec<char> {
    input.chars().map(fold).collect()
}

fn matches_prefix(text: &str, pattern: &[char]) -> Option<usize> {
    let mut remainder = text.char_indices();
    for expected in pattern {
        if fold(remainder.next()?.1) != *expected {
            return None;
        }
    }
    Some(remainder.next().map_or(text.len(), |(start, _)| start))
}

fn entries_or_empty<'de, D: serde::Deserializer<'de>>(value: D) -> Result<Vec<Entry>, D::Error> {
    Ok(Option::<Vec<Entry>>::deserialize(value)?.unwrap_or_default())
}

fn needs_boundary(mut characters: impl Iterator<Item = char>) -> bool {
    characters
        .find(|ch| ch.is_whitespace() || ch.is_alphanumeric() || *ch == '_')
        .is_some_and(english_word)
}

fn english_word(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn is_english(text: &str) -> bool {
    let mut letters = text.chars().filter(|ch| ch.is_alphabetic());
    letters.next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && letters.all(|ch| ch.is_ascii_alphabetic())
}

fn needs_english_space(output: &str, piece: &str, previous_english: bool, english: bool) -> bool {
    let (Some(left), Some(right)) = (output.chars().next_back(), piece.chars().next()) else {
        return false;
    };
    if left.is_whitespace() || right.is_whitespace() {
        return false;
    }
    match (previous_english, english) {
        (true, true) => true,
        (true, false) => word_neighbor(right),
        (false, true) => word_neighbor(left),
        (false, false) => false,
    }
}

fn word_neighbor(ch: char) -> bool {
    ch.is_alphanumeric()
        || ch == '_'
        || matches!(
            get_general_category(ch),
            GeneralCategory::NonspacingMark
                | GeneralCategory::SpacingMark
                | GeneralCategory::EnclosingMark
        )
}

pub fn for_insertion(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            space = !output.is_empty();
        } else if !ch.is_control() {
            if space {
                output.push(' ');
            }
            space = false;
            output.push(ch);
        }
    }
    output
}

pub fn append_transcript(output: &mut String, transcript: &str) {
    let transcript = transcript.trim();
    if transcript.is_empty() {
        return;
    }
    if !output.is_empty()
        && !transcript.starts_with([',', '.', '!', '?', ':', ';', ')', ']', '}', '…'])
    {
        output.push(' ');
    }
    output.push_str(transcript);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn entry(spoken: &str, replacement: &str) -> Entry {
        Entry {
            spoken: spoken.into(),
            replacement: replacement.into(),
        }
    }

    #[test]
    fn imports_an_export_and_preserves_existing_rules_and_ai_preference() {
        let directory = tempfile::tempdir().unwrap();
        let backup = directory.path().join("พจนานุกรม.json");
        let source = Corrections {
            enable_ai: true,
            entries: vec![entry("ai", "A.I."), entry("โอเพ่นเราเตอร์", "OpenRouter")],
        };
        source.export_dictionary(&backup).unwrap();
        assert_eq!(Corrections::load_file(&backup).unwrap().entries.len(), 2);

        let destination = Corrections {
            enable_ai: false,
            entries: vec![entry("AI", "เอไอ"), entry("วีทูที", "v2t")],
        };
        let imported = destination.import_dictionary(&backup).unwrap();
        assert_eq!(destination.entries.len(), 2);
        assert!(!imported.enable_ai);
        assert_eq!(imported.entries.len(), 3);
        assert_eq!(
            imported.apply("ai วีทูที โอเพ่นเราเตอร์").0,
            "เอไอ v2t OpenRouter"
        );
    }

    #[test]
    fn accepts_an_entries_only_file_with_a_utf8_bom() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("dictionary.json");
        fs::write(
            &file,
            b"\xef\xbb\xbf{\"Entries\":[{\"Spoken\":\"v two t\",\"Replacement\":\"v2t\"}]}",
        )
        .unwrap();
        let current = Corrections::default();
        let imported = current.import_dictionary(&file).unwrap();
        assert_eq!(imported.apply("v two t").0, "v2t");
    }

    #[test]
    fn rejects_invalid_imports_instead_of_treating_them_as_empty_exports() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("dictionary.json");
        let current = Corrections::default();
        assert_eq!(
            current.import_dictionary(&file).err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        for malformed in [
            r#"{}"#,
            r#"{"Entries":null}"#,
            r#"{"Entries":[{"Spoken":"word","Replacement":" "}]}"#,
            r#"{"Entries":[{"Spoken":"word","Replacement":"first"},{"Spoken":"WORD","Replacement":"second"}]}"#,
        ] {
            fs::write(&file, malformed).unwrap();
            assert_eq!(
                current.import_dictionary(&file).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
        }
        fs::write(&file, vec![b' '; 640 * 1024 + 1]).unwrap();
        assert!(current.import_dictionary(&file).is_err());
    }

    #[test]
    fn rejects_a_merge_that_would_exceed_the_dictionary_limit() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("dictionary.json");
        fs::write(
            &file,
            r#"{"Entries":[{"Spoken":"one extra","Replacement":"extra"}]}"#,
        )
        .unwrap();
        let current = Corrections {
            enable_ai: false,
            entries: (0..300)
                .map(|index| entry(&format!("term {index}"), "replacement"))
                .collect(),
        };
        let error = current.import_dictionary(&file).err().unwrap();
        assert!(error.to_string().contains("300 entries"));
    }

    #[test]
    fn joins_segments_in_order_with_phrase_spacing() {
        let mut output = String::new();
        for segment in ["  สวัสดี ", "", "OpenRouter", "!", "พูดต่อ"] {
            append_transcript(&mut output, segment);
        }
        assert_eq!(output, "สวัสดี OpenRouter! พูดต่อ");
    }

    #[test]
    fn preserves_thai_and_english_boundaries_and_applies_longer_terms_first() {
        let dictionary = Corrections {
            enable_ai: true,
            entries: vec![
                Entry {
                    spoken: "AI".into(),
                    replacement: "เอไอ".into(),
                },
                Entry {
                    spoken: "AI model".into(),
                    replacement: "AI model".into(),
                },
                Entry {
                    spoken: "โอเพ่นเราเตอร์".into(),
                    replacement: "OpenRouter".into(),
                },
            ],
        };
        let (text, terms) = dictionary.apply("ลองโอเพ่นเราเตอร์และ ai model, AIsa กับ ai");
        assert_eq!(text, "ลอง OpenRouter และ AI model, AIsa กับ เอไอ");
        assert_eq!(terms, ["OpenRouter", "AI model", "เอไอ"]);
    }

    #[test]
    fn spaces_english_replacements_where_they_touch_words() {
        let dictionary = Corrections {
            enable_ai: false,
            entries: [
                ("โอเพ่นเราเตอร์", "OpenRouter"),
                ("ซีพลัสพลัส", "C++"),
                ("ดอทเน็ต", ".NET"),
                ("เอไอ", " AI "),
                ("คำผิด", "คำถูก"),
            ]
            .into_iter()
            .map(|(spoken, replacement)| Entry {
                spoken: spoken.into(),
                replacement: replacement.into(),
            })
            .collect(),
        };
        for (input, expected) in [
            ("ใช้อยู่โอเพ่นเราเตอร์ได้เลย", "ใช้อยู่ OpenRouter ได้เลย"),
            ("ใช้ โอเพ่นเราเตอร์ ได้เลย", "ใช้ OpenRouter ได้เลย"),
            ("(โอเพ่นเราเตอร์),", "(OpenRouter),"),
            ("โอเพ่นเราเตอร์", "OpenRouter"),
            ("เรียนซีพลัสพลัสดอทเน็ตได้", "เรียน C++ .NET ได้"),
            ("ใช้เอไอวันนี้", "ใช้ AI วันนี้"),
            ("ใช้คำผิดได้", "ใช้คำถูกได้"),
        ] {
            assert_eq!(dictionary.apply(input).0, expected, "input {input:?}");
        }
    }

    #[test]
    fn normalizes_line_breaks_without_corrupting_unicode() {
        assert_eq!(
            for_insertion("  สวัสดี Rust 🙂\r\nข้อความ\t\0 "),
            "สวัสดี Rust 🙂 ข้อความ"
        );
    }
}
