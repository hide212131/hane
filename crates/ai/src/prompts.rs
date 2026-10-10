//! Independent, GPUI-free storage for the user's saved selection-action AI
//! prompts, per the "AIプロンプト" settings page (Issue #449).
//!
//! Deliberately separate from [`crate::settings::AiSettings`]: saving a
//! prompt list must never read, write, or otherwise affect the connection
//! settings, `settings_generation`, or runtime restart decisions, and vice
//! versa. This module owns its own file, its own lock
//! (`AiPaths::user_prompts_lock_path`), and its own optimistic-concurrency
//! `revision` counter.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

use crate::atomic_file::{AtomicWriteError, atomic_write_bytes, read_to_string_if_exists};
use crate::settings_lock::AiSettingsLock;

pub const USER_PROMPTS_SCHEMA_VERSION: u32 = 1;
/// Unicode scalar count, not bytes: section 2.2 of the implementation spec
/// defines the limit in characters.
pub const MAX_PROMPT_TITLE_CHARS: usize = 80;
pub const MAX_PROMPT_BODY_BYTES: usize = 8 * 1024;
pub const MAX_PROMPT_COUNT: usize = 64;

/// One saved instruction, shown in the selection AI menu by `title`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPrompt {
    pub id: u64,
    pub title: String,
    pub prompt: String,
}

/// The persisted file contents. `next_id` only ever increases: a deleted
/// prompt's id is never reused, and renaming a prompt never changes its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPrompts {
    pub schema_version: u32,
    pub revision: u64,
    pub next_id: u64,
    pub prompts: Vec<UserPrompt>,
}

/// One edited-or-new row from the settings page's in-memory draft list.
/// `id: None` means "not yet assigned an id" (a brand-new entry); `save`
/// assigns it one from the persisted `next_id` counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPromptDraft {
    pub id: Option<u64>,
    pub title: String,
    pub prompt: String,
}

/// The single built-in suggestion shown only while nothing has ever been
/// saved (`UserPromptsStore::load` returned `Ok(None)`). Never written to
/// disk by itself, and never re-shown once an explicit save — even one that
/// leaves the list empty — has landed.
pub fn initial_default_draft() -> UserPromptDraft {
    UserPromptDraft {
        id: None,
        title: "校正する".to_owned(),
        prompt: "以下の文章を校閲して適切な文章にする".to_owned(),
    }
}

#[derive(Debug)]
pub enum UserPromptsLoadError {
    Io(io::Error),
    /// The file's `schema_version` is newer than this build understands.
    /// Fails closed rather than guessing at fields it does not recognize.
    UnsupportedSchemaVersion { found: u32, supported: u32 },
}

impl std::fmt::Display for UserPromptsLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserPromptsLoadError::Io(e) => write!(f, "I/O error loading AI prompts: {e}"),
            UserPromptsLoadError::UnsupportedSchemaVersion { found, supported } => write!(
                f,
                "AI prompts file has schema_version {found}, which is newer than the highest version {supported} this build supports"
            ),
        }
    }
}

impl std::error::Error for UserPromptsLoadError {}

#[derive(Debug)]
pub enum UserPromptsSaveError {
    Io(io::Error),
    /// Another save is already in progress in this or another process.
    /// Never waited on; rejected immediately.
    Busy,
    /// `expected_revision` no longer matches the persisted revision: reload
    /// and retry rather than overwriting a concurrent change.
    RevisionConflict { current: Box<UserPrompts> },
    TooManyPrompts { max: usize },
    TitleEmpty,
    TitleContainsNewline,
    TitleTooLong { max_chars: usize },
    PromptEmpty,
    PromptTooLarge { max_bytes: usize },
    IdCounterOverflow,
    RevisionOverflow,
    /// The atomic replace's `rename` already landed, but this process could
    /// not confirm its crash-durability. The write must be treated as having
    /// happened; callers must reload rather than silently retry blind.
    PersistedDurabilityUnconfirmed(io::Error),
}

impl std::fmt::Display for UserPromptsSaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserPromptsSaveError::Io(e) => write!(f, "I/O error saving AI prompts: {e}"),
            UserPromptsSaveError::Busy => {
                write!(f, "AI prompts are locked by another save; try again")
            }
            UserPromptsSaveError::RevisionConflict { .. } => {
                write!(f, "AI prompts were changed concurrently; reload before saving")
            }
            UserPromptsSaveError::TooManyPrompts { max } => {
                write!(f, "a maximum of {max} saved prompts is supported")
            }
            UserPromptsSaveError::TitleEmpty => write!(f, "a prompt title cannot be empty"),
            UserPromptsSaveError::TitleContainsNewline => {
                write!(f, "a prompt title cannot contain a line break")
            }
            UserPromptsSaveError::TitleTooLong { max_chars } => {
                write!(f, "a prompt title cannot exceed {max_chars} characters")
            }
            UserPromptsSaveError::PromptEmpty => write!(f, "a prompt body cannot be empty"),
            UserPromptsSaveError::PromptTooLarge { max_bytes } => {
                write!(f, "a prompt body cannot exceed {max_bytes} bytes")
            }
            UserPromptsSaveError::IdCounterOverflow => {
                write!(f, "the saved prompt id counter overflowed")
            }
            UserPromptsSaveError::RevisionOverflow => {
                write!(f, "the saved prompt revision counter overflowed")
            }
            UserPromptsSaveError::PersistedDurabilityUnconfirmed(e) => write!(
                f,
                "AI prompts replace may have already taken effect, but its crash-durability could not be confirmed: {e}"
            ),
        }
    }
}

impl std::error::Error for UserPromptsSaveError {}

/// Reads/writes a single `UserPrompts` file, guarded by its own
/// [`AiSettingsLock`] instance (a plain path-scoped OS file lock, unrelated
/// to the AI connection settings lock of the same type).
pub struct UserPromptsStore {
    path: PathBuf,
    lock: AiSettingsLock,
}

impl UserPromptsStore {
    pub fn new(path: impl Into<PathBuf>, lock_path: impl Into<PathBuf>) -> Self {
        UserPromptsStore {
            path: path.into(),
            lock: AiSettingsLock::new(lock_path),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `Ok(None)` means the file has never been saved: callers should show
    /// `initial_default_draft()` without persisting anything yet. Any other
    /// failure (corrupt JSON, unsupported schema version, permission error)
    /// is a distinct `Err`, never silently treated as "never saved" — doing
    /// so would let a save overwrite data this process merely failed to
    /// read.
    pub fn load(&self) -> Result<Option<UserPrompts>, UserPromptsLoadError> {
        let contents = read_to_string_if_exists(&self.path).map_err(UserPromptsLoadError::Io)?;
        let Some(contents) = contents else {
            return Ok(None);
        };
        let prompts: UserPrompts = serde_json::from_str(&contents)
            .map_err(|e| UserPromptsLoadError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
        if prompts.schema_version > USER_PROMPTS_SCHEMA_VERSION {
            return Err(UserPromptsLoadError::UnsupportedSchemaVersion {
                found: prompts.schema_version,
                supported: USER_PROMPTS_SCHEMA_VERSION,
            });
        }
        Ok(Some(prompts))
    }

    /// Validates `drafts`, assigns ids to any new entry, and atomically
    /// persists the result as one new revision. Saving an empty `drafts`
    /// list is valid and, once persisted, is never silently replaced by
    /// `initial_default_draft()` again.
    pub fn save(
        &self,
        expected_revision: u64,
        drafts: Vec<UserPromptDraft>,
    ) -> Result<UserPrompts, UserPromptsSaveError> {
        let guard = self
            .lock
            .try_acquire_exclusive()
            .map_err(UserPromptsSaveError::Io)?
            .ok_or(UserPromptsSaveError::Busy)?;
        let _guard = guard;

        let current = self.load().map_err(|error| match error {
            UserPromptsLoadError::Io(e) => UserPromptsSaveError::Io(e),
            UserPromptsLoadError::UnsupportedSchemaVersion { found, supported } => {
                UserPromptsSaveError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "cannot save: on-disk schema_version {found} is newer than the supported {supported}"
                    ),
                ))
            }
        })?;
        let (current_revision, mut next_id) = current
            .as_ref()
            .map_or((0, 1), |current| (current.revision, current.next_id));
        if current_revision != expected_revision {
            return Err(UserPromptsSaveError::RevisionConflict {
                current: Box::new(current.unwrap_or_else(|| UserPrompts {
                    schema_version: USER_PROMPTS_SCHEMA_VERSION,
                    revision: 0,
                    next_id: 1,
                    prompts: Vec::new(),
                })),
            });
        }
        if drafts.len() > MAX_PROMPT_COUNT {
            return Err(UserPromptsSaveError::TooManyPrompts {
                max: MAX_PROMPT_COUNT,
            });
        }

        let mut prompts = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let title = draft.title.trim().to_owned();
            if title.is_empty() {
                return Err(UserPromptsSaveError::TitleEmpty);
            }
            if title.contains('\n') || title.contains('\r') {
                return Err(UserPromptsSaveError::TitleContainsNewline);
            }
            if title.chars().count() > MAX_PROMPT_TITLE_CHARS {
                return Err(UserPromptsSaveError::TitleTooLong {
                    max_chars: MAX_PROMPT_TITLE_CHARS,
                });
            }
            if draft.prompt.trim().is_empty() {
                return Err(UserPromptsSaveError::PromptEmpty);
            }
            if draft.prompt.len() > MAX_PROMPT_BODY_BYTES {
                return Err(UserPromptsSaveError::PromptTooLarge {
                    max_bytes: MAX_PROMPT_BODY_BYTES,
                });
            }
            let id = match draft.id {
                Some(id) => id,
                None => {
                    let assigned = next_id;
                    next_id = next_id
                        .checked_add(1)
                        .ok_or(UserPromptsSaveError::IdCounterOverflow)?;
                    assigned
                }
            };
            prompts.push(UserPrompt {
                id,
                title,
                prompt: draft.prompt,
            });
        }

        let new_revision = current_revision
            .checked_add(1)
            .ok_or(UserPromptsSaveError::RevisionOverflow)?;
        let new_prompts = UserPrompts {
            schema_version: USER_PROMPTS_SCHEMA_VERSION,
            revision: new_revision,
            next_id,
            prompts,
        };
        let bytes = serde_json::to_vec_pretty(&new_prompts)
            .map_err(|e| UserPromptsSaveError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
        match atomic_write_bytes(&self.path, &bytes) {
            Ok(()) => Ok(new_prompts),
            Err(AtomicWriteError::NotPersisted(e)) => Err(UserPromptsSaveError::Io(e)),
            Err(AtomicWriteError::RenameSucceededSyncFailed(e)) => {
                Err(UserPromptsSaveError::PersistedDurabilityUnconfirmed(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "hane-ai-prompts-test-{}-{name}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store(name: &str) -> UserPromptsStore {
        let dir = unique_dir(name);
        UserPromptsStore::new(dir.join("user-prompts.json"), dir.join("user-prompts.lock"))
    }

    fn draft(id: Option<u64>, title: &str, prompt: &str) -> UserPromptDraft {
        UserPromptDraft {
            id,
            title: title.to_owned(),
            prompt: prompt.to_owned(),
        }
    }

    /// T01: no file at all means "never saved": show the initial default
    /// without writing anything.
    #[test]
    fn load_without_a_saved_file_returns_none_and_touches_nothing() {
        let store = store("never_saved");
        assert!(store.load().unwrap().is_none());
        assert!(!store.path().exists());
    }

    /// T01: saving an empty list is valid and, once persisted, must never be
    /// silently replaced by the initial default again.
    #[test]
    fn saving_an_empty_list_persists_and_is_not_replaced_by_the_default_on_reload() {
        let store = store("empty_after_delete");
        let saved = store.save(0, Vec::new()).unwrap();
        assert_eq!(saved.revision, 1);
        assert!(saved.prompts.is_empty());

        let reloaded = store.load().unwrap().expect("file now exists");
        assert!(reloaded.prompts.is_empty());
        assert_eq!(reloaded.revision, 1);
    }

    /// T01: renaming a saved prompt's title must not change its id.
    #[test]
    fn renaming_a_saved_prompt_keeps_its_id() {
        let store = store("rename_keeps_id");
        let first = store
            .save(0, vec![draft(None, "校正する", "本文")])
            .unwrap();
        let id = first.prompts[0].id;

        let renamed = store
            .save(
                first.revision,
                vec![draft(Some(id), "校正する（新）", "本文")],
            )
            .unwrap();
        assert_eq!(renamed.prompts[0].id, id);
        assert_eq!(renamed.prompts[0].title, "校正する（新）");
    }

    /// T02: ids are assigned on save, never reused after a delete, and a
    /// second prompt with the same title as an existing one is allowed.
    #[test]
    fn ids_are_assigned_on_save_and_never_reused_after_deletion() {
        let store = store("ids_not_reused");
        let first = store
            .save(
                0,
                vec![draft(None, "校正する", "本文A"), draft(None, "校正する", "本文B")],
            )
            .unwrap();
        assert_eq!(first.prompts[0].id, 1);
        assert_eq!(first.prompts[1].id, 2);
        assert_eq!(first.next_id, 3);

        // Delete the first prompt and add a brand-new one.
        let after_delete = store
            .save(
                first.revision,
                vec![draft(Some(2), "校正する", "本文B"), draft(None, "新しい指示", "本文C")],
            )
            .unwrap();
        assert_eq!(after_delete.prompts[1].id, 3, "id 1 must never be reused");
        assert_eq!(after_delete.next_id, 4);
    }

    /// T02: a title's leading/trailing whitespace is trimmed before saving,
    /// but a prompt body's own whitespace/newlines are preserved exactly.
    #[test]
    fn title_whitespace_is_trimmed_but_prompt_whitespace_is_preserved() {
        let store = store("trim_title_keep_body");
        let saved = store
            .save(0, vec![draft(None, "  校正する  ", "  本文\n改行  ")])
            .unwrap();
        assert_eq!(saved.prompts[0].title, "校正する");
        assert_eq!(saved.prompts[0].prompt, "  本文\n改行  ");
    }

    /// T02: an empty or newline-only title is rejected; a whitespace-only
    /// prompt body is rejected; the per-field limits are enforced.
    #[test]
    fn invalid_title_and_body_are_rejected_without_truncating() {
        let store = store("validation");
        assert!(matches!(
            store.save(0, vec![draft(None, "  ", "本文")]),
            Err(UserPromptsSaveError::TitleEmpty)
        ));
        assert!(matches!(
            store.save(0, vec![draft(None, "a\nb", "本文")]),
            Err(UserPromptsSaveError::TitleContainsNewline)
        ));
        assert!(matches!(
            store.save(0, vec![draft(None, "校正する", "   ")]),
            Err(UserPromptsSaveError::PromptEmpty)
        ));
        let long_title = "a".repeat(MAX_PROMPT_TITLE_CHARS + 1);
        assert!(matches!(
            store.save(0, vec![draft(None, &long_title, "本文")]),
            Err(UserPromptsSaveError::TitleTooLong { .. })
        ));
        let long_body = "a".repeat(MAX_PROMPT_BODY_BYTES + 1);
        assert!(matches!(
            store.save(0, vec![draft(None, "校正する", &long_body)]),
            Err(UserPromptsSaveError::PromptTooLarge { .. })
        ));
        let too_many: Vec<_> = (0..MAX_PROMPT_COUNT + 1)
            .map(|i| draft(None, &format!("タイトル{i}"), "本文"))
            .collect();
        assert!(matches!(
            store.save(0, too_many),
            Err(UserPromptsSaveError::TooManyPrompts { .. })
        ));
        // Nothing was ever persisted by any of the rejected attempts.
        assert!(store.load().unwrap().is_none());
    }

    /// T03: a stale `expected_revision` is rejected and never overwrites the
    /// concurrently saved data.
    #[test]
    fn stale_expected_revision_is_rejected_and_does_not_overwrite() {
        let store = store("stale_revision");
        let first = store.save(0, vec![draft(None, "A", "本文A")]).unwrap();

        let err = store
            .save(0, vec![draft(None, "B", "本文B")])
            .unwrap_err();
        match err {
            UserPromptsSaveError::RevisionConflict { current } => assert_eq!(*current, first),
            other => panic!("expected RevisionConflict, got {other:?}"),
        }
        assert_eq!(store.load().unwrap().unwrap(), first);
    }

    /// T03: a save is rejected immediately while another exclusive holder
    /// exists, and existing data survives untouched.
    #[test]
    fn save_is_rejected_immediately_while_locked_by_another_holder() {
        let store = store("busy_holder");
        let first = store.save(0, vec![draft(None, "A", "本文A")]).unwrap();
        // Simulate a concurrent holder by taking the same lock path directly.
        let same_path_lock =
            AiSettingsLock::new(store.path().parent().unwrap().join("user-prompts.lock"));
        let held = same_path_lock.try_acquire_exclusive().unwrap().unwrap();

        let err = store.save(first.revision, Vec::new()).unwrap_err();
        assert!(matches!(err, UserPromptsSaveError::Busy));
        assert_eq!(store.load().unwrap().unwrap(), first);

        drop(held);
        store.save(first.revision, Vec::new()).unwrap();
    }

    /// T03: a future/unsupported schema version is reported distinctly from
    /// "never saved" and is never silently overwritten with defaults.
    #[test]
    fn load_rejects_a_future_schema_version_without_overwriting() {
        let store = store("future_schema");
        let future = serde_json::json!({
            "schema_version": USER_PROMPTS_SCHEMA_VERSION + 1,
            "revision": 1,
            "next_id": 2,
            "prompts": [],
        });
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), serde_json::to_vec_pretty(&future).unwrap()).unwrap();

        assert!(matches!(
            store.load(),
            Err(UserPromptsLoadError::UnsupportedSchemaVersion { .. })
        ));
        assert!(matches!(
            store.save(1, Vec::new()),
            Err(UserPromptsSaveError::Io(_))
        ));
        // The on-disk file is untouched.
        let raw = std::fs::read_to_string(store.path()).unwrap();
        assert!(raw.contains(&(USER_PROMPTS_SCHEMA_VERSION + 1).to_string()));
    }

    #[test]
    fn initial_default_draft_is_not_persisted_until_explicitly_saved() {
        let store = store("initial_default_not_persisted");
        let default = initial_default_draft();
        assert_eq!(default.title, "校正する");
        assert!(!store.path().exists());
    }
}
