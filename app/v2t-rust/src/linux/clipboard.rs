#[derive(Debug, PartialEq, Eq)]
pub(super) enum CopyOutcome {
    Copied,
    NoSpeech,
    Failed,
}

pub(super) fn copy_transcript(
    transcript: Option<&str>,
    write_clipboard: impl FnOnce(&str) -> bool,
) -> CopyOutcome {
    match transcript.filter(|text| !text.trim().is_empty()) {
        Some(text) if write_clipboard(text) => CopyOutcome::Copied,
        Some(_) => CopyOutcome::Failed,
        None => CopyOutcome::NoSpeech,
    }
}

#[cfg(test)]
mod tests {
    use super::{CopyOutcome, copy_transcript};

    #[test]
    fn leaves_existing_clipboard_alone_without_speech() {
        for text in [None, Some(""), Some("  \n")] {
            assert_eq!(
                copy_transcript(text, |_| panic!("clipboard would be overwritten")),
                CopyOutcome::NoSpeech
            );
        }
    }

    #[test]
    fn writes_the_complete_mixed_language_transcript() {
        let mut clipboard = String::new();
        let result = copy_transcript(
            Some("พรุ่งนี้มี meeting\nเวลา 9 โมง"),
            |text| {
                clipboard = text.to_owned();
                true
            },
        );
        assert_eq!(result, CopyOutcome::Copied);
        assert_eq!(clipboard, "พรุ่งนี้มี meeting\nเวลา 9 โมง");
    }

    #[test]
    fn reports_a_failed_clipboard_write() {
        assert_eq!(
            copy_transcript(Some("ข้อความ"), |_| false),
            CopyOutcome::Failed
        );
    }
}
