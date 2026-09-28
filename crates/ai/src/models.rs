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

    #[test]
    fn repeated_model_cursor_is_an_explicit_error() {
        let error = fetch_chatgpt_models(|_, _| Ok(json!({"data": [], "nextCursor": "repeat"})))
            .unwrap_err();
        assert_eq!(error, ModelListError::CursorLoop);
    }
}
