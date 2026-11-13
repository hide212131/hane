//! Typed projection of the Codex App Server 0.157.1 model catalog.
//!
//! `Model.id` is an internal catalog identifier. Settings and inference use
//! `Model.model`, as required by the versioned App Server contract.

use std::collections::HashSet;

use serde_json::{Value, json};

const PAGE_LIMIT: usize = 100;
const MAX_PAGES: usize = 50;
const MAX_MODELS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptModel {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelListError {
    Rpc,
    InvalidResponse,
    CursorLoop,
    TooManyPages,
    TooManyModels,
}

impl std::fmt::Display for ModelListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc => f.write_str("The model list could not be loaded."),
            Self::InvalidResponse => f.write_str("The model list response was invalid."),
            Self::CursorLoop => f.write_str("The model list repeated a paging cursor."),
            Self::TooManyPages => f.write_str("The model list exceeded the paging limit."),
            Self::TooManyModels => f.write_str("The model list exceeded the supported size."),
        }
    }
}

impl std::error::Error for ModelListError {}

/// Fetches the complete model list with a bounded cursor walk. The callback
/// receives only the fixed method name and schema-shaped parameters.
pub fn fetch_chatgpt_models(
    mut call: impl FnMut(&str, Value) -> Result<Value, ()>,
) -> Result<Vec<ChatGptModel>, ModelListError> {
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    let mut candidates = Vec::new();

    for page_index in 0..MAX_PAGES {
        let mut params = json!({"limit": PAGE_LIMIT, "includeHidden": false});
        if let Some(current) = &cursor {
            params["cursor"] = Value::String(current.clone());
        }
        let response = call("model/list", params).map_err(|_| ModelListError::Rpc)?;
        let data = response
            .get("data")
            .and_then(Value::as_array)
            .ok_or(ModelListError::InvalidResponse)?;

        let total = candidates.len().saturating_add(data.len());
        if total > MAX_MODELS {
            return Err(ModelListError::TooManyModels);
        }
        for item in data {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .ok_or(ModelListError::InvalidResponse)?;
            let model = item
                .get("model")
                .and_then(Value::as_str)
                .ok_or(ModelListError::InvalidResponse)?;
            let display_name = item
                .get("displayName")
                .and_then(Value::as_str)
                .ok_or(ModelListError::InvalidResponse)?;
            let hidden = item
                .get("hidden")
                .and_then(Value::as_bool)
                .ok_or(ModelListError::InvalidResponse)?;
            let text_capable = item
                .get("inputModalities")
                .and_then(Value::as_array)
                .is_some_and(|modalities| {
                    modalities
                        .iter()
                        .any(|value| value.as_str() == Some("text"))
                });
            if !hidden && text_capable {
                candidates.push(ChatGptModel {
                    id: id.to_string(),
                    model: model.to_string(),
                    display_name: display_name.to_string(),
                    is_default: item
                        .get("isDefault")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
        }

        let next = match response.get("nextCursor") {
            Some(Value::Null) | None => None,
            Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
            _ => return Err(ModelListError::InvalidResponse),
        };
        let Some(next_cursor) = next else {
            return Ok(candidates);
        };
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(ModelListError::CursorLoop);
        }
        if page_index + 1 == MAX_PAGES {
            return Err(ModelListError::TooManyPages);
        }
        cursor = Some(next_cursor);
    }

    Err(ModelListError::TooManyPages)
}

pub fn saved_model_is_available(
    saved_model: Option<&str>,
    models: &[ChatGptModel],
) -> Option<bool> {
    saved_model.map(|saved| models.iter().any(|model| model.model == saved))
}

/// The model the automatic connection preparation settles on. Every payload is
/// `Model.model` (never `Model.id`), the value settings and inference use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSelection {
    /// The user's confirmed choice, which is in the catalog.
    UserChoice(String),
    /// The saved model, kept because the catalog still lists it.
    Saved(String),
    /// A model was chosen or saved but the catalog does not list it. It is
    /// never replaced automatically.
    SavedMissing(String),
    /// Nothing was saved; the first catalog entry flagged `is_default`.
    Default(String),
    /// Nothing was saved and nothing is flagged default; the first candidate
    /// of the fully fetched catalog.
    FirstCandidate(String),
    /// Nothing was saved and the catalog is empty.
    NoCandidates,
}

fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

/// Decides the ChatGPT model. Priority: the user's confirmed choice, then the
/// saved model (when listed), then the first `is_default` candidate, then the
/// first candidate of the whole catalog. A saved model the catalog no longer
/// lists is reported, not replaced. `models` is the complete, already filtered
/// catalog from [`fetch_chatgpt_models`]; no display limit applies here.
pub fn select_chatgpt_model(
    user_choice: Option<&str>,
    saved_model: Option<&str>,
    models: &[ChatGptModel],
) -> ModelSelection {
    let listed = |candidate: &str| models.iter().any(|model| model.model == candidate);
    if let Some(choice) = present(user_choice) {
        return if listed(choice) {
            ModelSelection::UserChoice(choice.to_owned())
        } else {
            ModelSelection::SavedMissing(choice.to_owned())
        };
    }
    if let Some(saved) = present(saved_model) {
        return if listed(saved) {
            ModelSelection::Saved(saved.to_owned())
        } else {
            ModelSelection::SavedMissing(saved.to_owned())
        };
    }
    if let Some(default) = models.iter().find(|model| model.is_default) {
        return ModelSelection::Default(default.model.clone());
    }
    match models.first() {
        Some(first) => ModelSelection::FirstCandidate(first.model.clone()),
        None => ModelSelection::NoCandidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn model(id: &str, model: &str, hidden: bool, modalities: &[&str]) -> Value {
        json!({
            "id": id,
            "model": model,
            "displayName": format!("Display {model}"),
            "hidden": hidden,
            "inputModalities": modalities,
            "isDefault": false
        })
    }

    #[test]
    fn model_paging_uses_the_schema_and_projects_model_not_id() {
        let calls = Mutex::new(Vec::new());
        let models = fetch_chatgpt_models(|method, params| {
            assert_eq!(method, "model/list");
            calls.lock().unwrap().push(params.clone());
            Ok(if calls.lock().unwrap().len() == 1 {
                json!({
                    "data": [
                        model("catalog-7", "gpt-text", false, &["text", "image"]),
                        model("catalog-hidden", "gpt-hidden", true, &["text"]),
                        model("catalog-audio", "gpt-audio", false, &["audio"])
                    ],
                    "nextCursor": "page-2"
                })
            } else {
                json!({"data": [model("catalog-8", "gpt-next", false, &["text"])], "nextCursor": null})
            })
        })
        .unwrap();

        assert_eq!(
            models
                .iter()
                .map(|model| model.model.as_str())
                .collect::<Vec<_>>(),
            ["gpt-text", "gpt-next"]
        );
        assert_eq!(models[0].id, "catalog-7");
        assert_eq!(
            saved_model_is_available(Some("gpt-text"), &models),
            Some(true)
        );
        assert_eq!(
            saved_model_is_available(Some("catalog-7"), &models),
            Some(false)
        );
        assert_eq!(
            saved_model_is_available(Some("removed-model"), &models),
            Some(false)
        );
        let calls = calls.into_inner().unwrap();
        assert_eq!(calls[0]["includeHidden"], false);
        assert_eq!(calls[0]["limit"], PAGE_LIMIT);
        assert_eq!(calls[1]["cursor"], "page-2");
    }

    fn candidate(id: &str, model: &str, is_default: bool) -> ChatGptModel {
        ChatGptModel {
            id: id.to_owned(),
            model: model.to_owned(),
            display_name: format!("Display {model}"),
            is_default,
        }
    }

    #[test]
    fn saved_model_takes_precedence_over_default() {
        let models = [
            candidate("c1", "gpt-first", false),
            candidate("c2", "gpt-default", true),
        ];
        assert_eq!(
            select_chatgpt_model(None, Some("gpt-first"), &models),
            ModelSelection::Saved("gpt-first".to_owned())
        );
        assert_eq!(
            select_chatgpt_model(Some("gpt-default"), Some("gpt-first"), &models),
            ModelSelection::UserChoice("gpt-default".to_owned()),
            "the user's confirmed choice outranks the saved model"
        );
    }

    #[test]
    fn missing_saved_model_is_not_replaced() {
        let models = [candidate("c1", "gpt-default", true)];
        assert_eq!(
            select_chatgpt_model(None, Some("removed-model"), &models),
            ModelSelection::SavedMissing("removed-model".to_owned())
        );
        assert_eq!(
            select_chatgpt_model(None, Some("c1"), &models),
            ModelSelection::SavedMissing("c1".to_owned()),
            "a catalog id is not a model value"
        );
    }

    #[test]
    fn empty_selection_uses_default_then_first_candidate() {
        let with_defaults = [
            candidate("c1", "gpt-first", false),
            candidate("c2", "gpt-default-a", true),
            candidate("c3", "gpt-default-b", true),
        ];
        assert_eq!(
            select_chatgpt_model(None, None, &with_defaults),
            ModelSelection::Default("gpt-default-a".to_owned())
        );
        assert_eq!(
            select_chatgpt_model(None, Some("  "), &with_defaults),
            ModelSelection::Default("gpt-default-a".to_owned()),
            "a blank saved value counts as no saved model"
        );
        let without_defaults = [
            candidate("c1", "gpt-first", false),
            candidate("c2", "gpt-second", false),
        ];
        assert_eq!(
            select_chatgpt_model(None, None, &without_defaults),
            ModelSelection::FirstCandidate("gpt-first".to_owned())
        );
    }

    #[test]
    fn empty_or_failed_catalog_has_no_automatic_selection() {
        assert_eq!(
            select_chatgpt_model(None, None, &[]),
            ModelSelection::NoCandidates
        );
        let failed = fetch_chatgpt_models(|_, _| Err(()));
        assert_eq!(failed.unwrap_err(), ModelListError::Rpc);
    }

    #[test]
    fn selection_uses_model_value_from_all_pages() {
        let calls = Mutex::new(0usize);
        let models = fetch_chatgpt_models(|_, _| {
            let mut calls = calls.lock().unwrap();
            *calls += 1;
            Ok(if *calls == 1 {
                let data = (0..45)
                    .map(|index| {
                        model(
                            &format!("catalog-{index}"),
                            &format!("gpt-{index}"),
                            false,
                            &["text"],
                        )
                    })
                    .collect::<Vec<_>>();
                json!({"data": data, "nextCursor": "page-2"})
            } else {
                json!({
                    "data": [{
                        "id": "catalog-default",
                        "model": "gpt-default-model",
                        "displayName": "Default",
                        "hidden": false,
                        "inputModalities": ["text"],
                        "isDefault": true
                    }],
                    "nextCursor": null
                })
            })
        })
        .unwrap();

        assert_eq!(models.len(), 46);
        assert_eq!(
            select_chatgpt_model(None, None, &models),
            ModelSelection::Default("gpt-default-model".to_owned()),
            "the default on a later page is found, and its model value is used"
        );
    }

    #[test]
    fn repeated_model_cursor_is_an_explicit_error() {
        let error = fetch_chatgpt_models(|_, _| Ok(json!({"data": [], "nextCursor": "repeat"})))
            .unwrap_err();
        assert_eq!(error, ModelListError::CursorLoop);
    }
}
