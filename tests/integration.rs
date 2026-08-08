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

// ---------------------------------------------------------------------------
// Paths are handed to `jj file show` as fileset expressions. Unquoted, any
// path containing `( ) " ' ~ , [ ]` is parsed as fileset *syntax*, so the
// read either fails or reads a different file -- and because the read never
// checked its exit status, the failure became "empty file", the file yielded
// zero hunks, and it was dropped from `list` entirely.
// ---------------------------------------------------------------------------

/// Path characters that are legal on every platform jj-hunk targets and that
/// all mean something to the fileset parser.
const METACHAR_NAMES: &[&str] = &[
    "plain.txt",
    "paren(1).txt",
    "single'.txt",
    "tilde~x.txt",
    "has,comma.txt",
];

fn metachar_repo(name: &str, names: &[&str]) -> TestRepo {
    let repo = TestRepo::new(name);
    for f in names {
        repo.write_file(f, "a\n");
    }
    repo.jj_ok(&["commit", "-m", "base"]);
    for f in names {
        repo.write_file(f, "a\nb\n");
    }
    repo
}

#[test]
fn paths_with_fileset_metacharacters_are_not_silently_dropped() {
    let repo = metachar_repo("fileset-metachars", METACHAR_NAMES);

    // jj itself sees every file.
    let summary = repo.changed_files("@");
    assert_eq!(
        summary.len(),
        METACHAR_NAMES.len(),
        "precondition: jj should report every file: {summary:?}"
    );

    let out = repo.hunk_ok(&["list", "--files", "--format", "text"]);
    for f in METACHAR_NAMES {
        assert!(
            out.contains(f),
            "{f} vanished from `list` -- its path was parsed as fileset syntax:\n{out}"
        );
    }
}

/// `"` is legal in a POSIX filename and is the fileset string delimiter, so it
/// is the one character that a naive `"{path}"` wrapper would still get wrong.
#[cfg(unix)]
#[test]
fn a_double_quote_in_a_path_is_escaped_not_dropped() {
    let repo = metachar_repo("fileset-quote", &["plain.txt", "quote\".txt"]);
    let out = repo.hunk_ok(&["list", "--files", "--format", "text"]);
    assert!(
        out.contains("quote\".txt"),
        "a path containing a double quote vanished from `list`:\n{out}"
    );
}

/// A bare path is parsed as a *glob* pattern, so `br[1].txt` matched `br1.txt`
/// and `list` reported one file's hunks under the other file's name.
#[test]
fn glob_metacharacters_in_a_path_do_not_read_a_different_file() {
    let repo = TestRepo::new("fileset-glob-metachars");
    repo.write_file("br[1].txt", "same\n");
    repo.write_file("br1.txt", "same\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    // Only the bracketed file changes. A glob read would find `br1.txt`,
    // see no difference, and report zero hunks.
    repo.write_file("br[1].txt", "same\nbracketed-change\n");

    let out = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(
        out.contains("bracketed-change"),
        "hunks for `br[1].txt` were computed against `br1.txt`:\n{out}"
    );
    assert!(
        !out.contains("br1.txt (") && !out.contains("M br1.txt"),
        "`br1.txt` is unchanged and must not appear:\n{out}"
    );
}

/// The documented `--spec-template` -> `split` flow must move every file it
/// listed, not commit a subset and leave the rest behind at exit 0.
#[test]
fn spec_template_split_does_not_leave_metachar_files_behind() {
    let repo = metachar_repo("fileset-metachars-split", METACHAR_NAMES);

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    let spec_path = repo.path().join("_tmpl.json");
    std::fs::write(&spec_path, &template).unwrap();

    repo.hunk_ok(&["split", "-f", spec_path.to_str().unwrap(), "everything"]);

    let committed = repo.changed_files("@-");
    for f in METACHAR_NAMES {
        assert!(
            committed.iter().any(|l| l.contains(f)),
            "{f} was silently left in the working copy instead of committed: {committed:?}"
        );
    }
}

/// A read that fails must surface an error, not degrade into an empty file.
/// `tilde~x.txt` is the case that proves an exit-status check alone is not
/// enough: unquoted, jj parses it as `tilde ~ x.txt`, warns on stderr, and
/// exits **0** with empty stdout.
#[test]
fn a_tilde_in_a_path_is_not_parsed_as_a_difference_operator() {
    let repo = metachar_repo("fileset-tilde", &["plain.txt", "tilde~x.txt"]);
    let out = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(
        out.contains("tilde~x.txt"),
        "`tilde~x.txt` was parsed as `tilde ~ x.txt` and dropped:\n{out}"
    );
}

/// A leading `-` made jj's own argument parser read the path as flags, so the
/// read failed and the file was dropped. Quoting it into a `file:` pattern
/// fixes the argument parsing too.
#[test]
fn a_leading_dash_in_a_path_is_not_parsed_as_a_flag() {
    let repo = metachar_repo("fileset-leading-dash", &["plain.txt", "-dash.txt"]);
    let out = repo.hunk_ok(&["list", "--files", "--format", "text"]);
    assert!(
        out.contains("-dash.txt"),
        "`-dash.txt` was parsed as command-line flags and dropped:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// Merge revisions. `@-` on a two-parent commit is ambiguous: `jj file show -r
// @-` fails, the before-text came back empty, and every file looked like a
// whole-file insertion. Both `--spec-template` (IDs that can never match, so
// `split` makes an EMPTY commit) and index selection then acted on a diff that
// was never real -- at exit 0.
// ---------------------------------------------------------------------------

/// A working copy whose parent is a merge of two bookmarks, with `f.txt`
/// (present in both parents) modified on top.
fn merge_working_copy_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("f.txt", "L1\nL2\nL3\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.jj_ok(&["bookmark", "create", "b0", "-r", "@-"]);

    repo.write_file("a.txt", "A\n");
    repo.jj_ok(&["commit", "-m", "side A"]);
    repo.jj_ok(&["bookmark", "create", "ba", "-r", "@-"]);

    repo.jj_ok(&["new", "b0"]);
    repo.write_file("b.txt", "B\n");
    repo.jj_ok(&["commit", "-m", "side B"]);
    repo.jj_ok(&["bookmark", "create", "bb", "-r", "@-"]);

    // Working copy is now a merge of the two sides.
    repo.jj_ok(&["new", "ba", "bb"]);
    repo.write_file("f.txt", "L1\nCHANGED\nL3\n");
    repo
}

#[test]
fn a_merge_working_copy_is_an_error_not_a_bogus_whole_file_insertion() {
    let repo = merge_working_copy_repo("merge-list");

    // Precondition: this really is a merge, and the change really is a
    // one-line replacement.
    let status = repo.jj_ok(&["status"]);
    assert_eq!(
        status.matches("Parent commit").count(),
        2,
        "precondition: working copy should have two parents:\n{status}"
    );

    let err = repo.hunk_fail(&["list", "--format", "text"]);
    let lower = err.to_lowercase();
    assert!(
        lower.contains("merge") || lower.contains("parent"),
        "expected a clear merge-commit error, got: {err}"
    );
}

#[test]
fn a_merge_revision_does_not_produce_a_spec_template_that_matches_nothing() {
    let repo = merge_working_copy_repo("merge-spec-template");
    let err = repo.hunk_fail(&["list", "--spec-template"]);
    assert!(!err.is_empty(), "--spec-template should fail on a merge");
}

#[test]
fn a_merge_revision_never_yields_a_silent_empty_split() {
    let repo = merge_working_copy_repo("merge-split");

    // A hunkset selector goes through the same diff computation, so it must
    // fail rather than produce IDs from a diff that was never real.
    let err = repo.hunk_fail(&["split", "all()", "merge split"]);
    assert!(!err.is_empty(), "split on a merge should fail loudly");

    let descs = repo.log_descriptions();
    assert!(
        !descs.iter().any(|d| d.contains("merge split")),
        "an empty commit was created from a merge revision: {descs:?}"
    );
}

#[test]
fn a_merge_revision_selected_with_dash_r_is_also_rejected() {
    let repo = merge_working_copy_repo("merge-dash-r");
    // Describe the merge so we can name it, then move off it.
    repo.jj_ok(&["describe", "-m", "the merge"]);
    repo.jj_ok(&["new"]);

    let err = repo.hunk_fail(&["list", "-r", "@-", "--format", "text"]);
    let lower = err.to_lowercase();
    assert!(
        lower.contains("merge") || lower.contains("parent"),
        "expected a merge error for `-r @-`, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Multi-revision revsets. `jj diff -r 'all()' --summary` lists files, but
// `jj-hunk list -r 'all()'` printed nothing at exit 0 -- the before/after
// reads both failed and every file collapsed to zero hunks.
// ---------------------------------------------------------------------------

fn two_commit_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("x.txt", "1\n");
    repo.jj_ok(&["commit", "-m", "c1"]);
    repo.write_file("x.txt", "2\n");
    repo.jj_ok(&["commit", "-m", "c2"]);
    repo.write_file("x.txt", "3\n");
    repo
}

#[test]
fn a_multi_revision_revset_is_an_error_not_empty_output() {
    let repo = two_commit_repo("multi-rev");
    let err = repo.hunk_fail(&["list", "-r", "all()", "--format", "text"]);
    let lower = err.to_lowercase();
    assert!(
        lower.contains("revision") || lower.contains("revset"),
        "expected a 'single revision required' error, got: {err}"
    );
}

#[test]
fn a_multi_revision_revset_cannot_drive_a_mutating_command() {
    let repo = two_commit_repo("multi-rev-split");
    let err = repo.hunk_fail(&["split", "-r", "all()", "all()", "multi rev split"]);
    assert!(!err.is_empty(), "split -r 'all()' should fail");
    let descs = repo.log_descriptions();
    assert!(
        !descs.iter().any(|d| d.contains("multi rev split")),
        "a commit was created from a multi-revision revset: {descs:?}"
    );
}

/// The single-revision forms must keep working exactly as before.
#[test]
fn single_revision_revsets_are_unaffected() {
    let repo = two_commit_repo("single-rev-ok");

    // Bare working copy.
    let wc = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(wc.contains("x.txt"), "working copy list broke:\n{wc}");

    // `-r @-`.
    let prev = repo.hunk_ok(&["list", "-r", "@-", "--format", "text"]);
    assert!(prev.contains("x.txt"), "-r @- broke:\n{prev}");

    // A revset function that still resolves to exactly one revision.
    let named = repo.hunk_ok(&[
        "list",
        "-r",
        r#"description(substring:"c2")"#,
        "--format",
        "text",
    ]);
    assert!(named.contains("x.txt"), "single-rev revset broke:\n{named}");

    // The root commit has no parent; it must not be mistaken for an error.
    let root = repo.hunk(&["list", "-r", "root()", "--format", "text"]);
    assert!(
        root.status.success(),
        "root() should not error: {}",
        String::from_utf8_lossy(&root.stderr)
    );
}

// ---------------------------------------------------------------------------
// Executable bit. `apply_hunk_selection` rewrote the right-hand file with
// `fs::write`, which preserves that file's mode, so an unselected `chmod +x`
// rode along in the split commit.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn an_unselected_exec_bit_change_does_not_leak_into_a_split() {
    let repo = TestRepo::new("exec-bit-leak");
    repo.write_file("a.txt", "aaa\n");
    repo.write_file("s.sh", "S1\nS2\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("a.txt", "aaa\nbbb\n");
    repo.write_file("s.sh", "S1\nS2\nS3\n");
    let path = repo.path().join("s.sh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).unwrap();

    // Keep a.txt's hunk; explicitly keep nothing from s.sh.
    repo.hunk_ok(&[
        "split",
        r#"{"files": {"a.txt": {"hunks": [0]}, "s.sh": {"hunks": []}}, "default": "reset"}"#,
        "only a.txt",
    ]);

    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        !committed.contains("new mode 100755"),
        "an unselected chmod +x rode along in the split commit:\n{committed}"
    );
    assert!(
        committed.contains("+bbb"),
        "the selected change is missing:\n{committed}"
    );
}

/// The same leak, but through the `default: reset` path rather than an
/// explicit empty selection.
#[cfg(unix)]
#[test]
fn an_exec_bit_change_on_a_partially_selected_file_is_reset() {
    let repo = TestRepo::new("exec-bit-partial");
    repo.write_file("s.sh", "S1\nS2\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("s.sh", "S1\nS2\nS3\n");
    let path = repo.path().join("s.sh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).unwrap();

    // Select the content hunk. The mode change is not a hunk, so it must not
    // be carried along with it.
    repo.hunk_ok(&[
        "split",
        r#"{"files": {"s.sh": {"hunks": [0]}}, "default": "reset"}"#,
        "content only",
    ]);

    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("+S3"),
        "the selected content hunk is missing:\n{committed}"
    );
    assert!(
        !committed.contains("new mode 100755"),
        "the mode change rode along with the selected hunk:\n{committed}"
    );
}

/// A mode-only change produces zero hunks, so it was dropped from `list`
/// entirely -- the user could not see that the file had changed at all.
#[cfg(unix)]
#[test]
fn a_mode_only_change_is_visible_in_list() {
    let repo = TestRepo::new("exec-bit-visible");
    repo.write_file("only.sh", "x\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    let path = repo.path().join("only.sh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).unwrap();

    // Precondition: jj sees the change.
    let summary = repo.changed_files("@");
    assert!(
        summary.iter().any(|l| l.contains("only.sh")),
        "precondition: jj should report the mode change: {summary:?}"
    );

    let text = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(
        text.contains("only.sh"),
        "a mode-only change was invisible in `list`:\n{text}"
    );
    assert!(
        text.contains("100755"),
        "`list` should say what the mode changed to:\n{text}"
    );

    let json = repo.hunk_ok(&["list"]);
    assert!(
        json.contains("only.sh") && json.contains("100755"),
        "a mode-only change was invisible in the JSON output:\n{json}"
    );
}

/// A file with both a content change and a mode change must report both.
#[cfg(unix)]
#[test]
fn a_mode_change_alongside_hunks_is_also_reported() {
    let repo = TestRepo::new("exec-bit-visible-both");
    repo.write_file("s.sh", "S1\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("s.sh", "S1\nS2\n");
    let path = repo.path().join("s.sh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).unwrap();

    let text = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(text.contains("+ S2"), "content hunk missing:\n{text}");
    assert!(
        text.contains("100755"),
        "mode change not reported next to the hunks:\n{text}"
    );
}

/// Adding a file with the executable bit set is an addition, not a mode
/// *change*; it must not be annotated as one.
#[cfg(unix)]
#[test]
fn a_newly_added_executable_is_not_reported_as_a_mode_change() {
    let repo = TestRepo::new("exec-bit-added");
    repo.write_file("keep.txt", "k\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("new.sh", "n\n");
    let path = repo.path().join("new.sh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).unwrap();

    let text = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(text.contains("new.sh"), "added file missing:\n{text}");
    assert!(
        !text.contains("100644"),
        "an added executable was described as a mode change:\n{text}"
    );
}

#[test]
fn ambiguous_or_malformed_hunk_id_is_rejected() {
    let repo = strict_repo("id-ambiguous");
    for spec in [r#"id("hunk-")"#, r#"id("not-hex")"#, r#"id("")"#] {
        let err = repo.hunk_fail(&["list", "--spec", spec]);
        assert!(!err.is_empty(), "{spec} should error");
    }
}

// ---------------------------------------------------------------------------
// Renames. `list` diffs `left/<source>` against `right/<target>`, so the hunk
// id it reports is computed over the real edit. `select` only ever saw the
// spec key, so it joined the target name onto both directories, found no
// `left/<target>`, recomputed the hunk as "insert whole file" under a
// different id, matched nothing, and wrote an empty file.
// ---------------------------------------------------------------------------

/// A repo whose working copy renames `src.txt` to `dst.txt` and edits one line.
/// The lines are long enough for jj to report a rename rather than add+delete.
fn rename_repo(name: &str) -> (TestRepo, &'static str) {
    const BASE: &str = "aaaaaaaaaaaa\nbbbbbbbbbbbb\ncccccccccccc\ndddddddddddd\neeeeeeeeeeee\n";
    const EDITED: &str = "aaaaaaaaaaaa\nbbbbbbbbbbbb\nCCC-CHANGED\ndddddddddddd\neeeeeeeeeeee\n";

    let repo = TestRepo::new(name);
    repo.write_file("src.txt", BASE);
    repo.jj_ok(&["commit", "-m", "base"]);
    std::fs::remove_file(repo.path().join("src.txt")).unwrap();
    repo.write_file("dst.txt", EDITED);

    // Guard against a vacuous test: if jj reports add+delete instead of a
    // rename, none of the rename code paths are exercised at all.
    let summary = repo.changed_files("@");
    assert!(
        summary
            .iter()
            .any(|l| l.contains("src.txt") && l.contains("dst.txt")),
        "jj did not detect a rename, the test would be vacuous: {summary:?}"
    );

    (repo, EDITED)
}

#[test]
fn renamed_file_selected_by_id_keeps_its_content() {
    let (repo, edited) = rename_repo("rename-by-id");

    // The documented workflow: take the spec template and feed it straight back.
    let template = repo.hunk_ok(&["list", "--spec-template"]);
    repo.hunk_ok(&["split", &template, "keep the rename"]);

    let got = repo.jj_ok(&["file", "show", "-r", "@-", "dst.txt"]);
    assert_eq!(
        got, edited,
        "renamed file was committed with the wrong content (spec was:\n{template})"
    );
}

#[test]
fn renamed_file_selected_by_a_hand_written_id_spec_keeps_its_content() {
    // The other half of the id path: an id copied out of `list` into a spec by
    // hand carries no rename information, so the source has to be filled in
    // from the diff before `select` is handed the spec.
    let (repo, edited) = rename_repo("rename-handwritten-id");
    let id = first_hunk_id(&repo, "dst.txt");
    let spec = format!(r#"{{"files": {{"dst.txt": {{"ids": ["{id}"]}}}}, "default": "reset"}}"#);

    repo.hunk_ok(&["split", &spec, "keep the rename by bare id"]);

    let got = repo.jj_ok(&["file", "show", "-r", "@-", "dst.txt"]);
    assert_eq!(got, edited, "spec was:\n{spec}");
}

#[test]
fn renamed_file_selected_by_index_still_keeps_its_content() {
    // Backward compatibility: a spec written before `from` existed carries no
    // rename information, and index selection must keep working as it did.
    let (repo, edited) = rename_repo("rename-by-index");

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"dst.txt": {"hunks": [0]}}, "default": "reset"}"#,
        "keep the rename by index",
    ]);

    let got = repo.jj_ok(&["file", "show", "-r", "@-", "dst.txt"]);
    assert_eq!(got, edited, "index selection regressed for a renamed file");
}

#[test]
fn keeping_a_rename_does_not_resurrect_the_source_file() {
    // jj hands `select` the rename as two entries: `left/src.txt` and
    // `right/dst.txt`. Only the target is named in the spec, so the source
    // fell through to `default: reset` and was copied back into `right`,
    // turning the rename into a copy.
    let (repo, _) = rename_repo("rename-source");

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    repo.hunk_ok(&["split", &template, "keep the rename"]);

    let tracked = repo.jj_ok(&["file", "list", "-r", "@-"]);
    assert!(
        !tracked.lines().any(|l| l.trim() == "src.txt"),
        "the rename source was resurrected, making the commit a copy:\n{tracked}"
    );
    assert!(
        tracked.lines().any(|l| l.trim() == "dst.txt"),
        "the rename target is missing from the commit:\n{tracked}"
    );
}

#[test]
fn resetting_a_rename_restores_the_source_file() {
    // The mirror case: nothing selected for the target, so the whole rename
    // must be undone -- target gone, source back where it was.
    let (repo, _) = rename_repo("rename-reset");

    repo.write_file("other.txt", "other\n");
    let other_id = first_hunk_id(&repo, "other.txt");
    let spec = format!(
        r#"{{"files": {{"other.txt": {{"ids": ["{other_id}"]}}}}, "default": "reset"}}"#
    );
    repo.hunk_ok(&["split", &spec, "only other.txt"]);

    let files = repo.changed_files("@-");
    assert!(
        !files.iter().any(|f| f.contains("dst.txt")),
        "an unselected rename leaked into the commit: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.contains("src.txt")),
        "an unselected rename deleted its source: {files:?}"
    );
}

// ---------------------------------------------------------------------------
// A selection that keeps nothing must reset the file, not mutate it. Blanking
// out the ids you do not want in a `--spec-template` is the natural workflow,
// and it produced non-empty but wrong commits: deletions still applied, and
// added files landed as 0-byte stubs.
// ---------------------------------------------------------------------------

#[test]
fn deleted_file_with_an_empty_selection_is_not_deleted() {
    let repo = TestRepo::new("empty-sel-delete");
    repo.write_file("gone.txt", "gone-line1\ngone-line2\n");
    repo.write_file("keep.txt", "keep-line1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
    repo.write_file("keep.txt", "keep-line1\nkeep-line2\n");

    let keep_id = first_hunk_id(&repo, "keep.txt");
    let spec = format!(
        r#"{{"files": {{"gone.txt": {{"ids": []}}, "keep.txt": {{"ids": ["{keep_id}"]}}}}, "default": "reset"}}"#
    );
    repo.hunk_ok(&["split", &spec, "only keep.txt"]);

    let files = repo.changed_files("@-");
    assert!(
        !files.iter().any(|f| f.contains("gone.txt")),
        "a file whose selection keeps nothing was deleted anyway: {files:?}"
    );
    assert!(
        files.iter().any(|f| f.contains("keep.txt")),
        "the selected change is missing: {files:?}"
    );
}

#[test]
fn added_file_with_an_empty_selection_is_left_out_entirely() {
    let repo = TestRepo::new("empty-sel-add");
    repo.write_file("keep.txt", "keep-line1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("new.txt", "brand new\ncontent\n");
    repo.write_file("keep.txt", "keep-line1\nkeep-line2\n");

    let keep_id = first_hunk_id(&repo, "keep.txt");
    let spec = format!(
        r#"{{"files": {{"new.txt": {{"ids": []}}, "keep.txt": {{"ids": ["{keep_id}"]}}}}, "default": "reset"}}"#
    );
    repo.hunk_ok(&["split", &spec, "only keep2"]);

    let files = repo.changed_files("@-");
    assert!(
        !files.iter().any(|f| f.contains("new.txt")),
        "an unselected added file landed in the commit (as a 0-byte stub): {files:?}"
    );
    assert!(
        files.iter().any(|f| f.contains("keep.txt")),
        "the selected change is missing: {files:?}"
    );
}

#[test]
fn a_deletion_that_is_selected_still_deletes_the_file() {
    // The mirror of the case above: restoring a file whose selection keeps
    // nothing must not make deletions unselectable.
    let repo = TestRepo::new("selected-delete");
    repo.write_file("gone.txt", "gone-line1\ngone-line2\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();

    let gone_id = first_hunk_id(&repo, "gone.txt");
    let spec =
        format!(r#"{{"files": {{"gone.txt": {{"ids": ["{gone_id}"]}}}}, "default": "reset"}}"#);
    repo.hunk_ok(&["split", &spec, "drop gone.txt"]);

    let files = repo.changed_files("@-");
    assert!(
        files.iter().any(|f| f.contains("gone.txt")),
        "a selected deletion was undone: {files:?}"
    );
}

#[test]
fn an_added_file_that_is_selected_keeps_its_content() {
    // The mirror of the 0-byte case: removing unselected additions must not
    // drop selected ones.
    let repo = TestRepo::new("selected-add");
    repo.write_file("base.txt", "base\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("new.txt", "brand new\ncontent\n");

    let new_id = first_hunk_id(&repo, "new.txt");
    let spec = format!(r#"{{"files": {{"new.txt": {{"ids": ["{new_id}"]}}}}, "default": "reset"}}"#);
    repo.hunk_ok(&["split", &spec, "add new.txt"]);

    let got = repo.jj_ok(&["file", "show", "-r", "@-", "new.txt"]);
    assert_eq!(got, "brand new\ncontent\n", "a selected addition was dropped");
}

#[test]
fn hunkset_selection_of_a_renamed_file_keeps_its_content() {
    // The hunkset path builds its own spec, so it has to carry the rename
    // source too.
    let (repo, edited) = rename_repo("rename-hunkset");
    repo.hunk_ok(&["split", r#"file("dst.txt")"#, "hunkset rename"]);

    let got = repo.jj_ok(&["file", "show", "-r", "@-", "dst.txt"]);
    assert_eq!(got, edited, "hunkset selection lost the renamed content");
}

// ---------------------------------------------------------------------------
// A JSON spec must be checked against the diff it will be applied to, not just
// parsed. `selects_nothing` is structural: it sees a non-empty id list and is
// satisfied, even when no such hunk exists, so a typo produced an empty commit
// at exit 0 -- exactly what --allow-empty was introduced to prevent.
// ---------------------------------------------------------------------------

#[test]
fn out_of_range_hunk_index_is_rejected() {
    let repo = strict_repo("resolve-bad-index");
    let err = repo.hunk_fail(&[
        "split",
        r#"{"files": {"a.py": {"hunks": [99]}}, "default": "reset"}"#,
        "out of range",
    ]);
    assert!(err.contains("99"), "error should name the bad index: {err}");
    assert!(
        !repo
            .log_descriptions()
            .iter()
            .any(|d| d.contains("out of range")),
        "an empty commit was created from an out-of-range index"
    );
}

#[test]
fn spec_path_that_is_not_in_the_diff_is_rejected() {
    let repo = strict_repo("resolve-bad-path");
    let err = repo.hunk_fail(&[
        "split",
        r#"{"files": {"nope.txt": {"action": "keep"}}, "default": "reset"}"#,
        "bad path",
    ]);
    assert!(err.contains("nope.txt"), "error should name the path: {err}");
    assert!(
        !repo.log_descriptions().iter().any(|d| d.contains("bad path")),
        "an empty commit was created from an unknown path"
    );
}

#[test]
fn unknown_hunk_id_in_a_spec_is_rejected() {
    let repo = strict_repo("resolve-bad-id");
    let bogus = format!("hunk-{}", "a".repeat(64));
    let spec = format!(
        r#"{{"files": {{"a.py": {{"ids": ["{bogus}"]}}}}, "default": "reset"}}"#
    );
    let err = repo.hunk_fail(&["split", &spec, "bad id"]);
    assert!(err.contains(&bogus), "error should name the bad id: {err}");
    assert!(
        !repo.log_descriptions().iter().any(|d| d.contains("bad id")),
        "an empty commit was created from an unknown hunk id"
    );
}

#[test]
fn allow_empty_still_bypasses_the_resolution_check() {
    let repo = strict_repo("resolve-allow-empty");
    repo.hunk_ok(&[
        "split",
        "--allow-empty",
        r#"{"files": {"nope.txt": {"action": "keep"}}, "default": "reset"}"#,
        "intentionally empty",
    ]);
    assert!(
        repo.log_descriptions()
            .iter()
            .any(|d| d.contains("intentionally empty")),
        "--allow-empty must remain the escape hatch"
    );
}

#[test]
fn an_explicitly_empty_selection_is_not_a_resolution_error() {
    // Blanking out the ids you do not want is the documented workflow; only
    // ids and indices that cannot exist are errors.
    let repo = strict_repo("resolve-empty-entry");
    let py_id = first_hunk_id(&repo, "a.py");
    let spec = format!(
        r#"{{"files": {{"a.rs": {{"ids": []}}, "a.py": {{"ids": ["{py_id}"]}}}}, "default": "reset"}}"#
    );
    repo.hunk_ok(&["split", &spec, "python only"]);
    assert!(
        repo.log_descriptions()
            .iter()
            .any(|d| d.contains("python only")),
        "a blanked-out entry must not be rejected"
    );
}

#[test]
fn an_entry_that_keeps_nothing_may_name_an_unknown_path() {
    // A boilerplate "always reset these" list cannot cause an empty commit, so
    // a stale path in one is not worth failing over.
    let repo = strict_repo("resolve-harmless-path");
    let py_id = first_hunk_id(&repo, "a.py");
    let spec = format!(
        r#"{{"files": {{"stale.txt": {{"action": "reset"}}, "vanished.txt": {{"ids": []}}, "a.py": {{"ids": ["{py_id}"]}}}}, "default": "reset"}}"#
    );
    repo.hunk_ok(&["split", &spec, "python only, stale entries"]);
    assert!(
        repo.log_descriptions()
            .iter()
            .any(|d| d.contains("python only, stale entries")),
        "a harmless stale entry must not be rejected"
    );
}

/// The first hunk id `list` reports for `path`, read from `--format text`.
fn first_hunk_id(repo: &TestRepo, path: &str) -> String {
    let out = repo.hunk_ok(&["list", "--format", "text"]);
    let mut current = String::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("  hunk ") {
            if current == path {
                return rest
                    .split_whitespace()
                    .nth(2)
                    .expect("hunk line has no id")
                    .to_string();
            }
        } else if !line.starts_with(' ') {
            // File header: "<status char> <path>[ (from -> to)]".
            current = line.split_whitespace().nth(1).unwrap_or("").to_string();
        }
    }
    panic!("no hunk found for {path} in:\n{out}");
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
