pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub fn get_version() -> String {
    let profile = built_info::PROFILE;
    let ci_platform = built_info::CI_PLATFORM.unwrap_or("<CI platform not detected>");
    let commit = built_info::GIT_COMMIT_HASH_SHORT.unwrap_or("<Git commit hash not found>");

    let mut version = String::from("\n\n");

    version.push_str(&format!("PROFILE     = {profile}\n"));
    version.push_str(&format!("CI PLATFORM = {ci_platform}\n"));
    version.push_str(&format!("GIT COMMIT  = {commit}\n"));

    version
}
