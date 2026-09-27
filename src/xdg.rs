use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

/// `$XDG_CONFIG_HOME/ecsh`、未設定か空なら `$HOME/.config/ecsh`
pub fn config_dir() -> Option<PathBuf> {
    ecsh_dir(
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
        ".config",
    )
}

/// `$XDG_STATE_HOME/ecsh`、未設定か空なら `$HOME/.local/state/ecsh`
pub fn state_dir() -> Option<PathBuf> {
    ecsh_dir(
        env::var_os("XDG_STATE_HOME"),
        env::var_os("HOME"),
        ".local/state",
    )
}

fn ecsh_dir(xdg: Option<OsString>, home: Option<OsString>, under_home: &str) -> Option<PathBuf> {
    let base = match (xdg, home) {
        (Some(xdg), _) if !xdg.is_empty() => PathBuf::from(xdg),
        (_, Some(home)) if !home.is_empty() => PathBuf::from(home).join(under_home),
        _ => return None,
    };
    Some(base.join("ecsh"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_variable_is_preferred_over_home() {
        assert_eq!(
            ecsh_dir(Some("/xdg".into()), Some("/home/me".into()), ".local/state"),
            Some(PathBuf::from("/xdg/ecsh"))
        );
    }

    #[test]
    fn home_is_used_when_xdg_variable_is_unset_or_empty() {
        let expected = Some(PathBuf::from("/home/me/.local/state/ecsh"));

        assert_eq!(
            ecsh_dir(None, Some("/home/me".into()), ".local/state"),
            expected
        );
        assert_eq!(
            ecsh_dir(Some("".into()), Some("/home/me".into()), ".local/state"),
            expected
        );
    }

    #[test]
    fn nothing_is_resolved_without_xdg_variable_and_home() {
        assert_eq!(ecsh_dir(None, None, ".local/state"), None);
        assert_eq!(ecsh_dir(Some("".into()), Some("".into()), ".config"), None);
    }
}
