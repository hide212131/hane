//! UI-independent parsing of a saved AI prompt as a user prompt template
//! (Issue #594).
//!
//! A saved prompt that contains `{{selection}}` and/or `{{document_context}}`
//! is a *template*; any other prompt keeps the legacy meaning ("saved
//! instruction + selected text"). Values are never expanded here or
//! recursively: this module only classifies and validates the template text.

pub const SELECTION_VARIABLE: &str = "{{selection}}";
pub const DOCUMENT_CONTEXT_VARIABLE: &str = "{{document_context}}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptTemplateError {
    /// The named recognized variable appears more than once.
    DuplicateVariable(&'static str),
    /// `{{name}}` looks like a variable but is not one of the recognized ones.
    UnknownVariable(String),
}

impl std::fmt::Display for PromptTemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptTemplateError::DuplicateVariable(variable) => write!(
                f,
                "{variable} は1つのプロンプトに1回だけ使えます。2回目以降を削除してください"
            ),
            PromptTemplateError::UnknownVariable(name) => write!(
                f,
                "{{{{{name}}}}} は使えない変数です。使える変数は {SELECTION_VARIABLE} と {DOCUMENT_CONTEXT_VARIABLE} です"
            ),
        }
    }
}

impl std::error::Error for PromptTemplateError {}

/// Which recognized variables a template form prompt uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptTemplate {
    pub uses_selection: bool,
    pub uses_document_context: bool,
}

fn is_identifier_like(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Classifies `prompt`: `Ok(None)` for the legacy form (no recognized
/// variable, including prompts that merely contain other double braces),
/// `Ok(Some(_))` for a valid template, `Err` for an invalid one.
pub fn parse_prompt_template(prompt: &str) -> Result<Option<PromptTemplate>, PromptTemplateError> {
    let mut selection = 0usize;
    let mut document_context = 0usize;
    let mut first_unknown: Option<String> = None;

    let mut rest = prompt;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let inner = &after[..end];
        if inner.contains('{') {
            // `{{{x}}}`: retry from the next brace so the inner token is seen.
            rest = &rest[start + 1..];
            continue;
        }
        match inner {
            "selection" => selection += 1,
            "document_context" => document_context += 1,
            other => {
                if is_identifier_like(other.trim()) && first_unknown.is_none() {
                    first_unknown = Some(other.trim().to_owned());
                }
            }
        }
        rest = &after[end + 2..];
    }

    if selection == 0 && document_context == 0 {
        return Ok(None);
    }
    if selection > 1 {
        return Err(PromptTemplateError::DuplicateVariable(SELECTION_VARIABLE));
    }
    if document_context > 1 {
        return Err(PromptTemplateError::DuplicateVariable(
            DOCUMENT_CONTEXT_VARIABLE,
        ));
    }
    if let Some(name) = first_unknown {
        return Err(PromptTemplateError::UnknownVariable(name));
    }
    Ok(Some(PromptTemplate {
        uses_selection: selection == 1,
        uses_document_context: document_context == 1,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T01: legacy / template classification.
    #[test]
    fn prompts_without_recognized_variables_are_legacy() {
        assert_eq!(parse_prompt_template("校正する"), Ok(None));
        assert_eq!(parse_prompt_template("{{foo}} をそのまま"), Ok(None));
        assert_eq!(parse_prompt_template("{{ selection }}"), Ok(None));
    }

    #[test]
    fn recognized_variables_make_a_template() {
        assert_eq!(
            parse_prompt_template("a {{selection}} b"),
            Ok(Some(PromptTemplate {
                uses_selection: true,
                uses_document_context: false
            }))
        );
        assert_eq!(
            parse_prompt_template("{{document_context}}\n{{selection}}"),
            Ok(Some(PromptTemplate {
                uses_selection: true,
                uses_document_context: true
            }))
        );
        assert_eq!(
            parse_prompt_template("{{{selection}}}"),
            Ok(Some(PromptTemplate {
                uses_selection: true,
                uses_document_context: false
            }))
        );
    }

    #[test]
    fn duplicate_recognized_variables_are_rejected() {
        assert_eq!(
            parse_prompt_template("{{selection}} {{selection}}"),
            Err(PromptTemplateError::DuplicateVariable(SELECTION_VARIABLE))
        );
        assert_eq!(
            parse_prompt_template("{{document_context}}{{document_context}}"),
            Err(PromptTemplateError::DuplicateVariable(
                DOCUMENT_CONTEXT_VARIABLE
            ))
        );
    }

    #[test]
    fn unknown_variables_in_a_template_are_rejected_with_their_name() {
        let error = parse_prompt_template("{{selection}} {{foo}}").unwrap_err();
        assert_eq!(error, PromptTemplateError::UnknownVariable("foo".to_owned()));
        assert!(error.to_string().contains("{{foo}}"));
        assert!(matches!(
            parse_prompt_template("{{ selection }} {{selection}}"),
            Err(PromptTemplateError::UnknownVariable(_))
        ));
    }
}
