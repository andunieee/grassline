//! What to call this build. A build from a clean tag is just the tag; a
//! build from a dirty tree, past a tag, or with debug assertions names the
//! commit as well.

fn format(tag: Option<&str>, count: u64, commit: Option<&str>, dirty: bool, debug: bool) -> String {
    let Some(commit) = commit else {
        return format!("v{}", env!("CARGO_PKG_VERSION"));
    };
    if let Some(tag) = tag {
        if count == 0 && !dirty && !debug {
            return tag.to_string();
        }
    }
    let mut version = match tag {
        Some(tag) if count == 0 => format!("{tag}+{commit}"),
        Some(tag) => format!("{tag}-{count}-g{commit}"),
        None => commit.to_string(),
    };
    if dirty {
        version.push_str("-dirty");
    }
    if debug {
        version.push_str("-debug");
    }
    version
}

/// The version to show at startup and with `--version`.
pub fn full() -> &'static str {
    static FULL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FULL.get_or_init(|| {
        let count = option_env!("GRASSLINE_GIT_COUNT")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        format(
            option_env!("GRASSLINE_GIT_TAG"),
            count,
            option_env!("GRASSLINE_GIT_COMMIT"),
            option_env!("GRASSLINE_GIT_DIRTY") == Some("1"),
            cfg!(debug_assertions),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::format;

    #[test]
    fn a_clean_tag_is_enough() {
        assert_eq!(
            format(Some("v1.2.3"), 0, Some("abc1234"), false, false),
            "v1.2.3"
        );
    }

    #[test]
    fn past_a_tag_names_the_commit() {
        assert_eq!(
            format(Some("v1.2.3"), 4, Some("abc1234"), false, false),
            "v1.2.3-4-gabc1234"
        );
        assert_eq!(
            format(Some("v1.2.3"), 4, Some("abc1234"), true, false),
            "v1.2.3-4-gabc1234-dirty"
        );
    }

    #[test]
    fn a_debug_build_names_the_commit_even_on_a_tag() {
        assert_eq!(
            format(Some("v1.2.3"), 0, Some("abc1234"), false, true),
            "v1.2.3+abc1234-debug"
        );
    }

    #[test]
    fn with_no_tag_the_commit_is_the_version() {
        assert_eq!(
            format(None, 0, Some("abc1234"), true, true),
            "abc1234-dirty-debug"
        );
    }

    #[test]
    fn without_git_falls_back_to_the_package_version() {
        assert_eq!(
            format(None, 0, None, false, false),
            format!("v{}", env!("CARGO_PKG_VERSION"))
        );
    }
}
