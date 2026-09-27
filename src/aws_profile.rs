use std::env::{self, VarError};
use std::fmt;

use anyhow::{Result, bail};

/// AWS のクレデンシャルを解決するプロファイルと、それをどこから取ったか
#[derive(Debug, PartialEq, Eq)]
pub enum AwsProfile {
    /// 環境変数 `AWS_PROFILE`
    Env(String),
    /// 設定ファイルの `aws_profile`
    Config(String),
    /// どちらも無いので、SDK の既定のクレデンシャルチェーンに任せる
    Default,
}

impl AwsProfile {
    pub fn resolve(configured: Option<&str>) -> Result<Self> {
        let from_env = match env::var("AWS_PROFILE") {
            Ok(name) => Some(name),
            Err(VarError::NotPresent) => None,
            Err(VarError::NotUnicode(_)) => bail!("環境変数 AWS_PROFILE が UTF-8 ではありません"),
        };
        Ok(Self::resolve_from(from_env, configured))
    }

    fn resolve_from(from_env: Option<String>, configured: Option<&str>) -> Self {
        from_env
            .filter(|name| !name.is_empty())
            .map(Self::Env)
            .or_else(|| configured.map(|name| Self::Config(name.to_owned())))
            .unwrap_or(Self::Default)
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Env(name) | Self::Config(name) => Some(name),
            Self::Default => None,
        }
    }
}

impl fmt::Display for AwsProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(name) => write!(f, "{name}（環境変数 AWS_PROFILE）"),
            Self::Config(name) => write!(f, "{name}（設定の aws_profile）"),
            Self::Default => write!(f, "(未指定)（SDK の既定のクレデンシャルチェーン）"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_takes_precedence_over_config() {
        assert_eq!(
            AwsProfile::resolve_from(Some("from-env".into()), Some("from-config")),
            AwsProfile::Env("from-env".into())
        );
    }

    #[test]
    fn config_is_used_when_env_is_unset_or_empty() {
        let expected = AwsProfile::Config("from-config".into());

        assert_eq!(
            AwsProfile::resolve_from(None, Some("from-config")),
            expected
        );
        assert_eq!(
            AwsProfile::resolve_from(Some("".into()), Some("from-config")),
            expected
        );
    }

    #[test]
    fn falls_back_to_sdk_default_without_env_and_config() {
        let profile = AwsProfile::resolve_from(None, None);

        assert_eq!(profile, AwsProfile::Default);
        assert_eq!(profile.name(), None);
    }

    #[test]
    fn display_shows_name_and_where_it_came_from() {
        assert_eq!(
            AwsProfile::Env("mozu".into()).to_string(),
            "mozu（環境変数 AWS_PROFILE）"
        );
        assert_eq!(
            AwsProfile::Config("mozu".into()).to_string(),
            "mozu（設定の aws_profile）"
        );
        assert_eq!(
            AwsProfile::Default.to_string(),
            "(未指定)（SDK の既定のクレデンシャルチェーン）"
        );
    }
}
