use anyhow::{Result, bail};

use crate::config::Profile;

pub fn gc(profile: &Profile) -> Result<()> {
    eprintln!(
        "対象: region={} cluster={}",
        profile.region, profile.cluster
    );
    bail!("gc は未実装です")
}
