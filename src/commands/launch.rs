use std::env::{self, VarError};

use anyhow::{Result, bail};
use aws_sdk_ecs::Client;
use aws_sdk_ecs::types::{AssignPublicIp, AwsVpcConfiguration};

use crate::aws_error;
use crate::aws_profile::AwsProfile;
use crate::config::Profile;
use crate::ecs::{self, ServiceSnapshot};
use crate::prompt;
use crate::report::report;
use crate::ui::{self, Style};

/// 起動前の y/N まで済ませた、RunTask に要るもの
pub struct Prepared {
    pub aws_profile: AwsProfile,
    pub client: Client,
    pub snapshot: ServiceSnapshot,
    pub started_by: String,
}

/// 向かう先を出し、サービスから起動の設定を写して、起動前の y/N を聞く。exec と run で共通
///
/// `detail` は向かう先の次の行に出す（run のコマンドなど）
pub async fn prepare(
    name: &str,
    profile: &Profile,
    yes: bool,
    detail: Option<String>,
) -> Result<Prepared> {
    let style = Style::current();
    report!();
    report!(
        "  {}  →  {} / {} / {}",
        style.bold(name),
        profile.cluster,
        profile.service,
        profile.container
    );
    if let Some(detail) = detail {
        report!("  {detail}");
    }

    let aws_profile = AwsProfile::resolve(profile.aws_profile.as_deref())?;
    report!("  AWS  {aws_profile} · {}", profile.region);
    let started_by = ecs::started_by(&current_user()?)?;

    let client = ecs::client(&profile.region, &aws_profile).await;
    let snapshot = ecs::describe_service(&client, &profile.cluster, &profile.service)
        .await
        .map_err(|error| aws_error::explain(error, &aws_profile))?;
    report!("{}", style.note(network_line(&snapshot.network)));
    report!();
    prompt::confirm_launch(name, profile, yes)?;
    Ok(Prepared {
        aws_profile,
        client,
        snapshot,
        started_by,
    })
}

/// `ネットワーク  subnet-01234567…, subnet-89abcdef… · sg-01234567… · パブリック IP なし`
fn network_line(network: &AwsVpcConfiguration) -> String {
    let ids = |ids: &[String]| {
        ids.iter()
            .map(|id| ui::short_resource_id(id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let public_ip = match network.assign_public_ip() {
        Some(AssignPublicIp::Enabled) => "あり",
        Some(AssignPublicIp::Disabled) => "なし",
        _ => "(未指定)",
    };
    format!(
        "ネットワーク  {} · {} · パブリック IP {public_ip}",
        ids(network.subnets()),
        ids(network.security_groups())
    )
}

pub(super) fn current_user() -> Result<String> {
    match env::var("USER") {
        Ok(user) => Ok(user),
        Err(VarError::NotPresent) => bail!("環境変数 USER が設定されていません"),
        Err(VarError::NotUnicode(_)) => bail!("環境変数 USER が UTF-8 ではありません"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_line_shows_shortened_subnets_security_groups_and_public_ip() {
        let network = AwsVpcConfiguration::builder()
            .subnets("subnet-0123456789abcdef0")
            .subnets("subnet-89abcdef012345678")
            .security_groups("sg-0123456789abcdef0")
            .assign_public_ip(AssignPublicIp::Disabled)
            .build()
            .unwrap();

        assert_eq!(
            network_line(&network),
            "ネットワーク  subnet-01234567…, subnet-89abcdef… · sg-01234567… · パブリック IP なし"
        );
    }

    #[test]
    fn network_line_says_unspecified_when_public_ip_is_not_set() {
        let network = AwsVpcConfiguration::builder()
            .subnets("subnet-0123")
            .build()
            .unwrap();

        assert!(network_line(&network).ends_with("パブリック IP (未指定)"));
    }
}
