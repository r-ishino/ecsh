use std::env;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use anyhow::{Context, Result, anyhow};
use aws_sdk_ecs::Client;
use aws_sdk_ecs::config::endpoint::{DefaultResolver, Params, ResolveEndpoint};
use aws_sdk_ecs::operation::execute_command::ExecuteCommandOutput;
use aws_sdk_ecs::operation::execute_command::builders::ExecuteCommandFluentBuilder;
use serde_json::json;
use tokio::process::{Child, Command};

use crate::ui::Guidance;

const PLUGIN: &str = "session-manager-plugin";

/// PATH から session-manager-plugin を探す
pub fn find_plugin() -> Result<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    find_in(&path).ok_or_else(|| {
        Guidance::new(
            format!("{PLUGIN} が PATH にありません"),
            "インストールしてください: https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html",
        )
        .into()
    })
}

fn find_in(path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|dir| dir.join(PLUGIN))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// 入るタスクのコンテナ
pub struct Target<'a> {
    pub region: &'a str,
    pub cluster: &'a str,
    pub task_arn: &'a str,
    pub container: &'a str,
    /// DescribeTasks が返すコンテナの runtimeId
    pub runtime_id: &'a str,
}

/// ExecuteCommand でセッションを作り、session-manager-plugin を子プロセスとして起動する。終了は待たない
pub async fn start(
    client: &Client,
    plugin: &Path,
    target: &Target<'_>,
    shell_command: &str,
    aws_profile: Option<&str>,
) -> Result<Child> {
    let output = execute_command_request(client, target, shell_command)
        .send()
        .await
        .with_context(|| format!("ExecuteCommand に失敗しました（task={}）", target.task_arn))?;
    let endpoint = endpoint_url(target.region).await?;
    let args = plugin_args(&output, target, aws_profile, &endpoint)?;
    Command::new(plugin)
        .args(args)
        .spawn()
        .with_context(|| format!("{} を起動できません", plugin.display()))
}

/// 起動した session-manager-plugin の終了を待つ
pub async fn wait(child: &mut Child) -> Result<ExitStatus> {
    child
        .wait()
        .await
        .with_context(|| format!("{PLUGIN} の終了を待てません"))
}

/// session-manager-plugin を SIGKILL で終わらせる。先に終わっていても構わない
pub async fn kill(child: &mut Child) -> Result<()> {
    child
        .kill()
        .await
        .with_context(|| format!("{PLUGIN} を終了できません"))
}

fn execute_command_request(
    client: &Client,
    target: &Target<'_>,
    shell_command: &str,
) -> ExecuteCommandFluentBuilder {
    client
        .execute_command()
        .cluster(target.cluster)
        .task(target.task_arn)
        .container(target.container)
        .interactive(true)
        .command(shell_command)
}

/// session-manager-plugin に渡すエンドポイント。AWS CLI の `ecs execute-command` と同じく、SSM ではなく ECS のものを渡す
async fn endpoint_url(region: &str) -> Result<String> {
    let params = Params::builder()
        .region(region)
        .use_fips(false)
        .use_dual_stack(false)
        .build()
        .context("ECS のエンドポイントを解決するパラメータが不正です")?;
    let endpoint = DefaultResolver::new()
        .resolve_endpoint(&params)
        .await
        .map_err(|e| anyhow!(e))
        .with_context(|| {
            format!("リージョン `{region}` の ECS のエンドポイントを解決できません")
        })?;
    Ok(endpoint.url().to_owned())
}

/// session-manager-plugin の引数。AWS CLI の `ecs execute-command` が渡すものと同じ並び
fn plugin_args(
    output: &ExecuteCommandOutput,
    target: &Target<'_>,
    aws_profile: Option<&str>,
    endpoint: &str,
) -> Result<Vec<String>> {
    let session = output
        .session()
        .context("ExecuteCommand の応答にセッションがありません")?;
    let session_json = json!({
        "sessionId": session.session_id().context("ExecuteCommand の応答に sessionId がありません")?,
        "streamUrl": session.stream_url().context("ExecuteCommand の応答に streamUrl がありません")?,
        "tokenValue": session.token_value().context("ExecuteCommand の応答に tokenValue がありません")?,
    });
    let ssm_target = format!(
        "ecs:{}_{}_{}",
        last_segment(target.cluster),
        last_segment(target.task_arn),
        target.runtime_id
    );
    Ok(vec![
        session_json.to_string(),
        target.region.to_owned(),
        "StartSession".to_owned(),
        aws_profile.unwrap_or_default().to_owned(),
        json!({ "Target": ssm_target }).to_string(),
        endpoint.to_owned(),
    ])
}

/// クラスタ名か ARN のどちらでも、`/` の後ろの名前や ID を取り出す
fn last_segment(name_or_arn: &str) -> &str {
    name_or_arn.rsplit('/').next().unwrap_or(name_or_arn)
}

/// session-manager-plugin が 0 以外で終わったときに出す 1 行。0 で終わったら None
pub fn abnormal_exit_message(status: ExitStatus) -> Option<String> {
    if status.success() {
        return None;
    }
    let cause = match (status.code(), status.signal()) {
        (Some(code), _) => format!("終了コード {code}"),
        (None, Some(signal)) => format!("シグナル {signal}"),
        (None, None) => "原因不明".to_owned(),
    };
    Some(format!("セッションが異常終了しました（{cause}）"))
}

/// セッションで起動するシェル。プロンプトに `[<プロファイル名>]` を出す
pub fn shell_command(profile_name: &str) -> String {
    format!("env PS1='[{profile_name}] \\w # ' /bin/sh")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use aws_config::{BehaviorVersion, Region};
    use aws_sdk_ecs::types::Session;
    use serde_json::Value;

    use super::*;

    const TASK_ARN: &str = "arn:aws:ecs:us-east-1:123456789012:task/example-staging/abc123";

    fn target() -> Target<'static> {
        Target {
            region: "us-east-1",
            cluster: "example-staging",
            task_arn: TASK_ARN,
            container: "app",
            runtime_id: "abc123-456",
        }
    }

    fn output() -> ExecuteCommandOutput {
        ExecuteCommandOutput::builder()
            .session(
                Session::builder()
                    .session_id("ecs-execute-command-1")
                    .stream_url("wss://ssmmessages.us-east-1.amazonaws.com/v1/data-channel/1")
                    .token_value("token")
                    .build(),
            )
            .build()
    }

    fn offline_client() -> Client {
        Client::from_conf(
            aws_sdk_ecs::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .region(Region::new("us-east-1"))
                .build(),
        )
    }

    fn parse(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn execute_command_runs_the_shell_interactively_in_the_target_container() {
        let request = execute_command_request(&offline_client(), &target(), "/bin/sh");
        let input = request.as_input();

        assert_eq!(input.get_cluster().as_deref(), Some("example-staging"));
        assert_eq!(input.get_task().as_deref(), Some(TASK_ARN));
        assert_eq!(input.get_container().as_deref(), Some("app"));
        assert_eq!(input.get_interactive(), &Some(true));
        assert_eq!(input.get_command().as_deref(), Some("/bin/sh"));
    }

    #[test]
    fn plugin_receives_session_region_and_start_session_like_aws_cli() {
        let args = plugin_args(&output(), &target(), None, "https://ecs.example").unwrap();

        assert_eq!(args.len(), 6);
        assert_eq!(
            parse(&args[0]),
            json!({
                "sessionId": "ecs-execute-command-1",
                "streamUrl": "wss://ssmmessages.us-east-1.amazonaws.com/v1/data-channel/1",
                "tokenValue": "token",
            })
        );
        assert_eq!(args[1], "us-east-1");
        assert_eq!(args[2], "StartSession");
        assert_eq!(args[5], "https://ecs.example");
    }

    #[test]
    fn plugin_target_is_cluster_task_id_and_container_runtime_id() {
        let args = plugin_args(&output(), &target(), None, "https://ecs.example").unwrap();

        assert_eq!(
            parse(&args[4]),
            json!({ "Target": "ecs:example-staging_abc123_abc123-456" })
        );
    }

    #[test]
    fn plugin_target_uses_cluster_name_even_when_cluster_is_given_as_arn() {
        let target = Target {
            cluster: "arn:aws:ecs:us-east-1:123456789012:cluster/example-staging",
            ..target()
        };

        let args = plugin_args(&output(), &target, None, "https://ecs.example").unwrap();

        assert_eq!(
            parse(&args[4]),
            json!({ "Target": "ecs:example-staging_abc123_abc123-456" })
        );
    }

    #[test]
    fn plugin_receives_resolved_aws_profile_or_empty_string_for_sdk_default() {
        let with_profile =
            plugin_args(&output(), &target(), Some("example"), "https://ecs.example").unwrap();
        let without_profile =
            plugin_args(&output(), &target(), None, "https://ecs.example").unwrap();

        assert_eq!(with_profile[3], "example");
        assert_eq!(without_profile[3], "");
    }

    #[test]
    fn response_without_session_is_rejected() {
        let output = ExecuteCommandOutput::builder().build();

        assert!(plugin_args(&output, &target(), None, "https://ecs.example").is_err());
    }

    #[tokio::test]
    async fn endpoint_is_the_regional_ecs_endpoint() {
        assert_eq!(
            endpoint_url("ap-northeast-1").await.unwrap(),
            "https://ecs.ap-northeast-1.amazonaws.com"
        );
    }

    #[test]
    fn shell_is_bin_sh_with_profile_name_in_the_prompt() {
        assert_eq!(
            shell_command("staging"),
            r"env PS1='[staging] \w # ' /bin/sh"
        );
    }

    #[test]
    fn successful_session_exit_prints_nothing() {
        assert_eq!(abnormal_exit_message(ExitStatus::from_raw(0)), None);
    }

    #[test]
    fn nonzero_session_exit_is_reported_with_its_exit_code() {
        assert_eq!(
            abnormal_exit_message(ExitStatus::from_raw(255 << 8)).as_deref(),
            Some("セッションが異常終了しました（終了コード 255）")
        );
    }

    #[test]
    fn session_killed_by_a_signal_is_reported_with_the_signal_number() {
        assert_eq!(
            abnormal_exit_message(ExitStatus::from_raw(9)).as_deref(),
            Some("セッションが異常終了しました（シグナル 9）")
        );
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("ecsh-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put_plugin(dir: &Path, mode: u32) {
        let path = dir.join(PLUGIN);
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn plugin_is_found_in_the_first_path_directory_that_has_an_executable() {
        let empty = scratch_dir("empty");
        let installed = scratch_dir("installed");
        put_plugin(&installed, 0o755);
        let path = env::join_paths([&empty, &installed]).unwrap();

        assert_eq!(find_in(&path), Some(installed.join(PLUGIN)));
    }

    #[test]
    fn plugin_without_execute_permission_is_not_found() {
        let dir = scratch_dir("not-executable");
        put_plugin(&dir, 0o644);
        let path = env::join_paths([&dir]).unwrap();

        assert_eq!(find_in(&path), None);
    }
}
