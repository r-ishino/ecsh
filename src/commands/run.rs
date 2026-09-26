use anyhow::{Result, bail};

use crate::config::Profile;

pub fn run(profile: &Profile) -> Result<()> {
    eprintln!(
        "対象: region={} cluster={} service={} container={}",
        profile.region, profile.cluster, profile.service, profile.container
    );
    bail!("run は未実装です")
}
