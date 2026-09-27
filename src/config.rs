use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

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
    resolve_default_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

fn resolve_default_path(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf> {
    let config_dir = match (xdg_config_home, home) {
        (Some(xdg), _) if !xdg.is_empty() => PathBuf::from(xdg),
        (_, Some(home)) if !home.is_empty() => PathBuf::from(home).join(".config"),
        _ => {
            return Err(anyhow!(
                "XDG_CONFIG_HOME も HOME も未設定です。--config で設定ファイルを指定してください"
            ));
        }
    };
    Ok(config_dir.join("ecsh").join("config.toml"))
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

    #[test]
    fn default_path_prefers_xdg_config_home() {
        let path = resolve_default_path(Some("/xdg".into()), Some("/home/me".into())).unwrap();

        assert_eq!(path, PathBuf::from("/xdg/ecsh/config.toml"));
    }

    #[test]
    fn default_path_falls_back_to_home_when_xdg_is_unset_or_empty() {
        let expected = PathBuf::from("/home/me/.config/ecsh/config.toml");

        assert_eq!(
            resolve_default_path(None, Some("/home/me".into())).unwrap(),
            expected
        );
        assert_eq!(
            resolve_default_path(Some("".into()), Some("/home/me".into())).unwrap(),
            expected
        );
    }

    #[test]
    fn default_path_fails_without_xdg_and_home() {
        assert!(resolve_default_path(None, None).is_err());
    }
}
