//! AWS を呼んだときの失敗を分類し、手元で直せるものには対処を添える

use std::error::Error;

use aws_credential_types::provider::error::CredentialsError;
use aws_sdk_sso::operation::get_role_credentials::GetRoleCredentialsError;

use crate::aws_profile::AwsProfile;
use crate::ui::Guidance;

/// SSO のトークンが無い・切れているせいで失敗したなら、`aws sso login` の案内を重ねる。それ以外はそのまま返す
pub fn explain(error: anyhow::Error, aws_profile: &AwsProfile) -> anyhow::Error {
    if is_sso_session_expired(&error) {
        Guidance::new(
            "AWS の SSO セッションが切れています",
            sso_login_remedy(aws_profile.name()),
        )
        .wrap(error)
    } else {
        error
    }
}

/// 認証情報の読み込み（CredentialsError）より下で、SSO のトークンが原因の失敗が起きているか
fn is_sso_session_expired(error: &anyhow::Error) -> bool {
    error
        .chain()
        .skip_while(|cause| !cause.is::<CredentialsError>())
        .any(is_sso_token_failure)
}

/// aws-config の SSO のトークン読み込みのエラー型は非公開で downcast できないので、表示文字列で見分ける
const SSO_TOKEN_FAILURES: [&str; 2] = [
    "the SSO token has expired and cannot be refreshed",
    "failed to load the cached SSO token",
];

fn is_sso_token_failure(cause: &(dyn Error + 'static)) -> bool {
    if let Some(error) = cause.downcast_ref::<GetRoleCredentialsError>() {
        return error.is_unauthorized_exception();
    }
    let message = cause.to_string();
    SSO_TOKEN_FAILURES.contains(&message.as_str()) || is_missing_legacy_token_cache(&message)
}

/// `sso_session` を使わない旧形式のプロファイルは、トークンのキャッシュファイルを直接読み、無ければ「failed to read `<path>`」で落ちる
fn is_missing_legacy_token_cache(message: &str) -> bool {
    message.starts_with("failed to read `") && message.contains("/.aws/sso/cache/")
}

/// プロファイル名が無い（SDK の既定に任せている）ときは `--profile` を付けない
fn sso_login_remedy(aws_profile: Option<&str>) -> String {
    let command = match aws_profile {
        Some(name) => format!("aws sso login --profile {name}"),
        None => "aws sso login".to_owned(),
    };
    format!("`{command}` を実行してから、もう一度実行してください")
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, anyhow};
    use aws_sdk_ecs::config::http::HttpResponse;
    use aws_sdk_ecs::error::{ConnectorError, SdkError};
    use aws_sdk_ecs::operation::describe_services::DescribeServicesError;
    use aws_sdk_sso::types::error::UnauthorizedException;

    use super::*;
    use crate::ui::Style;

    /// ECS の呼び出しが、認証情報の読み込みで `cause` が起きて送れなかったときのエラー
    fn failed_loading_credentials(cause: impl Into<Box<dyn Error + Send + Sync>>) -> anyhow::Error {
        let credentials = CredentialsError::provider_error(CredentialsError::provider_error(cause));
        let sdk_error = SdkError::<DescribeServicesError, HttpResponse>::dispatch_failure(
            ConnectorError::other(credentials.into(), None),
        );
        Err::<(), _>(sdk_error)
            .context("DescribeServices に失敗しました（cluster=c service=s）")
            .unwrap_err()
    }

    fn sso_rejected_the_token() -> anyhow::Error {
        let rejected = GetRoleCredentialsError::UnauthorizedException(
            UnauthorizedException::builder()
                .message("Session token not found or invalid")
                .build(),
        );
        failed_loading_credentials(SdkError::<_, ()>::service_error(rejected, ()))
    }

    #[test]
    fn token_rejected_by_sso_while_loading_credentials_is_an_expired_session() {
        assert!(is_sso_session_expired(&sso_rejected_the_token()));
    }

    #[test]
    fn expired_or_missing_cached_token_is_an_expired_session() {
        for cause in [
            "the SSO token has expired and cannot be refreshed",
            "failed to load the cached SSO token",
            "failed to read `/home/me/.aws/sso/cache/0123456789abcdef.json`",
        ] {
            assert!(
                is_sso_session_expired(&failed_loading_credentials(anyhow!(cause))),
                "{cause}"
            );
        }
    }

    #[test]
    fn other_credential_failures_are_not_an_expired_session() {
        for cause in [
            "no providers in chain provided credentials",
            "The security token included in the request is expired",
            "failed to read `/home/me/.aws/credentials`",
        ] {
            assert!(
                !is_sso_session_expired(&failed_loading_credentials(anyhow!(cause))),
                "{cause}"
            );
        }
    }

    #[test]
    fn sso_like_message_outside_credential_loading_is_not_an_expired_session() {
        let error =
            anyhow!("failed to load the cached SSO token").context("RunTask に失敗しました");

        assert!(!is_sso_session_expired(&error));
    }

    #[test]
    fn expired_session_shows_sso_login_for_the_resolved_profile_above_the_original_causes() {
        let error = explain(sso_rejected_the_token(), &AwsProfile::Config("dev".into()));

        assert_eq!(
            Style::PLAIN.error_lines(&error),
            [
                "✗ AWS の SSO セッションが切れています",
                "  `aws sso login --profile dev` を実行してから、もう一度実行してください",
                "  DescribeServices に失敗しました（cluster=c service=s）",
                "  dispatch failure",
                "  other",
                "  an error occurred while loading credentials",
                "  an error occurred while loading credentials",
                "  service error",
                "  UnauthorizedException: Session token not found or invalid",
                "  UnauthorizedException: Session token not found or invalid",
            ]
        );
    }

    #[test]
    fn other_errors_are_returned_unchanged() {
        let error = explain(anyhow!("RunTask に失敗しました"), &AwsProfile::Default);

        assert_eq!(
            Style::PLAIN.error_lines(&error),
            ["✗ RunTask に失敗しました"]
        );
    }

    #[test]
    fn sso_login_names_the_profile_only_when_one_was_resolved() {
        assert_eq!(
            sso_login_remedy(Some("dev")),
            "`aws sso login --profile dev` を実行してから、もう一度実行してください"
        );
        assert_eq!(
            sso_login_remedy(None),
            "`aws sso login` を実行してから、もう一度実行してください"
        );
    }
}
