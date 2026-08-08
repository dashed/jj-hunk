/// Match a glob pattern against a path.
///
/// Supports `*` (single segment wildcard), `**` (multi-segment), and `?` (single char).
/// When the pattern contains no `/` or `**`, it matches against the filename only.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./");
    let path = path.trim_start_matches("./");

    if pattern.contains('/') || pattern.contains("**") {
        let pat_segs: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
        let path_segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        match_segments(&pat_segs, &path_segs)
    } else {
        // Filename-only glob: match against the last path component
        let filename = path.rsplit('/').next().unwrap_or(path);
        match_segment(pattern, filename)
    }
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        if match_segments(&pattern[1..], path) {
            return true;
        }
        if !path.is_empty() {
            return match_segments(pattern, &path[1..]);
        }
        return false;
    }
    if path.is_empty() {
        return false;
    }
    if !match_segment(pattern[0], path[0]) {
        return false;
    }
    match_segments(&pattern[1..], &path[1..])
}

fn match_segment(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let mut dp = vec![vec![false; txt.len() + 1]; pat.len() + 1];
    dp[0][0] = true;
    for i in 1..=pat.len() {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=pat.len() {
        for j in 1..=txt.len() {
            dp[i][j] = match pat[i - 1] {
                '*' => dp[i - 1][j] || dp[i][j - 1],
                '?' => dp[i - 1][j - 1],
                c => dp[i - 1][j - 1] && c == txt[j - 1],
            };
        }
    }
    dp[pat.len()][txt.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_glob() {
        assert!(glob_match("*.rs", "lib.rs"));
        assert!(glob_match("*.rs", "src/lib.rs")); // filename-only when no /
        assert!(!glob_match("*.rs", "lib.py"));
    }

    #[test]
    fn path_glob() {
        assert!(glob_match("src/**/*.rs", "src/lib.rs"));
        assert!(glob_match("src/**/*.rs", "src/sub/lib.rs"));
        assert!(!glob_match("src/**/*.rs", "tests/lib.rs"));
    }

    #[test]
    fn question_mark() {
        assert!(glob_match("?.rs", "a.rs"));
        assert!(!glob_match("?.rs", "ab.rs"));
    }

    #[test]
    fn exact_path() {
        assert!(glob_match("src/lib.rs", "src/lib.rs"));
        assert!(!glob_match("src/lib.rs", "src/main.rs"));
    }
}
