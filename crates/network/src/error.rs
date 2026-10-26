use std::error::Error;
use std::fmt;

use crate::credential::CredentialError;

/// Where a rejected proxy value came from. Never contains the value itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxySource {
    Manual,
    /// Name of the environment variable that supplied the value.
    Environment(&'static str),
}

/// Why a proxy URL was rejected. Deliberately carries no input text because
/// the URL may contain credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlProblem {
    Empty,
    MissingScheme,
    /// https, socks, or any other scheme than `http`.
    UnsupportedScheme,
    /// Manual proxy URLs must not embed userinfo.
    UserinfoNotAllowed,
    InvalidUserinfo,
    ControlCharacter,
    PathNotAllowed,
    QueryNotAllowed,
    FragmentNotAllowed,
    EmptyHost,
    InvalidHost,
    InvalidPort,
}

/// Why a bypass (no_proxy) list was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BypassProblem {
    /// Wildcard other than a lone `*`.
    Wildcard,
    Cidr,
    UrlForm,
    /// DNS name or address with a port.
    PortNotAllowed,
    Whitespace,
    InvalidHost,
    InvalidAddress,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkError {
    InvalidProxyUrl {
        source: ProxySource,
        problem: UrlProblem,
    },
    InvalidBypass {
        source: ProxySource,
        problem: BypassProblem,
    },
    InvalidDestination,
    InvalidCredentialRef,
    /// The stored settings use a schema this build does not understand.
    UnsupportedSchema(u32),
    /// Manual mode is selected but no endpoint is stored.
    ManualNotConfigured,
    /// The credential could not be obtained; never degrades to Direct or
    /// unauthenticated connection.
    Credential(CredentialError),
}

impl fmt::Display for ProxySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manual => write!(f, "手動設定"),
            Self::Environment(name) => write!(f, "環境変数 {name}"),
        }
    }
}

impl fmt::Display for UrlProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Empty => "値が空です。http://host:port の形式で指定してください",
            Self::MissingScheme => {
                "スキームがありません。http://host:port の形式で指定してください"
            }
            Self::UnsupportedScheme => {
                "未対応のスキームです。http:// で始まるプロキシURLを指定してください"
            }
            Self::UserinfoNotAllowed => {
                "URLにユーザー名やパスワードを含めることはできません。資格情報は専用の項目で設定してください"
            }
            Self::InvalidUserinfo => "URLのユーザー情報の形式が不正です",
            Self::ControlCharacter => "URLに制御文字が含まれています",
            Self::PathNotAllowed => "URLにパスを含めることはできません。host:port のみを指定してください",
            Self::QueryNotAllowed => "URLにクエリを含めることはできません。host:port のみを指定してください",
            Self::FragmentNotAllowed => {
                "URLにフラグメントを含めることはできません。host:port のみを指定してください"
            }
            Self::EmptyHost => "ホスト名が空です",
            Self::InvalidHost => "ホスト名またはIPアドレスの形式が不正です",
            Self::InvalidPort => "ポート番号が不正です。1〜65535の数値で指定してください",
        };
        f.write_str(text)
    }
}

impl fmt::Display for BypassProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Wildcard => {
                "ワイルドカードは単独の * のみ使用できます。部分的な * は使わず、ドメイン名で指定してください"
            }
            Self::Cidr => "CIDR表記（192.0.2.0/24 など）は未対応です。個別のホストやドメインで指定してください",
            Self::UrlForm => "URL形式は使用できません。スキームやパスを除いたホスト名で指定してください",
            Self::PortNotAllowed => "ポート番号付きの除外先は未対応です。ポートを除いて指定してください",
            Self::Whitespace => "除外先に空白が含まれています。カンマ区切りで空白なしに指定してください",
            Self::InvalidHost => "除外先のホスト名の形式が不正です",
            Self::InvalidAddress => "除外先のIPアドレスの形式が不正です",
        };
        f.write_str(text)
    }
}

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProxyUrl { source, problem } => {
                write!(f, "プロキシ設定が不正です（{source}: {problem}）")
            }
            Self::InvalidBypass { source, problem } => {
                write!(f, "プロキシ除外先が不正です（{source}: {problem}）")
            }
            Self::InvalidDestination => write!(f, "接続先ホストが不正です"),
            Self::InvalidCredentialRef => write!(f, "資格情報の参照IDが不正です"),
            Self::UnsupportedSchema(v) => write!(f, "未対応のネットワーク設定schemaです（{v}）"),
            Self::ManualNotConfigured => write!(f, "手動プロキシが設定されていません"),
            Self::Credential(e) => write!(f, "プロキシ資格情報を取得できません（{e}）"),
        }
    }
}

impl Error for NetworkError {}

#[cfg(test)]
mod tests {
    use super::*;

    const URL_PROBLEMS: [UrlProblem; 12] = [
        UrlProblem::Empty,
        UrlProblem::MissingScheme,
        UrlProblem::UnsupportedScheme,
        UrlProblem::UserinfoNotAllowed,
        UrlProblem::InvalidUserinfo,
        UrlProblem::ControlCharacter,
        UrlProblem::PathNotAllowed,
        UrlProblem::QueryNotAllowed,
        UrlProblem::FragmentNotAllowed,
        UrlProblem::EmptyHost,
        UrlProblem::InvalidHost,
        UrlProblem::InvalidPort,
    ];

    const BYPASS_PROBLEMS: [BypassProblem; 7] = [
        BypassProblem::Wildcard,
        BypassProblem::Cidr,
        BypassProblem::UrlForm,
        BypassProblem::PortNotAllowed,
        BypassProblem::Whitespace,
        BypassProblem::InvalidHost,
        BypassProblem::InvalidAddress,
    ];

    #[test]
    fn proxy_source_displays_japanese_and_keeps_variable_name() {
        assert_eq!(ProxySource::Manual.to_string(), "手動設定");
        assert_eq!(
            ProxySource::Environment("http_proxy").to_string(),
            "環境変数 http_proxy"
        );
    }

    #[test]
    fn every_problem_has_a_distinct_japanese_message() {
        let mut seen = Vec::new();
        for problem in URL_PROBLEMS {
            let text = problem.to_string();
            assert!(!text.is_empty());
            assert_ne!(text, format!("{problem:?}"));
            assert!(!seen.contains(&text));
            seen.push(text);
        }
        let mut seen = Vec::new();
        for problem in BYPASS_PROBLEMS {
            let text = problem.to_string();
            assert!(!text.is_empty());
            assert_ne!(text, format!("{problem:?}"));
            assert!(!seen.contains(&text));
            seen.push(text);
        }
    }

    #[test]
    fn network_error_display_does_not_use_debug_names() {
        for problem in URL_PROBLEMS {
            for source in [ProxySource::Manual, ProxySource::Environment("http_proxy")] {
                let text = NetworkError::InvalidProxyUrl { source, problem }.to_string();
                assert!(!text.contains(&format!("{problem:?}")), "{text}");
                assert!(!text.contains("Environment("), "{text}");
                assert!(!text.contains("Manual"), "{text}");
            }
        }
        for problem in BYPASS_PROBLEMS {
            let text = NetworkError::InvalidBypass {
                source: ProxySource::Environment("no_proxy"),
                problem,
            }
            .to_string();
            assert!(!text.contains(&format!("{problem:?}")), "{text}");
            assert!(text.contains("環境変数 no_proxy"), "{text}");
        }
    }

    #[test]
    fn error_text_and_debug_do_not_echo_input_values() {
        // Error variants carry no input text, so a secret can never surface.
        let secret = "s3cret-token";
        let err = NetworkError::InvalidProxyUrl {
            source: ProxySource::Manual,
            problem: UrlProblem::UserinfoNotAllowed,
        };
        assert!(!err.to_string().contains(secret));
        assert!(!format!("{err:?}").contains(secret));
    }
}
