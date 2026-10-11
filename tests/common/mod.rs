#![allow(dead_code)]

use std::path::PathBuf;

#[cfg(feature = "rpc")]
mod geth;
#[cfg(feature = "rpc")]
#[allow(unused_imports)]
pub use geth::Geth;

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/geth")
}

pub fn update_fixtures() -> bool {
    std::env::var("ETH_STATE_DIFF_UPDATE_FIXTURES").as_deref() == Ok("1")
}
