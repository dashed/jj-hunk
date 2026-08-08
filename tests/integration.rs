use std::path::{Path, PathBuf};
use std::process::Command;

/// A temporary jj repo that is fully isolated from any ambient git/jj state.
/// Cleaned up on drop.
struct TestRepo {
    dir: PathBuf,
    config_path: PathBuf,
}

impl TestRepo {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("jj-hunk-test-{}-{}", name, std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();

        // Init a git-backed jj repo
        let out = Command::new("jj")
            .args(["git", "init"])
            .current_dir(&dir)
            .env("JJ_USER", "Test User")
            .env("JJ_EMAIL", "test@example.com")
            .env_remove("JJ_CONFIG")
            .output()
            .expect("jj git init failed");
        assert!(
            out.status.success(),
            "jj git init: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Write a jj config file for merge-tool setup.
        // This is passed via JJ_CONFIG to all jj invocations (including
        // those spawned by jj-hunk internally).
        let config_path = dir.join("_jj_config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[merge-tools.jj-hunk]\nprogram = {:?}\nedit-args = [\"select\", \"$left\", \"$right\"]\n",
                jj_hunk_bin(),
            ),
        ).unwrap();

        Self { dir, config_path }
    }

    fn path(&self) -> &Path {
        &self.dir
    }

    fn write_file(&self, name: &str, content: &str) {
        let path = self.dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn jj(&self, args: &[&str]) -> std::process::Output {
        Command::new("jj")
            .args(args)
            .current_dir(&self.dir)
            .env("JJ_USER", "Test User")
            .env("JJ_EMAIL", "test@example.com")
            .env("JJ_CONFIG", &self.config_path)
            .env("PATH", path_with_jj_hunk())
            .output()
            .expect("failed to run jj")
    }

    fn jj_ok(&self, args: &[&str]) -> String {
        let out = self.jj(args);
        assert!(
            out.status.success(),
            "jj {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn hunk(&self, args: &[&str]) -> std::process::Output {
        Command::new(jj_hunk_bin())
            .args(args)
            .current_dir(&self.dir)
            .env("JJ_USER", "Test User")
            .env("JJ_EMAIL", "test@example.com")
            .env("JJ_CONFIG", &self.config_path)
            .env("PATH", path_with_jj_hunk())
            .output()
            .expect("failed to run jj-hunk")
    }

    fn hunk_with_empty_config(&self, args: &[&str]) -> std::process::Output {
        let config = self.dir.join("empty-jj-config.toml");
        std::fs::write(&config, "").unwrap();

        Command::new(jj_hunk_bin())
            .args(args)
            .current_dir(&self.dir)
            .env("JJ_USER", "Test User")
            .env("JJ_EMAIL", "test@example.com")
            .env("JJ_CONFIG", config)
            .output()
            .expect("failed to run jj-hunk")
    }

    fn hunk_ok(&self, args: &[&str]) -> String {
        let out = self.hunk(args);
        assert!(
            out.status.success(),
            "jj-hunk {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout),
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn hunk_ok_with_empty_config(&self, args: &[&str]) -> String {
        let out = self.hunk_with_empty_config(args);
        assert!(
            out.status.success(),
            "jj-hunk {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout),
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn hunk_fail(&self, args: &[&str]) -> String {
        let out = self.hunk(args);
        assert!(
            !out.status.success(),
            "jj-hunk {:?} should have failed but succeeded: {}",
            args,
            String::from_utf8_lossy(&out.stdout),
        );
        let mut combined = String::from_utf8_lossy(&out.stderr).to_string();
        combined.push_str(&String::from_utf8_lossy(&out.stdout));
        combined
    }

    /// Get the log as a simple list of descriptions (most recent first).
    fn log_descriptions(&self) -> Vec<String> {
        let out = self.jj_ok(&[
            "log",
            "--no-graph",
            "-T",
            r#"if(description, description.first_line() ++ "\n", "")"#,
        ]);
        out.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// Show files changed in a revision.
    fn changed_files(&self, rev: &str) -> Vec<String> {
        let out = self.jj_ok(&["diff", "-r", rev, "--summary"]);
        out.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Build a PATH that includes the directory containing the jj-hunk binary,
/// so that `jj --tool=jj-hunk` can find it.
fn path_with_jj_hunk() -> String {
    let bin_dir = jj_hunk_bin().parent().unwrap().to_path_buf();
    let current_path = std::env::var("PATH").unwrap_or_default();
    format!("{}:{}", bin_dir.display(), current_path)
}

fn jj_hunk_bin() -> PathBuf {
    // Use the binary built by `cargo test`
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("jj-hunk");
    assert!(
        path.exists(),
        "jj-hunk binary not found at {:?}. Run `cargo build` first.",
        path
    );
    path
}

// ---------------------------------------------------------------------------
// list -r
// ---------------------------------------------------------------------------

#[test]
fn list_rev_shows_hunks_for_non_working_copy() {
    let repo = TestRepo::new("list-rev");

    // Create a commit with some content
    repo.write_file("a.txt", "line1\nline2\n");
    repo.jj_ok(&["commit", "-m", "add a.txt"]);

    // Make a second commit that modifies a.txt
    repo.write_file("a.txt", "line1\nLINE2\n");
    repo.jj_ok(&["commit", "-m", "modify a.txt"]);

    // Working copy is now empty — list @ should have nothing
    let list_wc = repo.hunk_ok(&["list"]);
    assert!(
        !list_wc.contains("a.txt"),
        "working copy should have no hunks for a.txt"
    );

    // list -r @- should show the modification
    let list_prev = repo.hunk_ok(&["list", "-r", "@-"]);
    assert!(
        list_prev.contains("a.txt"),
        "list -r @- should show a.txt hunks:\n{}",
        list_prev
    );
    assert!(list_prev.contains("LINE2"));
}

#[test]
fn list_rev_files_mode() {
    let repo = TestRepo::new("list-rev-files");

    repo.write_file("foo.txt", "hello\n");
    repo.write_file("bar.txt", "world\n");
    repo.jj_ok(&["commit", "-m", "initial"]);

    repo.write_file("foo.txt", "hello changed\n");
    repo.write_file("bar.txt", "world changed\n");
    repo.jj_ok(&["commit", "-m", "changes"]);

    let out = repo.hunk_ok(&["list", "-r", "@-", "--files"]);
    assert!(out.contains("foo.txt"));
    assert!(out.contains("bar.txt"));
}

// ---------------------------------------------------------------------------
// split -r
// ---------------------------------------------------------------------------

#[test]
fn split_rev_splits_non_working_copy_revision() {
    let repo = TestRepo::new("split-rev");

    // Base commit
    repo.write_file("a.txt", "aaa\n");
    repo.write_file("b.txt", "bbb\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    // A commit that touches both files
    repo.write_file("a.txt", "AAA\n");
    repo.write_file("b.txt", "BBB\n");
    repo.jj_ok(&["commit", "-m", "modify both"]);

    // Now split @- keeping only a.txt changes in the first commit
    let spec = r#"{"files": {"a.txt": {"action": "keep"}}, "default": "reset"}"#;
    repo.hunk_ok(&["split", "-r", "@-", spec, "only a.txt changes"]);

    // Should now have: base -> "only a.txt changes" -> (rest) -> @
    let log = repo.log_descriptions();
    assert!(
        log.iter().any(|d| d == "only a.txt changes"),
        "should have the split commit: {:?}",
        log
    );

    // The first split commit should only touch a.txt
    // Find the commit by description
    let diff_out = repo.jj_ok(&[
        "log",
        "--no-graph",
        "-r",
        r#"description(substring:"only a.txt changes")"#,
        "-T",
        "change_id ++ \"\n\"",
    ]);
    let change_id = diff_out.trim();
    assert!(!change_id.is_empty(), "should find split commit");

    let files = repo.changed_files(change_id);
    let has_a = files.iter().any(|f| f.contains("a.txt"));
    let has_b = files.iter().any(|f| f.contains("b.txt"));
    assert!(
        has_a,
        "split commit should contain a.txt changes: {:?}",
        files
    );
    assert!(
        !has_b,
        "split commit should NOT contain b.txt changes: {:?}",
        files
    );
}

#[test]
fn split_rev_with_spec_file() {
    let repo = TestRepo::new("split-rev-specfile");

    repo.write_file("x.txt", "xxx\n");
    repo.write_file("y.txt", "yyy\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("x.txt", "XXX\n");
    repo.write_file("y.txt", "YYY\n");
    repo.jj_ok(&["commit", "-m", "modify both"]);

    // Write spec to a file
    let spec_path = repo.path().join("_spec.json");
    std::fs::write(
        &spec_path,
        r#"{"files": {"x.txt": {"action": "keep"}}, "default": "reset"}"#,
    )
    .unwrap();

    repo.hunk_ok(&[
        "split",
        "-r",
        "@-",
        "-f",
        spec_path.to_str().unwrap(),
        "x only",
    ]);

    let log = repo.log_descriptions();
    assert!(log.iter().any(|d| d == "x only"), "log: {:?}", log);
}

#[test]
fn split_without_rev_operates_on_working_copy() {
    let repo = TestRepo::new("split-no-rev");

    repo.write_file("a.txt", "aaa\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    // Changes in working copy
    repo.write_file("a.txt", "AAA\n");
    repo.write_file("b.txt", "BBB\n");

    let spec = r#"{"files": {"a.txt": {"action": "keep"}}, "default": "reset"}"#;
    repo.hunk_ok(&["split", spec, "a changes"]);

    let log = repo.log_descriptions();
    assert!(
        log.iter().any(|d| d == "a changes"),
        "should have the split commit: {:?}",
        log
    );
}

// ---------------------------------------------------------------------------
// squash -r
// ---------------------------------------------------------------------------

#[test]
fn squash_rev_squashes_non_working_copy_into_parent() {
    let repo = TestRepo::new("squash-rev");

    // Base with a.txt
    repo.write_file("a.txt", "aaa\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    // Commit that adds b.txt and c.txt
    repo.write_file("b.txt", "bbb\n");
    repo.write_file("c.txt", "ccc\n");
    repo.jj_ok(&["commit", "-m", "add b and c"]);

    // Empty working copy now. Squash only b.txt from @- into its parent.
    let spec = r#"{"files": {"b.txt": {"action": "keep"}}, "default": "reset"}"#;
    repo.hunk_ok(&["squash", "-r", "@-", spec]);

    // The parent ("base") should now contain b.txt
    let base_files = repo.changed_files(r#"description(substring:"base")"#);
    let has_b = base_files.iter().any(|f| f.contains("b.txt"));
    assert!(has_b, "base should now have b.txt: {:?}", base_files);

    // @- should still have c.txt but not b.txt
    let mid_files = repo.changed_files("@-");
    let has_c = mid_files.iter().any(|f| f.contains("c.txt"));
    let still_has_b = mid_files.iter().any(|f| f.contains("b.txt"));
    assert!(has_c, "@- should still have c.txt: {:?}", mid_files);
    assert!(
        !still_has_b,
        "@- should NOT have b.txt anymore: {:?}",
        mid_files
    );
}

#[test]
fn squash_without_rev_operates_on_working_copy() {
    let repo = TestRepo::new("squash-no-rev");

    repo.write_file("a.txt", "aaa\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    // Working copy changes
    repo.write_file("a.txt", "AAA\n");
    repo.write_file("b.txt", "BBB\n");

    let spec = r#"{"files": {"a.txt": {"action": "keep"}}, "default": "reset"}"#;
    repo.hunk_ok(&["squash", spec]);

    // a.txt change should be squashed into base
    let base_files = repo.changed_files(r#"description(substring:"base")"#);
    let has_a = base_files.iter().any(|f| f.contains("a.txt"));
    assert!(has_a, "base should have a.txt: {:?}", base_files);
}

// ---------------------------------------------------------------------------
// commit (no -r, sanity check)
// ---------------------------------------------------------------------------

#[test]
fn commit_works_on_working_copy() {
    let repo = TestRepo::new("commit-wc");

    repo.write_file("a.txt", "aaa\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("a.txt", "AAA\n");
    repo.write_file("b.txt", "BBB\n");

    let spec = r#"{"files": {"a.txt": {"action": "keep"}}, "default": "reset"}"#;
    repo.hunk_ok(&["commit", spec, "commit a only"]);

    let log = repo.log_descriptions();
    assert!(log.iter().any(|d| d == "commit a only"), "log: {:?}", log);

    // b.txt should still be in working copy
    let wc_files = repo.changed_files("@");
    let has_b = wc_files.iter().any(|f| f.contains("b.txt"));
    assert!(has_b, "b.txt should remain in working copy: {:?}", wc_files);
}

#[test]
fn commit_self_configures_jj_hunk_tool_when_user_config_is_empty() {
    let repo = TestRepo::new("commit-self-configures");

    repo.write_file("a.txt", "aaa\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("a.txt", "AAA\n");
    repo.write_file("b.txt", "BBB\n");

    let spec = r#"{"files": {"a.txt": {"action": "keep"}}, "default": "reset"}"#;
    repo.hunk_ok_with_empty_config(&["commit", spec, "commit a via self config"]);

    let log = repo.log_descriptions();
    assert!(
        log.iter().any(|d| d == "commit a via self config"),
        "log: {:?}",
        log
    );
}

// ---------------------------------------------------------------------------
// error cases
// ---------------------------------------------------------------------------

#[test]
fn split_rev_invalid_revset_fails() {
    let repo = TestRepo::new("split-bad-rev");

    repo.write_file("a.txt", "aaa\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    let spec = r#"{"files": {"a.txt": {"action": "keep"}}, "default": "reset"}"#;
    let err = repo.hunk_fail(&["split", "-r", "nonexistent_bookmark", spec, "msg"]);
    assert!(
        err.contains("failed")
            || err.contains("error")
            || err.contains("Error")
            || err.contains("Revision"),
        "should fail with bad revset: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// list -r with spec preview
// ---------------------------------------------------------------------------

#[test]
fn list_rev_with_spec_filters_output() {
    let repo = TestRepo::new("list-rev-spec");

    repo.write_file("keep.txt", "keep\n");
    repo.write_file("drop.txt", "drop\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("keep.txt", "KEEP\n");
    repo.write_file("drop.txt", "DROP\n");
    repo.jj_ok(&["commit", "-m", "changes"]);

    // List with a spec that only shows keep.txt
    let spec = r#"{"files": {"keep.txt": {"action": "keep"}}, "default": "reset"}"#;
    let out = repo.hunk_ok(&["list", "-r", "@-", "--spec", spec]);
    assert!(out.contains("keep.txt"), "should show keep.txt:\n{}", out);
    assert!(
        !out.contains("drop.txt"),
        "should NOT show drop.txt:\n{}",
        out
    );
}

// ---------------------------------------------------------------------------
// `--format diff` must emit patches that real patch tools apply correctly.
//
// Regression tests for the off-by-one in zero-length hunk ranges. In unified
// diff format a range with length 0 is written as `-<n>,0` / `+<n>,0` where
// `n` is the line AFTER WHICH the change applies, not the line it sits at.
// Emitting the latter makes `git apply` silently place the change in the
// wrong location instead of rejecting the patch.
// ---------------------------------------------------------------------------

/// Run `git apply` on a patch inside the repo dir. Returns (success, stderr).
fn git_apply(dir: &Path, patch: &str) -> (bool, String) {
    let patch_path = dir.join("_test.patch");
    std::fs::write(&patch_path, patch).unwrap();
    let out = Command::new("git")
        .args(["apply", "_test.patch"])
        .current_dir(dir)
        .output()
        .expect("failed to run git apply");
    let _ = std::fs::remove_file(&patch_path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Build a repo with `base` committed, then `modified` in the working copy.
/// Returns the `--format diff` patch, having restored the file to `base` so
/// the patch has a clean target to apply against.
fn patch_for(repo: &TestRepo, name: &str, base: &str, modified: &str) -> String {
    repo.write_file(name, base);
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file(name, modified);
    let patch = repo.hunk_ok(&["list", "--format", "diff"]);
    repo.write_file(name, base); // reset to base for a clean apply
    patch
}

#[test]
fn diff_format_insertion_applies_at_the_right_place() {
    let repo = TestRepo::new("diff-fmt-insert");
    let base = "L1\nL2\nL3\nL4\nL5\n";
    let modified = "L1\nL2\nINS\nL3\nL4\nL5\n";
    let patch = patch_for(&repo, "f.txt", base, modified);

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected the patch:\n{err}\npatch:\n{patch}");

    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(
        got, modified,
        "insertion landed in the wrong place\npatch:\n{patch}"
    );
}

#[test]
fn diff_format_insertion_at_start_of_file_applies_correctly() {
    let repo = TestRepo::new("diff-fmt-insert-start");
    let base = "L1\nL2\nL3\n";
    let modified = "TOP\nL1\nL2\nL3\n";
    let patch = patch_for(&repo, "f.txt", base, modified);

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected the patch:\n{err}\npatch:\n{patch}");

    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(got, modified, "patch:\n{patch}");
}

#[test]
fn diff_format_deletion_applies_at_the_right_place() {
    let repo = TestRepo::new("diff-fmt-delete");
    let base = "L1\nL2\nL3\nL4\nL5\n";
    let modified = "L1\nL2\nL4\nL5\n";
    let patch = patch_for(&repo, "f.txt", base, modified);

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected the patch:\n{err}\npatch:\n{patch}");

    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(got, modified, "patch:\n{patch}");
}

#[test]
fn diff_format_replacement_applies_at_the_right_place() {
    let repo = TestRepo::new("diff-fmt-replace");
    let base = "L1\nL2\nL3\nL4\nL5\n";
    let modified = "L1\nL2\nXX\nL4\nL5\n";
    let patch = patch_for(&repo, "f.txt", base, modified);

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected the patch:\n{err}\npatch:\n{patch}");

    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(got, modified, "patch:\n{patch}");
}

#[test]
fn diff_format_multiple_hunks_apply_correctly() {
    let repo = TestRepo::new("diff-fmt-multi");
    let base = "L1\nL2\nL3\nL4\nL5\nL6\nL7\nL8\n";
    let modified = "L1\nADD\nL2\nL3\nL4\nL5\nL6\nL8\n"; // insert near top, delete near bottom
    let patch = patch_for(&repo, "f.txt", base, modified);

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected the patch:\n{err}\npatch:\n{patch}");

    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(got, modified, "patch:\n{patch}");
}

/// Parse `@@ -a,b +c,d @@` into (a, b, c, d).
fn parse_hunk_header(header: &str) -> (usize, usize, usize, usize) {
    let inner = header
        .trim_start_matches("@@ ")
        .split(" @@")
        .next()
        .expect("malformed header");
    let (old, new) = inner.split_once(' ').expect("malformed ranges");
    let parse = |s: &str| -> (usize, usize) {
        let s = &s[1..]; // drop leading - / +
        match s.split_once(',') {
            Some((a, b)) => (a.parse().unwrap(), b.parse().unwrap()),
            None => (s.parse().unwrap(), 1),
        }
    };
    let (a, b) = parse(old);
    let (c, d) = parse(new);
    (a, b, c, d)
}

/// The header's declared lengths must equal the lines actually emitted.
/// A mismatch is what makes a patch silently misapply, so assert it directly
/// rather than relying on `git apply` to notice.
#[test]
fn diff_format_headers_agree_with_emitted_body() {
    let repo = TestRepo::new("diff-fmt-headers");
    let base = "L1\nL2\nL3\nL4\nL5\nL6\nL7\nL8\nL9\nL10\n";
    let modified = "L1\nADD\nL2\nL3\nL4\nL5\nL6\nL7\nL9\nXX\n";
    let patch = patch_for(&repo, "f.txt", base, modified);

    let mut checked = 0;
    let lines: Vec<&str> = patch.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !line.starts_with("@@") {
            continue;
        }
        let (_, old_len, _, new_len) = parse_hunk_header(line);
        let (mut old_count, mut new_count) = (0, 0);
        for body in &lines[i + 1..] {
            match body.chars().next() {
                Some(' ') => {
                    old_count += 1;
                    new_count += 1;
                }
                Some('-') if !body.starts_with("---") => old_count += 1,
                Some('+') if !body.starts_with("+++") => new_count += 1,
                _ => break,
            }
        }
        assert_eq!(
            (old_len, new_len),
            (old_count, new_count),
            "header {line} disagrees with its body\npatch:\n{patch}"
        );
        checked += 1;
    }
    assert!(checked > 0, "no hunk headers found in:\n{patch}");
}

/// Hunks close enough that their context windows overlap must be emitted as a
/// single block; two blocks with overlapping ranges are an invalid patch.
#[test]
fn diff_format_adjacent_hunks_merge_into_one_block() {
    let repo = TestRepo::new("diff-fmt-adjacent");
    let base = "L1\nL2\nL3\nL4\nL5\nL6\n";
    let modified = "X1\nL2\nL3\nL4\nL5\nX6\n"; // two changes, 4 lines apart
    let patch = patch_for(&repo, "f.txt", base, modified);

    let headers = patch.lines().filter(|l| l.starts_with("@@")).count();
    assert_eq!(headers, 1, "expected one merged block, got {headers}\n{patch}");

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected merged block:\n{err}\n{patch}");
    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(got, modified, "patch:\n{patch}");
}

/// Far-apart hunks stay separate blocks and still apply.
#[test]
fn diff_format_distant_hunks_stay_separate_and_apply() {
    let repo = TestRepo::new("diff-fmt-distant");
    let base: String = (1..=30).map(|i| format!("L{i}\n")).collect();
    let modified: String = (1..=30)
        .map(|i| match i {
            2 => "X2\n".to_string(),
            28 => "X28\n".to_string(),
            _ => format!("L{i}\n"),
        })
        .collect();
    let patch = patch_for(&repo, "f.txt", &base, &modified);

    let headers = patch.lines().filter(|l| l.starts_with("@@")).count();
    assert_eq!(headers, 2, "expected two blocks, got {headers}\n{patch}");

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected:\n{err}\n{patch}");
    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(got, modified, "patch:\n{patch}");
}

/// A newly added file has no before-side context; it must still apply.
#[test]
fn diff_format_new_file_applies() {
    let repo = TestRepo::new("diff-fmt-newfile");
    repo.write_file("existing.txt", "keep\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("new.txt", "alpha\nbeta\n");
    let patch = repo.hunk_ok(&["list", "--format", "diff"]);
    std::fs::remove_file(repo.path().join("new.txt")).unwrap();

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected new-file patch:\n{err}\n{patch}");
    let got = std::fs::read_to_string(repo.path().join("new.txt")).unwrap();
    assert_eq!(got, "alpha\nbeta\n", "patch:\n{patch}");
}

// ---------------------------------------------------------------------------
// Strict selectors.
//
// A hunkset expression that is malformed, misspelled, or misused must fail
// loudly. Selecting nothing and exiting 0 is the worst outcome for this tool:
// an agent driving `split` in a loop produces empty junk commits and believes
// it succeeded.
// ---------------------------------------------------------------------------

/// A repo with two single-line insertions, one in a .py and one in a .rs file.
fn strict_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("a.py", "import os\n");
    repo.write_file("a.rs", "use std::fs;\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.py", "import os\nimport sys\n");
    repo.write_file("a.rs", "use std::fs;\nuse std::io;\n");
    repo
}

#[test]
fn unknown_hunk_type_is_an_error_not_an_empty_selection() {
    let repo = strict_repo("strict-type");
    let err = repo.hunk_fail(&["list", "--spec", "type(insertt)"]);
    assert!(
        err.contains("insertt"),
        "error should name the bad value, got: {err}"
    );
}

#[test]
fn unknown_status_value_is_an_error() {
    let repo = strict_repo("strict-status");
    let err = repo.hunk_fail(&["list", "--spec", "status(bogus)"]);
    assert!(err.contains("bogus"), "got: {err}");
}

#[test]
fn hunk_type_is_case_sensitive_and_reports_the_bad_value() {
    let repo = strict_repo("strict-case");
    let err = repo.hunk_fail(&["list", "--spec", "type(INSERT)"]);
    assert!(err.contains("INSERT"), "got: {err}");
}

#[test]
fn valid_enum_values_still_work() {
    let repo = strict_repo("strict-valid-enum");
    for spec in ["type(insert)", "type(delete)", "type(replace)", "status(modified)"] {
        let out = repo.hunk_ok(&["list", "--spec", spec, "--format", "text"]);
        // Must not error; may legitimately match nothing for delete/replace.
        assert!(!out.contains("error"), "{spec} errored: {out}");
    }
}

#[test]
fn hunkset_syntax_error_is_reported_as_a_hunkset_error() {
    let repo = strict_repo("strict-syntax");
    let err = repo.hunk_fail(&["list", "--spec", "type(insert"]);
    assert!(
        !err.contains("Failed to parse spec as JSON"),
        "syntax error leaked into the JSON parser: {err}"
    );
    assert!(
        err.to_lowercase().contains("hunkset"),
        "expected a hunkset error, got: {err}"
    );
}

#[test]
fn trailing_operator_is_a_hunkset_syntax_error() {
    let repo = strict_repo("strict-trailing-op");
    let err = repo.hunk_fail(&["list", "--spec", "type(insert) &"]);
    assert!(
        !err.contains("Failed to parse spec as JSON"),
        "leaked into JSON parser: {err}"
    );
}

#[test]
fn json_specs_still_parse_and_report_json_errors() {
    let repo = strict_repo("strict-json-untouched");
    // Valid JSON spec keeps working.
    let out = repo.hunk_ok(&[
        "list",
        "--spec",
        r#"{"files": {"a.py": {"action": "keep"}}, "default": "reset"}"#,
        "--format",
        "text",
    ]);
    assert!(out.contains("a.py"), "valid JSON spec broke: {out}");
    // Malformed JSON must still be reported as a spec error, not a hunkset one.
    let err = repo.hunk_fail(&["list", "--spec", r#"{"files": {"#]);
    assert!(err.to_lowercase().contains("json") || err.to_lowercase().contains("yaml"),
        "expected a JSON/YAML error, got: {err}");
}

#[test]
fn explicit_substring_prefix_is_honoured_not_downgraded_to_exact() {
    let repo = strict_repo("strict-substring");
    let out = repo.hunk_ok(&["list", "--spec", r#"file(substring:"a.p")"#, "--format", "text"]);
    assert!(
        out.contains("a.py"),
        "explicit substring: was discarded, got: {out}"
    );
}

#[test]
fn explicit_regex_prefix_is_honoured_by_glob_predicate() {
    let repo = strict_repo("strict-glob-regex");
    let out = repo.hunk_ok(&["list", "--spec", r#"glob(regex:"a\\.py")"#, "--format", "text"]);
    assert!(
        out.contains("a.py"),
        "explicit regex: was discarded by glob(), got: {out}"
    );
}

#[test]
fn default_exact_matching_for_file_is_preserved() {
    let repo = strict_repo("strict-exact-default");
    // A bare string on file() still means exact: a partial path must not match.
    let out = repo.hunk_ok(&["list", "--spec", r#"file("a.p")"#, "--format", "text"]);
    assert!(!out.contains("a.py"), "bare string should be exact, got: {out}");
    let out = repo.hunk_ok(&["list", "--spec", r#"file("a.py")"#, "--format", "text"]);
    assert!(out.contains("a.py"), "exact path should match, got: {out}");
}

#[test]
fn pattern_prefix_inside_quotes_is_rejected_with_a_hint() {
    let repo = strict_repo("strict-quoted-prefix");
    let err = repo.hunk_fail(&["list", "--spec", r#"content("regex:import")"#]);
    assert!(
        err.contains("regex:"),
        "error should quote the offending prefix, got: {err}"
    );
}

#[test]
fn mutating_command_refuses_an_empty_selection() {
    let repo = strict_repo("strict-empty-split");
    let err = repo.hunk_fail(&["split", "type(insertt)", "typo commit"]);
    assert!(
        !err.is_empty(),
        "split with a bad selector should fail loudly"
    );
    // and must not have created a commit
    let descs = repo.log_descriptions();
    assert!(
        !descs.iter().any(|d| d.contains("typo commit")),
        "an empty junk commit was created anyway: {descs:?}"
    );
}

#[test]
fn mutating_command_refuses_a_valid_selector_that_matches_nothing() {
    let repo = strict_repo("strict-empty-nomatch");
    // Syntactically fine, semantically empty: no such file.
    let err = repo.hunk_fail(&["split", r#"file("nope.txt")"#, "no match"]);
    assert!(!err.is_empty(), "expected failure");
    let descs = repo.log_descriptions();
    assert!(
        !descs.iter().any(|d| d.contains("no match")),
        "created a commit from an empty selection: {descs:?}"
    );
}

#[test]
fn list_with_a_selector_matching_nothing_is_not_an_error() {
    // `list` is read-only: an empty result is legitimate output, not a failure.
    let repo = strict_repo("strict-empty-list");
    let out = repo.hunk_ok(&["list", "--spec", r#"file("nope.txt")"#, "--format", "text"]);
    assert!(!out.contains("a.py"), "unexpected match: {out}");
}

#[test]
fn split_with_a_matching_selector_still_works() {
    let repo = strict_repo("strict-happy-path");
    repo.hunk_ok(&["split", r#"file("a.py")"#, "feat: python change"]);
    let descs = repo.log_descriptions();
    assert!(
        descs.iter().any(|d| d.contains("feat: python change")),
        "happy path broke: {descs:?}"
    );
}

#[test]
fn depth_rejects_a_non_numeric_argument() {
    let repo = strict_repo("strict-depth");
    let err = repo.hunk_fail(&["list", "--spec", "depth(abc)"]);
    assert!(
        err.contains("abc") || err.contains("number or range"),
        "expected an argument error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// toplevel() and depth() must agree with each other, and must not quietly
// include hunks from files no parser ever looked at.
//
// These need a real parser, so they are gated on the `semantic` feature.
// They are inert on alberto/hunkset-lang (which declares `semantic = []`)
// and run in the integration merge, where the grammars are present.
// include hunks from files no parser ever looked at.
// ---------------------------------------------------------------------------

#[test]
#[cfg(feature = "semantic")]
fn unparsed_files_are_excluded_from_both_toplevel_and_depth() {
    let repo = TestRepo::new("sem-unparsed");
    repo.write_file("a.rs", "use std::fs;\n");
    repo.write_file("notes.txt", "hello\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.rs", "use std::fs;\nuse std::io;\n");
    repo.write_file("notes.txt", "hello\nworld\n");

    let toplevel = repo.hunk_ok(&["list", "--spec", "toplevel()", "--format", "text"]);
    let depth0 = repo.hunk_ok(&["list", "--spec", "depth(0)", "--format", "text"]);

    // Whatever the answer is, the two must agree about notes.txt.
    assert_eq!(
        toplevel.contains("notes.txt"),
        depth0.contains("notes.txt"),
        "toplevel() and depth(0) disagree about an unparsed file\n\
         toplevel:\n{toplevel}\ndepth(0):\n{depth0}"
    );
    assert!(
        !depth0.contains("notes.txt"),
        "a file with no parser must not be reported as depth 0:\n{depth0}"
    );
}

#[test]
#[cfg(feature = "semantic")]
fn toplevel_and_depth_agree_on_python_and_rust() {
    let repo = TestRepo::new("sem-py-rs");
    repo.write_file("a.py", "import os\n");
    repo.write_file("a.rs", "use std::fs;\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.py", "import os\nimport sys\n");
    repo.write_file("a.rs", "use std::fs;\nuse std::io;\n");

    let toplevel = repo.hunk_ok(&["list", "--spec", "toplevel()", "--format", "text"]);
    for f in ["a.py", "a.rs"] {
        assert!(
            toplevel.contains(f),
            "{f} top-level import missing from toplevel():\n{toplevel}"
        );
    }
    let depth0 = repo.hunk_ok(&["list", "--spec", "depth(0)", "--format", "text"]);
    for f in ["a.py", "a.rs"] {
        assert!(depth0.contains(f), "{f} missing from depth(0):\n{depth0}");
    }
}

#[test]
#[cfg(feature = "semantic")]
fn semantic_predicates_warn_when_a_file_cannot_be_analyzed() {
    let repo = TestRepo::new("sem-warn");
    repo.write_file("notes.txt", "hello\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("notes.txt", "hello\nworld\n");

    // Every hunk is unanalyzable, so a semantic query returning nothing is
    // misleading without a diagnostic.
    let out = repo.hunk(&["list", "--spec", r#"function("anything")"#, "--format", "text"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("warning"),
        "expected a warning about unanalyzable files, got stderr: {stderr}"
    );
}

#[test]
#[cfg(feature = "semantic")]
fn no_warning_when_files_are_analyzable() {
    let repo = TestRepo::new("sem-nowarn");
    repo.write_file("a.rs", "fn a() {}\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.rs", "fn a() {}\nfn b() {}\n");

    let out = repo.hunk(&["list", "--spec", r#"function("b")"#, "--format", "text"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.to_lowercase().contains("warning"),
        "should not warn for a supported language: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Symlinks. WalkDir does not follow them, so filtering on is_file() left them
// out of the file union entirely: no spec could select or reset one, and the
// change rode along in every commit. `exists()` and `fs::copy` both traverse
// links, so a dangling link in `left` deleted an unselected file and a live one
// wrote the target's bytes into the link's path.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn symlink_change_does_not_leak_into_a_selective_split() {
    let repo = TestRepo::new("symlink-leak");
    repo.write_file("a.txt", "AAA-line1\n");
    repo.write_file("f.txt", "FFF-secret\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.txt", "AAA-line1\nAAA-line2\n");
    std::fs::remove_file(repo.path().join("f.txt")).unwrap();
    std::os::unix::fs::symlink("a.txt", repo.path().join("f.txt")).unwrap();

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"a.txt": {"hunks": [0]}}, "default": "reset"}"#,
        "only a.txt",
    ]);

    // The selected change must land intact -- not the symlink target's bytes.
    let committed = repo.jj_ok(&["file", "show", "-r", "@-", "a.txt"]);
    assert_eq!(
        committed, "AAA-line1\nAAA-line2\n",
        "a.txt in the split commit was corrupted"
    );
    // And the unselected symlink change must not ride along.
    let summary = repo.changed_files("@-");
    assert!(
        !summary.iter().any(|l| l.contains("f.txt")),
        "unselected symlink change leaked into the commit: {summary:?}"
    );
}

#[cfg(unix)]
#[test]
fn dangling_symlink_does_not_delete_an_unselected_file() {
    let repo = TestRepo::new("symlink-dangling");
    repo.write_file("a.txt", "one\n");
    // `s` points at a path that will not be materialised alongside it.
    std::os::unix::fs::symlink("absent-target.txt", repo.path().join("s")).unwrap();
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.txt", "one\ntwo\n");
    std::fs::remove_file(repo.path().join("s")).unwrap();
    repo.write_file("s", "now a regular file\n");

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"a.txt": {"hunks": [0]}}, "default": "reset"}"#,
        "only a.txt",
    ]);

    let summary = repo.changed_files("@-");
    assert!(
        !summary.iter().any(|l| l.contains(" s") || l.ends_with('s')),
        "unselected symlink->file change leaked into the commit: {summary:?}"
    );
}

#[cfg(unix)]
#[test]
fn symlink_is_restored_as_a_link_not_as_its_target_content() {
    let repo = TestRepo::new("symlink-restore");
    repo.write_file("target.txt", "TARGET\n");
    std::os::unix::fs::symlink("target.txt", repo.path().join("link")).unwrap();
    repo.write_file("a.txt", "one\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    // retarget the link, and change a.txt
    repo.write_file("a.txt", "one\ntwo\n");
    std::fs::remove_file(repo.path().join("link")).unwrap();
    std::os::unix::fs::symlink("a.txt", repo.path().join("link")).unwrap();

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"a.txt": {"hunks": [0]}}, "default": "reset"}"#,
        "only a.txt",
    ]);

    // After reset, the working copy's link must still be a symlink.
    let meta = std::fs::symlink_metadata(repo.path().join("link")).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "reset replaced the symlink with a regular file"
    );
}

// ---------------------------------------------------------------------------
// Argument validation. A missing or wrong-typed argument used to degenerate
// into `none()` -- which negation turned into `all()`, so a forgotten filename
// committed the entire diff.
// ---------------------------------------------------------------------------

#[test]
fn zero_argument_predicates_are_an_error_not_an_empty_set() {
    let repo = strict_repo("args-zero");
    for spec in ["file()", "type()", "content()", "glob()", "id()", "lines()"] {
        let err = repo.hunk_fail(&["list", "--spec", spec]);
        assert!(!err.is_empty(), "{spec} should error");
    }
}

#[test]
fn negated_zero_argument_predicate_does_not_select_everything() {
    let repo = strict_repo("args-zero-neg");
    // The dangerous shape: user means "everything except <file>", forgets the
    // name, and would otherwise commit the whole diff.
    let err = repo.hunk_fail(&["list", "--spec", "~file()"]);
    assert!(err.contains("file()"), "got: {err}");
    // And it must not be reachable through a mutating command either.
    let err = repo.hunk_fail(&["split", "~file()", "oops"]);
    assert!(!err.is_empty());
    assert!(
        !repo.log_descriptions().iter().any(|d| d.contains("oops")),
        "a commit was created from a bogus selector"
    );
}

#[test]
fn wrong_typed_arguments_are_rejected() {
    let repo = strict_repo("args-type");
    for spec in [r#"lines("abc")"#, "type(1..3)", "file(1..3)", r#"depth("x")"#] {
        let err = repo.hunk_fail(&["list", "--spec", spec]);
        assert!(!err.is_empty(), "{spec} should error");
    }
}

#[test]
fn reversed_line_range_is_rejected() {
    let repo = strict_repo("args-reversed");
    let err = repo.hunk_fail(&["list", "--spec", "lines(100..1)"]);
    assert!(err.contains("100..1"), "got: {err}");
}

#[test]
fn bare_number_is_accepted_as_a_single_line_range() {
    let repo = strict_repo("args-bare-num");
    // `lines(2)` reads as "hunks touching line 2" and must behave as 2..2
    // rather than silently matching nothing.
    let one = repo.hunk_ok(&["list", "--spec", "lines(2)", "--format", "text"]);
    let rng = repo.hunk_ok(&["list", "--spec", "lines(2..2)", "--format", "text"]);
    assert_eq!(one, rng, "lines(2) should equal lines(2..2)");
}

#[test]
fn valid_argument_forms_still_work() {
    let repo = strict_repo("args-regression");
    for spec in [
        "all()",
        r#"file("a.py")"#,
        "type(insert)",
        "lines(1..3)",
        r#"~file("a.py")"#,
        r#"content("import")"#,
    ] {
        let out = repo.hunk_ok(&["list", "--spec", spec, "--format", "text"]);
        assert!(!out.contains("error"), "{spec} regressed: {out}");
    }
}

#[test]
fn ambiguous_or_malformed_hunk_id_is_rejected() {
    let repo = strict_repo("id-ambiguous");
    for spec in [r#"id("hunk-")"#, r#"id("not-hex")"#, r#"id("")"#] {
        let err = repo.hunk_fail(&["list", "--spec", spec]);
        assert!(!err.is_empty(), "{spec} should error");
    }
}

#[test]
fn a_full_hunk_id_still_selects_exactly_one() {
    let repo = strict_repo("id-full");
    let json = repo.hunk_ok(&["list", "--format", "json"]);
    let id = json
        .split(r#""id": ""#)
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("no id in output")
        .to_string();
    let out = repo.hunk_ok(&["list", "--spec", &format!(r#"id("{id}")"#), "--format", "text"]);
    assert_eq!(out.matches("  hunk ").count(), 1, "expected exactly one: {out}");
}

// ---------------------------------------------------------------------------
// Line-terminator fidelity in `--format diff`.
//
// str::lines() strips \r and loses whether the last line was newline
// terminated. Re-emitting with a bare \n converted CRLF files to LF and
// dropped the `\ No newline at end of file` marker -- and when the ONLY change
// was the trailing newline, that produced a textually identical hunk that
// git apply accepted while silently discarding the edit.
// ---------------------------------------------------------------------------

/// Write bytes, emit a patch, restore the base, apply, compare bytes.
fn diff_roundtrip(name: &str, base: &[u8], modified: &[u8]) {
    let repo = TestRepo::new(name);
    std::fs::write(repo.path().join("f.txt"), base).unwrap();
    repo.jj_ok(&["commit", "-m", "base"]);
    std::fs::write(repo.path().join("f.txt"), modified).unwrap();

    let patch = repo.hunk_ok(&["list", "--format", "diff"]);
    std::fs::write(repo.path().join("f.txt"), base).unwrap();

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "{name}: git apply rejected:\n{err}\npatch:\n{patch}");
    let got = std::fs::read(repo.path().join("f.txt")).unwrap();
    assert_eq!(
        got, modified,
        "{name}: bytes differ after round-trip\npatch:\n{patch}"
    );
}

#[test]
fn diff_format_preserves_missing_trailing_newline() {
    diff_roundtrip("nl-none", b"a\nb\nc", b"a\nZ\nc");
}

#[test]
fn diff_format_handles_dropping_the_trailing_newline() {
    // The silent-corruption case: without the marker this emits `-c`/`+c`,
    // which is textually identical, so git apply succeeds and loses the edit.
    diff_roundtrip("nl-drop", b"a\nb\nc\n", b"a\nb\nc");
}

#[test]
fn diff_format_handles_adding_a_trailing_newline() {
    diff_roundtrip("nl-add", b"a\nb\nc", b"a\nb\nc\n");
}

#[test]
fn diff_format_preserves_crlf() {
    diff_roundtrip("crlf", b"a\r\nb\r\nc\r\n", b"a\r\nB\r\nc\r\n");
}

#[test]
fn diff_format_preserves_crlf_without_final_newline() {
    diff_roundtrip("crlf-nonl", b"a\r\nb\r\nc", b"a\r\nB\r\nc");
}
