//! Program version banner.

use crate::constants::VERSION;

/// Prints the program version banner.
pub(crate) fn version() {
    let build = option_env!("CI_PIPELINE_ID");
    let rev = option_env!("CI_COMMIT_SHORT_SHA");
    let tag = option_env!("CI_COMMIT_REF_NAME");

    if let (Some(build), Some(rev), Some(tag)) = (build, rev, tag) {
        println!("chilled-crates {VERSION}+{build}.g{rev}.{tag}");
    } else {
        println!("chilled-crates {VERSION}");
    }
}
