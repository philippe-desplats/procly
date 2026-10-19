//! The Guard: the Claude Code `PreToolUse` hook Procly registers in the settings of a Worktree.
//!
//! JSON holds no comments, so the Guard has no markers. It is the hook group whose command is `procly guard`, and
//! the fingerprint of the group Procly wrote is recorded as for any Managed block, which tells a group Procly wrote
//! from one the user edited (ADR 0008).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::managed::{self, ManagedBlocks, ManagedError, Origin, Outcome, Removal};

/// The command of the hook, which reads the tool call on its standard input.
pub(crate) const COMMAND: &str = "procly guard";
/// The name the fingerprint of the Guard is recorded under.
pub(super) const BLOCK: &str = "guard";

#[derive(Debug, thiserror::Error)]
pub(super) enum HookError {
    #[error(transparent)]
    Managed(#[from] ManagedError),
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot parse {path}, fix or remove it")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("cannot add the Guard to {path}: {what}")]
    Shape { path: PathBuf, what: &'static str },
}

/// The hook group Procly writes: every Bash tool call goes through `procly guard`.
fn group() -> Value {
    json!({
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": COMMAND }],
    })
}

/// Whether a hook group runs `procly guard`.
fn is_guard(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .is_some_and(|hooks| hooks.iter().any(|hook| hook["command"] == COMMAND))
}

/// Makes the settings file `file` register the Guard, creating the file and its directory when they are missing.
/// Everything else in the file is kept, and a Guard the user edited is left alone.
pub(super) fn install(managed: &ManagedBlocks, file: &Path) -> Result<Outcome, HookError> {
    let (text, existed) = match fs::read_to_string(file) {
        Ok(text) => (text, true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (String::new(), false),
        Err(source) => {
            return Err(HookError::Read {
                path: file.to_owned(),
                source,
            });
        }
    };
    let mut settings = parse_settings(file, &text)?;
    let shape = |what| HookError::Shape {
        path: file.to_owned(),
        what,
    };
    let groups = settings
        .as_object_mut()
        .ok_or_else(|| shape("the settings are not a JSON object"))?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| shape("`hooks` is not an object"))?
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| shape("`hooks.PreToolUse` is not an array"))?;

    // The file is written again as a whole, so its text is kept to give it back to the byte.
    let mut origin = None;
    let outcome = match groups.iter().position(is_guard) {
        None => {
            groups.push(group());
            origin = Some(Origin {
                existed,
                newline_added: false,
                original: existed.then_some(text),
            });
            Outcome::Written
        }
        Some(at) if groups[at] == group() => Outcome::Unchanged,
        Some(at) if managed.is_recorded(file, BLOCK, &compact(&groups[at]))? => {
            groups[at] = group();
            Outcome::Written
        }
        Some(_) => Outcome::Edited,
    };
    if outcome == Outcome::Written {
        let mut text = serde_json::to_string_pretty(&settings).expect("a JSON value always serializes");
        text.push('\n');
        managed::write_text(file, &text)?;
        managed.record(file, BLOCK, &compact(&group()), origin)?;
    }
    Ok(outcome)
}

/// Takes the Guard out of the settings file `file` when Procly wrote it and nobody edited it. The hooks the user has
/// stay, and the keys that end up empty go, so that a file Procly created is deleted with it.
pub(super) fn remove(managed: &ManagedBlocks, file: &Path) -> Result<Removal, HookError> {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Removal::Absent),
        Err(source) => {
            return Err(HookError::Read {
                path: file.to_owned(),
                source,
            });
        }
    };
    let Ok(mut settings) = serde_json::from_str::<Value>(&text) else {
        // A file that is no JSON cannot be told apart from an edited Guard once it names the command.
        return Ok(if text.contains(COMMAND) {
            Removal::Edited
        } else {
            Removal::Absent
        });
    };
    let Some(groups) = settings
        .get_mut("hooks")
        .and_then(|hooks| hooks.get_mut("PreToolUse"))
        .and_then(Value::as_array_mut)
    else {
        return Ok(Removal::Absent);
    };
    let Some(at) = groups.iter().position(is_guard) else {
        // The developer took the Guard out by hand, so its fingerprint describes nothing anymore.
        managed.forget_block(file, BLOCK)?;
        return Ok(Removal::Absent);
    };
    if groups[at] != group() && !managed.is_recorded(file, BLOCK, &compact(&groups[at]))? {
        return Ok(Removal::Edited);
    }
    groups.remove(at);
    let origin = managed.origin(file, BLOCK)?;
    // The file as the developer had it when nothing else changed in it, layout included, or else as the settings
    // are now without the Guard.
    let original = origin
        .as_ref()
        .and_then(|origin| origin.original.as_deref())
        .filter(|original| {
            let unchanged = |settings: &Value| parse_settings(file, original).is_ok_and(|parsed| parsed == *settings);
            let mut pruned = settings.clone();
            prune_empty_hooks(&mut pruned);
            unchanged(&settings) || unchanged(&pruned)
        });
    let created = !origin.as_ref().is_some_and(|origin| origin.existed);
    prune_empty_hooks(&mut settings);
    let deleted = created && settings.as_object().is_some_and(Map::is_empty);
    if deleted {
        fs::remove_file(file).map_err(|source| ManagedError::Remove {
            path: file.to_owned(),
            source,
        })?;
    } else if let Some(original) = original {
        managed::write_text(file, original)?;
    } else {
        let mut text = serde_json::to_string_pretty(&settings).expect("a JSON value always serializes");
        text.push('\n');
        managed::write_text(file, &text)?;
    }
    managed.forget_block(file, BLOCK)?;
    Ok(Removal::Removed { deleted })
}

/// Removes `hooks.PreToolUse` and `hooks` from the settings when the Guard was all they held.
fn prune_empty_hooks(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    if hooks
        .get("PreToolUse")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        hooks.shift_remove("PreToolUse");
    }
    if hooks.is_empty() {
        settings
            .as_object_mut()
            .expect("the settings are an object, they held `hooks`")
            .shift_remove("hooks");
    }
}

/// The settings a file holds, an empty file being empty settings.
fn parse_settings(file: &Path, text: &str) -> Result<Value, HookError> {
    if text.trim().is_empty() {
        Ok(Value::Object(Map::new()))
    } else {
        serde_json::from_str(text).map_err(|source| HookError::Parse {
            path: file.to_owned(),
            source,
        })
    }
}

fn compact(group: &Value) -> String {
    serde_json::to_string(group).expect("a JSON value always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, ManagedBlocks, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let blocks = ManagedBlocks::new(dir.path().join("managed-blocks.json"));
        let file = dir.path().join(".claude/settings.json");
        (dir, blocks, file)
    }

    fn settings(file: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
    }

    #[test]
    fn the_guard_is_registered_in_a_settings_file_that_does_not_exist_yet() {
        let (_dir, blocks, file) = fixture();

        assert_eq!(install(&blocks, &file).unwrap(), Outcome::Written);

        assert_eq!(settings(&file), json!({ "hooks": { "PreToolUse": [group()] } }));
    }

    #[test]
    fn the_settings_and_hooks_of_the_developer_are_kept_in_their_order() {
        let (_dir, blocks, file) = fixture();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            r#"{"model":"opus","hooks":{"PostToolUse":[{"hooks":[{"type":"command","command":"fmt"}]}],"PreToolUse":[{"matcher":"Edit","hooks":[{"type":"command","command":"lint"}]}]},"env":{"A":"1"}}"#,
        )
        .unwrap();

        assert_eq!(install(&blocks, &file).unwrap(), Outcome::Written);

        let written = settings(&file);
        let keys: Vec<_> = written.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks", "env"]);
        assert_eq!(written["hooks"]["PostToolUse"][0]["hooks"][0]["command"], "fmt");
        assert_eq!(written["hooks"]["PreToolUse"][0]["matcher"], "Edit");
        assert_eq!(written["hooks"]["PreToolUse"][1], group());
    }

    #[test]
    fn a_second_install_changes_nothing() {
        let (_dir, blocks, file) = fixture();
        install(&blocks, &file).unwrap();
        let first = fs::read(&file).unwrap();

        assert_eq!(install(&blocks, &file).unwrap(), Outcome::Unchanged);

        assert_eq!(fs::read(&file).unwrap(), first);
    }

    #[test]
    fn an_edited_guard_is_left_alone_and_one_procly_wrote_earlier_is_updated() {
        let (_dir, blocks, file) = fixture();
        install(&blocks, &file).unwrap();

        // The developer narrowed the matcher of the Guard.
        let mut edited = settings(&file);
        edited["hooks"]["PreToolUse"][0]["matcher"] = json!("Bash|Task");
        fs::write(&file, serde_json::to_string_pretty(&edited).unwrap()).unwrap();
        assert_eq!(install(&blocks, &file).unwrap(), Outcome::Edited);
        assert_eq!(settings(&file), edited);

        // A group an earlier Procly wrote is recorded, so a later Procly may replace it.
        let older = json!({ "matcher": "Bash", "hooks": [{ "type": "command", "command": "procly guard" }, { "type": "command", "command": "older" }] });
        let mut earlier = settings(&file);
        earlier["hooks"]["PreToolUse"][0] = older.clone();
        fs::write(&file, serde_json::to_string_pretty(&earlier).unwrap()).unwrap();
        blocks.record(&file, BLOCK, &compact(&older), None).unwrap();
        assert_eq!(install(&blocks, &file).unwrap(), Outcome::Written);
        assert_eq!(settings(&file)["hooks"]["PreToolUse"][0], group());
    }

    #[test]
    fn a_settings_file_that_is_not_json_is_refused_and_left_as_it_is() {
        let (_dir, blocks, file) = fixture();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{ not json").unwrap();

        let error = install(&blocks, &file).unwrap_err();

        assert!(matches!(error, HookError::Parse { .. }), "{error}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ not json");
    }

    #[test]
    fn removing_the_guard_deletes_the_settings_file_it_created_and_forgets_it() {
        let (_dir, blocks, file) = fixture();
        install(&blocks, &file).unwrap();

        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Removed { deleted: true });

        assert!(!file.exists());
        assert_eq!(blocks.files_holding(BLOCK).unwrap(), Vec::<PathBuf>::new());
        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Absent);
    }

    #[test]
    fn removing_the_guard_keeps_every_other_hook_and_setting_in_their_order() {
        let (_dir, blocks, file) = fixture();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let own = r#"{"model":"opus","hooks":{"PostToolUse":[{"hooks":[{"type":"command","command":"fmt"}]}],"PreToolUse":[{"matcher":"Edit","hooks":[{"type":"command","command":"lint"}]}]},"env":{"A":"1"}}"#;
        fs::write(&file, own).unwrap();
        install(&blocks, &file).unwrap();

        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Removed { deleted: false });

        let written = settings(&file);
        assert_eq!(written, serde_json::from_str::<Value>(own).unwrap());
        let keys: Vec<_> = written.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks", "env"]);
    }

    #[test]
    fn removing_the_guard_drops_the_hooks_it_alone_filled() {
        let (_dir, blocks, file) = fixture();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{"model":"opus"}"#).unwrap();
        install(&blocks, &file).unwrap();

        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Removed { deleted: false });

        assert_eq!(settings(&file), json!({ "model": "opus" }));
    }

    #[test]
    fn an_edited_guard_or_a_file_that_is_not_json_is_left_alone() {
        let (_dir, blocks, file) = fixture();
        install(&blocks, &file).unwrap();
        let mut edited = settings(&file);
        edited["hooks"]["PreToolUse"][0]["matcher"] = json!("Bash|Task");
        let text = serde_json::to_string_pretty(&edited).unwrap();
        fs::write(&file, &text).unwrap();
        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Edited);
        assert_eq!(fs::read_to_string(&file).unwrap(), text);

        fs::write(&file, "{ procly guard").unwrap();
        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Edited);
        fs::write(&file, "{ not json").unwrap();
        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Absent);
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ not json");
    }

    #[test]
    fn removing_the_guard_gives_the_file_back_in_the_layout_the_developer_wrote() {
        let (_dir, blocks, file) = fixture();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let own = "{\n    \"model\": \"opus\"\n}\n";
        fs::write(&file, own).unwrap();
        install(&blocks, &file).unwrap();

        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Removed { deleted: false });

        assert_eq!(fs::read_to_string(&file).unwrap(), own);
    }

    #[test]
    fn an_empty_settings_file_stays_an_empty_file_and_a_missing_one_goes() {
        let (_dir, blocks, file) = fixture();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "").unwrap();
        install(&blocks, &file).unwrap();

        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Removed { deleted: false });

        assert_eq!(fs::read(&file).unwrap(), b"");
    }

    #[test]
    fn a_settings_file_changed_since_the_guard_went_in_keeps_the_changes() {
        let (_dir, blocks, file) = fixture();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{\n    \"model\": \"opus\"\n}\n").unwrap();
        install(&blocks, &file).unwrap();
        let mut changed = settings(&file);
        changed["model"] = json!("sonnet");
        fs::write(&file, serde_json::to_string_pretty(&changed).unwrap()).unwrap();

        assert_eq!(remove(&blocks, &file).unwrap(), Removal::Removed { deleted: false });

        assert_eq!(settings(&file), json!({ "model": "sonnet" }));
    }
}
