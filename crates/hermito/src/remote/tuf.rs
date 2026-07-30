use std::path::PathBuf;

use url::Url;

#[derive(Clone, Debug)]
pub struct TufPolicy {
    pub trusted_root: PathBuf,
    pub metadata_base_url: Url,
    pub targets_base_url: Url,
    pub datastore: PathBuf,
    pub target_cache: PathBuf,
    pub offline_metadata_url: Option<Url>,
    pub offline_targets_url: Option<Url>,
    pub allow_offline_cache: bool,
    pub max_target_bytes: u64,
}

impl TufPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_target_bytes == 0 {
            return Err("TUF target cap must be non-zero");
        }
        if self.allow_offline_cache
            && (self.offline_metadata_url.is_none() || self.offline_targets_url.is_none())
        {
            return Err("offline TUF policy requires explicit metadata and target cache URLs");
        }
        Ok(())
    }
}
