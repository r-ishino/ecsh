use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::xdg;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub region: String,
    pub cluster: String,
    /// ネットワーク設定のコピー元にするサービス
    pub service: String,
    /// exec で入るコンテナ
    pub container: String,
    /// 環境変数 AWS_PROFILE が無いときに使う AWS プロファイル
    pub aws_profile: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("設定ファイルを読めません: {}", path.display()))?;
        Self::parse(&text)
            .with_context(|| format!("設定ファイルの形式が不正です: {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        if config.profiles.is_empty() {
            bail!("設定ファイルにプロファイルが 1 つもありません");
        }
        Ok(config)
    }

    pub fn profiles(&self) -> impl Iterator<Item = (&str, &Profile)> {
        self.profiles
            .iter()
            .map(|(name, profile)| (name.as_str(), profile))
    }

    pub fn profile(&self, name: &str) -> Result<&Profile> {
        self.profiles.get(name).ok_or_else(|| {
            let known: Vec<&str> = self.profiles.keys().map(String::as_str).collect();
            anyhow!(
                "プロファイル `{name}` が設定ファイルにありません（定義済み: {}）",
                known.join(", ")
            )
        })
    }
}

pub fn default_path() -> Result<PathBuf> {
    let dir = xdg::config_dir().ok_or_else(|| {
        anyhow!("XDG_CONFIG_HOME も HOME も未設定です。--config で設定ファイルを指定してください")
    })?;
    Ok(dir.join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../config.example.toml");

    #[test]
    fn example_config_is_valid() {
        let config = Config::parse(EXAMPLE).unwrap();

        assert_eq!(
            config.profile("staging").unwrap(),
            &Profile {
                region: "us-east-1".into(),
                cluster: "example-staging".into(),
                service: "worker".into(),
                container: "app".into(),
                aws_profile: Some("example".into()),
            }
        );
        assert_eq!(config.profile("production").unwrap().aws_profile, None);
    }

    #[test]
    fn removed_confirm_field_is_rejected_as_unknown() {
        let text = r#"
            [profiles.production]
            region = "us-east-1"
            cluster = "example-production"
            service = "worker"
            container = "app"
            confirm = true
        "#;

        let message = format!("{:#}", Config::parse(text).unwrap_err());

        assert!(message.contains("unknown field `confirm`"), "{message}");
    }

    #[test]
    fn config_without_profiles_is_rejected() {
        let message = Config::parse("[profiles]").unwrap_err().to_string();

        assert!(message.contains("プロファイルが 1 つもありません"));
    }

    #[test]
    fn unknown_profile_error_lists_defined_profiles() {
        let config = Config::parse(EXAMPLE).unwrap();

        let message = config.profile("dev").unwrap_err().to_string();

        assert!(message.contains("`dev`"));
        assert!(message.contains("production, staging"));
    }

    #[test]
    fn missing_field_is_rejected() {
        let text = r#"
            [profiles.staging]
            region = "us-east-1"
            cluster = "example-staging"
            service = "worker"
        "#;

        assert!(Config::parse(text).is_err());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let text = r#"
            [profiles.staging]
            region = "us-east-1"
            cluster = "example-staging"
            service = "worker"
            container = "app"
            contianer = "typo"
        "#;

        assert!(Config::parse(text).is_err());
    }
}
