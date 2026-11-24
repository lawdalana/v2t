use std::{
    fs::{self, OpenOptions, TryLockError},
    io,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

use crate::{
    config::invalid,
    text::{Corrections, Entry},
};

pub(super) enum Change {
    Add(Entry),
    Update { row: usize, entry: Entry },
    Remove { row: usize },
}

pub(super) struct Dictionary {
    directory: PathBuf,
    rules: Corrections,
}

impl Dictionary {
    pub(super) fn load(directory: PathBuf) -> io::Result<Self> {
        let rules = Corrections::load(&directory)?;
        Ok(Self { directory, rules })
    }

    pub(super) fn entries(&self) -> &[Entry] {
        &self.rules.entries
    }

    pub(super) fn change(&mut self, change: Change) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let file = self.directory.join("correction.json");
        let file = match file.symlink_metadata() {
            Ok(_) => file.canonicalize()?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => file,
            Err(error) => return Err(error),
        };
        let mut lock_path = file.clone().into_os_string();
        lock_path.push(".v2t-lock");
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another dictionary window is saving. Try again.",
            ),
            TryLockError::Error(error) => error,
        })?;
        let mut updated = Corrections::load_file(&file)?;
        match change {
            Change::Add(entry) => updated.entries.push(entry),
            Change::Update { row, entry } => {
                let row = self.current_row(row, &updated)?;
                updated.entries[row] = entry;
            }
            Change::Remove { row } => {
                let row = self.current_row(row, &updated)?;
                updated.entries.remove(row);
            }
        }
        updated.save_file(&file)?;
        self.rules = updated;
        Ok(())
    }

    fn current_row(&self, row: usize, updated: &Corrections) -> io::Result<usize> {
        self.rules
            .entries
            .get(row)
            .and_then(|old| {
                updated.entries.iter().position(|entry| {
                    entry.spoken == old.spoken && entry.replacement == old.replacement
                })
            })
            .ok_or_else(|| invalid("This rule changed on disk. Reopen Dictionary to reload it."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(spoken: &str, replacement: &str) -> Entry {
        Entry {
            spoken: spoken.into(),
            replacement: replacement.into(),
        }
    }

    #[test]
    fn saved_edits_replace_words_and_preserve_the_ai_preference() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        Corrections {
            enable_ai: false,
            entries: vec![entry("วีทูที", "v2t")],
        }
        .save(path)
        .unwrap();
        let mut dictionary = Dictionary::load(path.to_owned()).unwrap();
        dictionary
            .change(Change::Add(entry("โอเพ่นเราเตอร์", "Open Router")))
            .unwrap();
        dictionary
            .change(Change::Update {
                row: 1,
                entry: entry("โอเพ่นเราเตอร์", "OpenRouter"),
            })
            .unwrap();
        dictionary.change(Change::Remove { row: 0 }).unwrap();
        let saved = Corrections::load(path).unwrap();
        assert!(!saved.enable_ai);
        assert_eq!(saved.entries.len(), 1);
        assert_eq!(saved.apply("ใช้โอเพ่นเราเตอร์").0, "ใช้ OpenRouter");
    }

    #[test]
    fn older_editors_keep_new_rows_and_refuse_to_overwrite_changed_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        let mut first = Dictionary::load(path.to_owned()).unwrap();
        let mut another = Dictionary::load(path.to_owned()).unwrap();
        first
            .change(Change::Add(entry("dictation", "Dictation")))
            .unwrap();
        another
            .change(Change::Add(entry("speaker", "Speaker")))
            .unwrap();
        assert_eq!(another.entries().len(), 2);

        first
            .change(Change::Update {
                row: 0,
                entry: entry("dictation", "v2t"),
            })
            .unwrap();
        assert!(another.change(Change::Remove { row: 0 }).is_err());
        assert_eq!(
            Corrections::load(path)
                .unwrap()
                .apply("dictation speaker")
                .0,
            "v2t Speaker"
        );
    }

    #[test]
    fn updates_the_target_of_a_linked_dictionary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        let target = path.join("custom-rules.json");
        let link = path.join("correction.json");
        Corrections::default().save_file(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let mut dictionary = Dictionary::load(path.to_owned()).unwrap();
        dictionary
            .change(Change::Add(entry(" v2t", " V2t")))
            .unwrap();
        assert!(link.symlink_metadata().unwrap().is_symlink());
        assert_eq!(
            Corrections::load_file(&target).unwrap().apply("use v2t").0,
            "use V2t"
        );
    }
}
