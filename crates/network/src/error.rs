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

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProxyUrl { source, problem } => {
                write!(f, "プロキシ設定が不正です（{source:?}: {problem:?}）")
            }
            Self::InvalidBypass { source, problem } => {
                write!(f, "プロキシ除外先が不正です（{source:?}: {problem:?}）")
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
