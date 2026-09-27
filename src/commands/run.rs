use anyhow::Result;

use crate::ui::Guidance;

/// 入るコマンドの旧名。何も起動せず、`exec` を案内して失敗する
pub fn run(args: &[String]) -> Result<()> {
    Err(anyhow::Error::new(guidance(args)))
}

fn guidance(args: &[String]) -> Guidance {
    let profile = args
        .iter()
        .take_while(|arg| *arg != "--")
        .find(|arg| !arg.starts_with('-'))
        .map_or("<プロファイル>", String::as_str);
    Guidance::new(
        "タスクに入るコマンドは `ecsh exec` に改名しました",
        format!("対話で入るなら `ecsh exec {profile}`"),
    )
}

#[cfg(test)]
mod tests {
    use crate::ui::Style;

    use super::*;

    fn shown(args: &[&str]) -> Vec<String> {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        Style::PLAIN.error_lines(&run(&args).unwrap_err())
    }

    #[test]
    fn run_points_to_exec_with_the_given_profile() {
        assert_eq!(
            shown(&["-y", "staging"]),
            [
                "✗ タスクに入るコマンドは `ecsh exec` に改名しました",
                "  対話で入るなら `ecsh exec staging`",
            ]
        );
    }

    #[test]
    fn run_without_a_profile_points_to_exec_with_a_placeholder() {
        assert_eq!(shown(&[])[1], "  対話で入るなら `ecsh exec <プロファイル>`");
        assert_eq!(
            shown(&["--", "rake"])[1],
            "  対話で入るなら `ecsh exec <プロファイル>`"
        );
    }
}
