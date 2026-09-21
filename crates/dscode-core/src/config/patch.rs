//! Comment- and unknown-key-preserving TOML overlay.
//!
//! Why this exists: `toml::to_string_pretty(&self)` + `fs::write` is a
//! *replacement*, not a save. It flattens the file to exactly the fields this
//! struct currently declares, so anything the user hand-wrote — comments, and
//! more importantly any **active** key a newer (or older) build understands but
//! this one is not aware of, like `providers.*.api_format` before it was a struct
//! field — is silently deleted the first time the settings page writes anything.
//! The active-key half is the serious one: a hand-set network parameter or a
//! newer build's setting vanishing on an unrelated save is silent data loss.
//!
//! That failure mode is why the desktop frontend has to read the config back,
//! spread it, and re-send the whole thing on every keystroke (`buildSavePayload`
//! in `configStore.ts`). Rust preserving the file is what lets that dance be
//! retired later.
//!
//! The rule, stated exactly:
//!
//!   · A table the layer declares is **authoritative for the keys in it**:
//!     keys present in the layer are written, keys absent from it are removed.
//!     That pruning is what gives `Option::None` real "delete this" meaning — an
//!     empty `enabled_models = []` genuinely differs from the key being absent.
//!   · **Pruning is confined to plain tables the layer also declares as a plain
//!     table.** If the layer renders a key as a *value* (an array-of-tables like
//!     `[[extensions.mcp_servers]]`, an inline table, a scalar), that key is
//!     replaced wholesale and nothing of the user's inside it is touched. This is
//!     the distinction that keeps the rule safe: `providers.deepseek` is a real
//!     table on both sides, so it prunes; `extensions.mcp_servers` is a value, so
//!     it does not.
//!   · The **root** table is never pruned, only added to and updated, so a stray
//!     top-level key a user added by hand survives. (Root-level removals would
//!     only ever fire if a field were deleted from `Config`, and leaving the
//!     stale key behind is harmless — serde ignores keys it does not know.)
//!   · Tables of the base that the layer does **not** mention are left entirely
//!     alone. That is where unknown sections survive.
//!   · Arrays are replaced wholesale, never merged element-wise.
//!   · Existing key decor (the comment and spacing on the key's own line) is
//!     carried over on update, so editing one value does not strip the comment
//!     above it.
//!
//! **One honest limitation**: pruning removes the key and *any comment or
//! commented-out line attached to it*, including from unrelated saves. In
//! practice the stock `config.toml` carries no comments at all (it is written
//! wholesale by `Config::save`), so this costs nothing today; it becomes real the
//! moment someone hand-edits the file, which is exactly when the preservation
//! matters most. Documented rather than fixed because the fix —
//! merging at key granularity while leaving comment lines in place — would mean
//! deciding where an orphaned comment belongs after the key it described is gone,
//! and there is no answer that is right for every file. See the note in
//! `dscode-review/` for the worked example.

use toml_edit::{Document, Item, Table, Value};

/// Overlay `layer` onto `base`, in place, preserving comments and unknown keys.
pub fn merge_document(base: &mut Document, layer: &Document) {
    merge_table(base.as_table_mut(), layer.as_table(), true);
}

/// Apply `layer` (TOML text) over `base` (TOML text), returning the merged text.
///
/// A convenience wrapper around parse + [`merge_document`] for callers that hold
/// both sides as text. Prefer the document form when you need to edit the result
/// further (e.g. to strip a nested key) — round-tripping through a string just to
/// re-parse it loses nothing but is easy to get wrong.
pub fn overlay(base: &str, layer: &str) -> Result<String, toml_edit::TomlError> {
    let mut doc: Document = base.parse()?;
    let patch: Document = layer.parse()?;
    merge_document(&mut doc, &patch);
    Ok(doc.to_string())
}

fn merge_table(base: &mut Table, layer: &Table, is_root: bool) {
    // 1. Remove keys this table is authoritative over but the layer omits.
    //    Skipped at the root — see the module docs.
    if !is_root {
        let drop: Vec<String> = base
            .iter()
            .map(|(k, _)| k.to_string())
            .filter(|k| layer.get(k).is_none())
            .collect();
        for k in drop {
            base.remove(&k);
        }
    }

    // 2. Write every key the layer declares.
    for (key, litem) in layer.iter() {
        match litem {
            // Recurse when both sides are plain tables. An array-of-tables or an
            // inline table is treated as a value: replaced, not merged.
            Item::Table(ltable) if matches!(base.get(key), Some(Item::Table(_))) => {
                if let Some(Item::Table(btable)) = base.get_mut(key) {
                    merge_table(btable, ltable, false);
                }
            }
            Item::Table(ltable) => {
                let mut fresh = ltable.clone();
                // A nested table's own decor sits on its header line; carry the
                // base's over when we are replacing one wholesale.
                if let Some(Item::Table(old)) = base.get(key) {
                    *fresh.decor_mut() = old.decor().clone();
                }
                base.insert(key, Item::Table(fresh));
            }
            Item::Value(lvalue) => {
                if let Some(Item::Value(bvalue)) = base.get_mut(key) {
                    // Keep the base value's decor — this is the comment/spacing
                    // on the key's line. Replacing the Item outright would drop it.
                    let mut fresh = lvalue.clone();
                    *fresh.decor_mut() = bvalue.decor().clone();
                    *bvalue = fresh;
                } else {
                    base.insert(key, Item::Value(lvalue.clone()));
                }
            }
            Item::None => {}
            // Array-of-tables (`[[extensions.mcp_servers]]`) — replaced whole.
            other => {
                base.insert(key, other.clone());
            }
        }
    }
}

/// Read a scalar out of a document, for the one-shot migrations that used to
/// live inline in `Config::load`.
pub fn get_str(doc: &Document, path: &[&str]) -> Option<String> {
    let mut item: &Item = doc.as_item();
    for seg in path {
        item = item.get(seg)?;
    }
    item.as_str().map(str::to_string)
}

/// Set a value at `path`, creating intermediate tables as needed.
pub fn set_value(doc: &mut Document, path: &[&str], value: Value) {
    let (last, parents) = match path.split_last() {
        Some(x) => x,
        None => return,
    };
    let mut table = doc.as_table_mut();
    for seg in parents {
        if table.get(seg).map(|i| !i.is_table()).unwrap_or(false) {
            table.insert(seg, Item::Table(Table::new()));
        }
        let entry = table
            .entry(seg)
            .or_insert(Item::Table(Table::new()));
        match entry.as_table_mut() {
            Some(t) => table = t,
            None => return,
        }
    }
    table.insert(last, Item::Value(value));
}

/// Remove `path` if present, returning whether anything was removed.
pub fn remove_path(doc: &mut Document, path: &[&str]) -> bool {
    let (last, parents) = match path.split_last() {
        Some(x) => x,
        None => return false,
    };
    let mut table = doc.as_table_mut();
    for seg in parents {
        match table.get_mut(seg).and_then(|i| i.as_table_mut()) {
            Some(t) => table = t,
            None => return false,
        }
    }
    table.remove(last).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"# DS Code config — hand edited
default_model = "deepseek-v4-pro"

# keep this comment
[providers.deepseek]
api_key = "sk-old"
base_url = "https://api.deepseek.com/v1"
enabled_models = ["a", "b"]

[some_future_section]
tuning = 7
"#;

    /// The headline guarantee: a key this build does not know about, sitting in
    /// a section it *does* know about, survives a write.
    #[test]
    fn unknown_nested_key_survives() {
        let layer = r#"
default_model = "deepseek-v4-pro"

[providers.deepseek]
api_key = "sk-old"
base_url = "https://api.deepseek.com/v1"
enabled_models = ["a", "b"]
api_format = "responses"
"#;
        let out = overlay(BASE, layer).unwrap();
        assert!(out.contains("api_format = \"responses\""), "{out}");
        // ...and so does a section it knows nothing about at all.
        assert!(out.contains("[some_future_section]"), "{out}");
        assert!(out.contains("tuning = 7"), "{out}");
    }

    #[test]
    fn comments_survive_an_update() {
        let layer = r#"
default_model = "kimi-k2"

[providers.deepseek]
api_key = "sk-new"
base_url = "https://api.deepseek.com/v1"
enabled_models = ["a", "b"]
"#;
        let out = overlay(BASE, layer).unwrap();
        assert!(out.contains("# DS Code config — hand edited"), "{out}");
        assert!(out.contains("# keep this comment"), "{out}");
        assert!(out.contains("api_key = \"sk-new\""), "{out}");
        assert!(!out.contains("sk-old"), "{out}");
    }

    /// `None` must mean *gone*, not *unchanged* — otherwise a user can clear the
    /// model whitelist and have the old one silently come back.
    #[test]
    fn absent_nested_key_is_removed() {
        let layer = r#"
default_model = "deepseek-v4-pro"

[providers.deepseek]
api_key = "sk-old"
base_url = "https://api.deepseek.com/v1"
"#;
        let out = overlay(BASE, layer).unwrap();
        assert!(!out.contains("enabled_models"), "{out}");
    }

    /// A root key the layer omits is kept — root is add/update only.
    #[test]
    fn root_key_is_not_pruned() {
        let layer = r#"
default_model = "deepseek-v4-pro"
"#;
        let out = overlay(BASE, layer).unwrap();
        assert!(out.contains("[providers.deepseek]"), "{out}");
        assert!(out.contains("[some_future_section]"), "{out}");
    }

    #[test]
    fn malformed_base_is_an_error_not_a_clobber() {
        assert!(overlay("this is not = = toml", "default_model = \"x\"").is_err());
    }

    #[test]
    fn set_and_remove_path() {
        let mut doc: Document = BASE.parse().unwrap();
        set_value(&mut doc, &["proxy", "url"], Value::from("http://127.0.0.1:7890"));
        assert_eq!(
            get_str(&doc, &["proxy", "url"]).as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert!(remove_path(&mut doc, &["providers", "deepseek", "enabled_models"]));
        assert!(!remove_path(&mut doc, &["providers", "deepseek", "enabled_models"]));
    }
}
