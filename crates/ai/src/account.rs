//! Redacted adapters for the Codex App Server 0.157.1 account/OAuth API.

use serde_json::Value;
use url::Url;

const OFFICIAL_OAUTH_HOST: &str = "auth.openai.com";

#[derive(Clone, PartialEq, Eq)]
pub enum AccountState {
    Unknown,
    SignedOut,
    SignedIn {
        email: Option<String>,
        plan_type: String,
    },
    ApiKey,
}

impl std::fmt::Debug for AccountState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("Unknown"),
            Self::SignedOut => f.write_str("SignedOut"),
            Self::SignedIn { email, plan_type } => f
                .debug_struct("SignedIn")
                .field("email", &email.as_ref().map(|_| "<redacted>"))
                .field("plan_type", plan_type)
                .finish(),
            Self::ApiKey => f.write_str("ApiKey"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountError {
    InvalidResponse,
    UnsupportedAccount,
    UnsupportedLogin,
    UntrustedAuthorizationUrl,
    InvalidCancelResponse,
}

impl std::fmt::Display for AccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidResponse => f.write_str("The account response was invalid."),
            Self::UnsupportedAccount => {
                f.write_str("The App Server returned an unsupported account type.")
            }
            Self::UnsupportedLogin => {
                f.write_str("The App Server returned an unsupported login type.")
            }
            Self::UntrustedAuthorizationUrl => {
                f.write_str("The authorization URL did not match the trusted sign-in host.")
            }
            Self::InvalidCancelResponse => {
                f.write_str("The login cancellation response was invalid.")
            }
        }
    }
}

impl std::error::Error for AccountError {}

pub fn account_read_params(refresh_token: bool) -> Value {
    if refresh_token {
        serde_json::json!({"refreshToken": true})
    } else {
        serde_json::json!({})
    }
}

pub fn parse_account_read(response: &Value) -> Result<AccountState, AccountError> {
    let requires_auth = response
        .get("requiresOpenaiAuth")
        .and_then(Value::as_bool)
        .ok_or(AccountError::InvalidResponse)?;
    match response
        .get("account")
        .ok_or(AccountError::InvalidResponse)?
    {
        Value::Null => Ok(if requires_auth {
            AccountState::SignedOut
        } else {
            AccountState::Unknown
        }),
        Value::Object(account) => match account.get("type").and_then(Value::as_str) {
            Some("chatgpt") => Ok(AccountState::SignedIn {
                email: match account.get("email") {
                    Some(Value::String(email)) => Some(email.clone()),
                    Some(Value::Null) => None,
                    _ => return Err(AccountError::InvalidResponse),
                },
                plan_type: account
                    .get("planType")
                    .and_then(Value::as_str)
                    .ok_or(AccountError::InvalidResponse)?
                    .to_string(),
            }),
            Some("apiKey") => Ok(AccountState::ApiKey),
            _ => Err(AccountError::UnsupportedAccount),
        },
        _ => Err(AccountError::InvalidResponse),
    }
}

pub struct AuthorizationUrl(String);

impl AuthorizationUrl {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AuthorizationUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthorizationUrl(<redacted>)")
    }
}

pub struct LoginStart {
    pub(crate) login_id: String,
    pub(crate) authorization_url: AuthorizationUrl,
}

impl std::fmt::Debug for LoginStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginStart")
            .field("login_id", &"<redacted>")
            .field("authorization_url", &self.authorization_url)
            .finish()
    }
}

pub fn parse_chatgpt_login_start(response: &Value) -> Result<LoginStart, AccountError> {
    if response.get("type").and_then(Value::as_str) != Some("chatgpt") {
        return Err(AccountError::UnsupportedLogin);
    }
    let login_id = response
        .get("loginId")
        .and_then(Value::as_str)
        .ok_or(AccountError::InvalidResponse)?;
    if login_id.is_empty() {
        return Err(AccountError::InvalidResponse);
    }
    let raw_url = response
        .get("authUrl")
        .and_then(Value::as_str)
        .ok_or(AccountError::InvalidResponse)?;
    let parsed = Url::parse(raw_url).map_err(|_| AccountError::UntrustedAuthorizationUrl)?;
    let authority = raw_url
        .split_once("://")
        .map(|(_, remainder)| remainder.split(['/', '?', '#']).next().unwrap_or_default())
        .unwrap_or_default();
    let safe = parsed.scheme() == "https"
        && parsed.host_str() == Some(OFFICIAL_OAUTH_HOST)
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && !authority.contains('@')
        && parsed.port_or_known_default() == Some(443)
        && parsed.path() == "/oauth/authorize"
        && parsed.fragment().is_none();
    if !safe {
        return Err(AccountError::UntrustedAuthorizationUrl);
    }
    Ok(LoginStart {
        login_id: login_id.to_string(),
        authorization_url: AuthorizationUrl(parsed.into()),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelStatus {
    Canceled,
    NotFound,
}

pub fn parse_cancel_status(response: &Value) -> Result<CancelStatus, AccountError> {
    match response.get("status").and_then(Value::as_str) {
        Some("canceled") => Ok(CancelStatus::Canceled),
        Some("notFound") => Ok(CancelStatus::NotFound),
        _ => Err(AccountError::InvalidCancelResponse),
    }
}

pub fn completed_login_matches(params: Option<&Value>, login_id: &str) -> Option<bool> {
    let params = params?;
    let completed_id = params.get("loginId")?.as_str()?;
    (completed_id == login_id).then(|| {
        params
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_read_requires_a_positive_signed_out_or_signed_in_response() {
        assert_eq!(
            parse_account_read(&serde_json::json!({"account": null, "requiresOpenaiAuth": true}))
                .unwrap(),
            AccountState::SignedOut
        );
        assert_eq!(
            parse_account_read(&serde_json::json!({
                "account": {"type":"chatgpt", "email":"user@example.test", "planType":"plus"},
                "requiresOpenaiAuth": false
            }))
            .unwrap(),
            AccountState::SignedIn {
                email: Some("user@example.test".to_string()),
                plan_type: "plus".to_string()
            }
        );
        assert_eq!(
            parse_account_read(&serde_json::json!({"account":null, "requiresOpenaiAuth":false}))
                .unwrap(),
            AccountState::Unknown
        );
    }

    #[test]
    fn oauth_url_debug_is_redacted_and_host_validation_is_exact() {
        let login = parse_chatgpt_login_start(&serde_json::json!({
            "type":"chatgpt", "loginId":"attempt-secret", "authUrl":"https://auth.openai.com/oauth/authorize?state=do-not-log"
        }))
        .unwrap();
        let debug = format!("{login:?}");
        assert!(!debug.contains("attempt-secret"));
        assert!(!debug.contains("do-not-log"));
        assert!(matches!(
            parse_chatgpt_login_start(&serde_json::json!({
                "type":"chatgpt", "loginId":"id", "authUrl":"https://auth.openai.com.evil.test/oauth/authorize"
            })),
            Err(AccountError::UntrustedAuthorizationUrl)
        ));
        assert!(matches!(
            parse_chatgpt_login_start(&serde_json::json!({
                "type":"chatgpt", "loginId":"id", "authUrl":"https://user@auth.openai.com/oauth/authorize"
            })),
            Err(AccountError::UntrustedAuthorizationUrl)
        ));
    }

    #[test]
    fn cancellation_and_completed_login_are_correlated_by_id() {
        assert_eq!(
            parse_cancel_status(&serde_json::json!({"status":"notFound"})),
            Ok(CancelStatus::NotFound)
        );
        assert_eq!(
            completed_login_matches(
                Some(&serde_json::json!({"loginId":"right", "success":true})),
                "right"
            ),
            Some(true)
        );
        assert_eq!(
            completed_login_matches(
                Some(&serde_json::json!({"loginId":"other", "success":true})),
                "right"
            ),
            None
        );
        assert_eq!(account_read_params(false), serde_json::json!({}));
        assert_eq!(
            account_read_params(true),
            serde_json::json!({"refreshToken":true})
        );
    }
}
