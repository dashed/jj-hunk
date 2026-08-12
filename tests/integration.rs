use std::collections::HashMap;
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
        // The directory must be unique per TestRepo, not per name. Keying it on
        // name+pid alone means two tests that happen to pick the same name share
        // a directory, and since the suite runs in parallel one wipes the
        // other's repo mid-run -- a flake that reproduces roughly one run in
        // three and vanishes under --test-threads=1. The counter makes name
        // collisions harmless instead of merely unlikely.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "jj-hunk-test-{}-{}-{}",
            name,
            std::process::id(),
            seq
        ));
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

    /// Run with extra environment variables set on top of the usual ones.
    ///
    /// `JJ_HUNK_ERROR_FORMAT` is the only way to opt a *nested* invocation into
    /// structured errors — the mutating verbs re-enter this binary through
    /// `jj --tool=jj-hunk`, which no flag reaches — so it has to be testable.
    fn hunk_with_env(&self, env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
        let mut command = Command::new(jj_hunk_bin());
        command
            .args(args)
            .current_dir(&self.dir)
            .env("JJ_USER", "Test User")
            .env("JJ_EMAIL", "test@example.com")
            .env("JJ_CONFIG", &self.config_path)
            .env("PATH", path_with_jj_hunk());
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("failed to run jj-hunk")
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

    /// A file's whole content as of `rev`.
    fn file_at(&self, rev: &str, path: &str) -> String {
        self.jj_ok(&["file", "show", "-r", rev, &format!("file:{path}")])
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
// Executable bit. A mode change is not a hunk, so it cannot be selected on its
// own: it follows the content. `apply_hunk_selection` rewrote the right-hand
// file with `fs::write`, which preserves that file's mode, so an unselected
// `chmod +x` rode along in the split commit.
//
// The converse -- a chmod riding along with hunks that *were* selected -- is
// under "The executable bit is not a hunk" at the end of this file.
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

// A test asserting that a chmod is stripped from a file whose hunks *were*
// selected used to live here. That is the leak's mirror image, not another
// instance of it: it left the chmod behind in the working copy for a split,
// and discarded it outright for a `diffedit`, which keeps no remainder. See
// `a_chmod_rides_along_with_the_hunks_it_accompanies`.

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
fn allow_empty_does_not_switch_off_the_resolution_check() {
    // The flag used to gate this check as well, so a spec that referred to
    // nothing at all still produced a commit at exit 0 -- the very outcome the
    // check exists to make loud. `--allow-empty` says an empty RESULT is
    // acceptable; it does not say the names in the spec need not exist.
    let repo = strict_repo("resolve-allow-empty");
    let err = repo.hunk_fail(&[
        "split",
        "--allow-empty",
        r#"{"files": {"nope.txt": {"action": "keep"}}, "default": "reset"}"#,
        "intentionally empty",
    ]);
    assert!(err.contains("nope.txt"), "the error must name the path: {err}");
    assert!(
        !repo
            .log_descriptions()
            .iter()
            .any(|d| d.contains("intentionally empty")),
        "--allow-empty let a spec that refers to nothing create a commit"
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

// ---------------------------------------------------------------------------
// CLI and robustness fixes
// ---------------------------------------------------------------------------

/// Run jj-hunk with extra environment overrides.
///
/// A free function rather than a `TestRepo` method so the shared `impl` block
/// stays untouched. `envs` is applied last, so it wins over the defaults.
fn hunk_with_env(repo: &TestRepo, args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(jj_hunk_bin());
    cmd.args(args)
        .current_dir(repo.path())
        .env("JJ_USER", "Test User")
        .env("JJ_EMAIL", "test@example.com")
        .env("JJ_CONFIG", &repo.config_path)
        .env("PATH", path_with_jj_hunk());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().expect("failed to run jj-hunk")
}

/// A repo whose single file has 20 four-byte lines, with the first and the
/// last changed. Untruncated that is two hunks; any cut before line 20 drops
/// the second.
fn truncation_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    let before: String = (1..=20).map(|i| format!("a{:02}\n", i)).collect();
    repo.write_file("f.txt", &before);
    repo.jj_ok(&["new", "-m", "base"]);

    let after = before.replace("a01\n", "Z01\n").replace("a20\n", "Z20\n");
    repo.write_file("f.txt", &after);
    repo
}

#[test]
fn without_truncation_both_ends_of_the_file_are_diffed() {
    let repo = truncation_repo("trunc-none");
    let out = repo.hunk_ok(&["list", "--files", "--format", "text"]);
    assert!(out.contains("(2 hunks)"), "{out}");
    assert!(!out.contains("[truncated]"), "{out}");
}

#[test]
fn max_lines_truncates_before_diffing() {
    let repo = truncation_repo("trunc-lines");
    let out = repo.hunk_ok(&["list", "--files", "--format", "text", "--max-lines", "5"]);
    // Only the line-1 change survives the cut.
    assert!(out.contains("(1 hunks)"), "{out}");
    assert!(out.contains("[truncated]"), "{out}");
}

#[test]
fn max_bytes_truncates_before_diffing() {
    let repo = truncation_repo("trunc-bytes");
    // 20 bytes == the first five 4-byte lines.
    let out = repo.hunk_ok(&["list", "--files", "--format", "text", "--max-bytes", "20"]);
    assert!(out.contains("(1 hunks)"), "{out}");
    assert!(out.contains("[truncated]"), "{out}");
}

#[test]
fn a_limit_larger_than_the_file_does_not_mark_it_truncated() {
    let repo = truncation_repo("trunc-slack");
    let out = repo.hunk_ok(&["list", "--files", "--format", "text", "--max-lines", "500"]);
    assert!(out.contains("(2 hunks)"), "{out}");
    assert!(!out.contains("[truncated]"), "{out}");
}

#[test]
fn truncation_is_reported_in_json_output() {
    let repo = truncation_repo("trunc-json");
    let out = repo.hunk_ok(&["list", "--max-lines", "5"]);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["files"][0]["truncated"], serde_json::json!(true), "{out}");
}

/// Hunk ids are content-addressed over the text they were diffed from, so ids
/// computed from truncated text do not exist in the real diff. Emitting them
/// into a spec template hands over a file that `split` is guaranteed to
/// reject, so refuse to produce it.
#[test]
fn spec_template_refuses_files_whose_content_was_truncated() {
    let repo = truncation_repo("trunc-template");
    let err = repo.hunk_fail(&["list", "--spec-template", "--max-lines", "5"]);
    assert!(err.contains("f.txt"), "error should name the file: {err}");
    assert!(
        err.contains("truncat"),
        "error should explain truncation: {err}"
    );
}

/// Everything one `list` prints must describe one view of the diff. The
/// hunkset used to be evaluated against the whole file while the listing it
/// filtered was truncated, so any hunk the cut reshaped had an id the selector
/// had never seen and vanished -- `--files` and `--spec` disagreed about how
/// many hunks the same command was looking at.
///
/// A byte cut landing mid-line is what exposes it: it rewrites the boundary
/// hunk rather than merely dropping later ones, so the id changes.
#[test]
fn a_selector_filters_the_same_view_that_is_listed() {
    let repo = TestRepo::new("trunc-spec-view");
    let before: String = (1..=20).map(|i| format!("line {}\n", i)).collect();
    repo.write_file("big.txt", &before);
    repo.jj_ok(&["new", "-m", "base"]);
    let after = before
        .replace("line 2\n", "line 2 CHANGED\n")
        .replace("line 18\n", "line 18 CHANGED\n");
    repo.write_file("big.txt", &after);

    // 40 bytes lands inside "line 6\n", which reshapes the hunk at the cut.
    let count = |args: &[&str]| -> usize {
        repo.hunk_ok(args)
            .lines()
            .filter(|l| l.trim_start().starts_with("hunk "))
            .count()
    };

    let plain = count(&["list", "--max-bytes", "40", "--format", "text"]);
    assert_eq!(plain, 2, "the truncated view should have two hunks");

    let summary = repo.hunk_ok(&["list", "--files", "--max-bytes", "40", "--format", "text"]);
    assert!(
        summary.contains("(2 hunks)"),
        "summary should agree with the listing: {summary}"
    );

    for selector in ["all()", r#"file("big.txt")"#] {
        let selected = count(&[
            "list", "--max-bytes", "40", "--spec", selector, "--format", "text",
        ]);
        assert_eq!(
            selected, plain,
            "`--spec {selector}` should keep every hunk of the view it filters"
        );
    }
}

/// A selector is still applied against the whole file when no truncation was
/// asked for -- the common case must not have moved.
#[test]
fn a_selector_without_truncation_sees_the_whole_file() {
    let repo = truncation_repo("trunc-spec-whole");
    let all = repo.hunk_ok(&["list", "--spec", "all()", "--format", "text"]);
    assert_eq!(
        all.lines()
            .filter(|l| l.trim_start().starts_with("hunk "))
            .count(),
        2,
        "{all}"
    );
}

#[test]
fn spec_template_still_works_when_nothing_was_actually_truncated() {
    let repo = truncation_repo("trunc-template-ok");
    let out = repo.hunk_ok(&["list", "--spec-template", "--max-lines", "500"]);
    assert!(out.contains("f.txt"), "{out}");
}

// --- commit messages that begin with a dash ---------------------------------

#[test]
fn split_accepts_a_message_starting_with_a_dash() {
    let repo = TestRepo::new("dash-split");
    repo.write_file("a.txt", "one\n");
    repo.jj_ok(&["new", "-m", "base"]);
    repo.write_file("a.txt", "two\n");

    repo.hunk_ok(&["split", "all()", "--", "--not-a-flag"]);

    let log = repo.log_descriptions();
    assert!(
        log.iter().any(|d| d == "--not-a-flag"),
        "a message beginning with a dash should round-trip: {log:?}"
    );
}

#[test]
fn commit_accepts_a_message_starting_with_a_dash() {
    let repo = TestRepo::new("dash-commit");
    repo.write_file("a.txt", "one\n");
    repo.jj_ok(&["new", "-m", "base"]);
    repo.write_file("a.txt", "two\n");

    repo.hunk_ok(&["commit", "all()", "--", "-m"]);

    let log = repo.log_descriptions();
    assert!(
        log.iter().any(|d| d == "-m"),
        "a message that is itself a flag name should round-trip: {log:?}"
    );
}

// --- temp spec file ---------------------------------------------------------

/// A spec that parses as neither hunkset nor Spec skips every pre-flight `jj`
/// call, so the run reaches the point where the temp file has been written and
/// `jj` is spawned. With `jj` unreachable that spawn fails, and the file used
/// to be left behind.
const UNPARSEABLE_SPEC: &str = r#"{"files": {"a.txt": {"bogus_field": 1}}}"#;

#[test]
fn temp_spec_file_is_cleaned_up_when_jj_cannot_be_spawned() {
    let repo = TestRepo::new("tmp-leak");
    repo.write_file("a.txt", "one\n");
    repo.jj_ok(&["new", "-m", "base"]);
    repo.write_file("a.txt", "two\n");

    let tmp = repo.path().join("scratch-tmp");
    std::fs::create_dir_all(&tmp).unwrap();

    let out = hunk_with_env(
        &repo,
        &["split", UNPARSEABLE_SPEC, "msg"],
        &[("TMPDIR", tmp.to_str().unwrap()), ("PATH", "/nonexistent")],
    );
    assert!(!out.status.success(), "expected the jj spawn to fail");

    let leftover: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        leftover.is_empty(),
        "temp spec file leaked when jj could not be spawned: {leftover:?}"
    );
}

#[test]
fn temp_spec_file_error_names_the_directory() {
    let repo = TestRepo::new("tmp-badenv");
    repo.write_file("a.txt", "one\n");
    repo.jj_ok(&["new", "-m", "base"]);
    repo.write_file("a.txt", "two\n");

    let missing = "/nonexistent/jj-hunk-temp-dir";
    let out = hunk_with_env(
        &repo,
        &["split", UNPARSEABLE_SPEC, "msg"],
        &[("TMPDIR", missing)],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(missing),
        "error should name the unusable temp dir: {err}"
    );
}

// --- comma-containing paths in --include / --exclude ------------------------

/// Two files, one of which has a comma in its name, both modified.
fn comma_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("plain.txt", "x\n");
    repo.write_file("has,comma.txt", "x\n");
    repo.jj_ok(&["new", "-m", "base"]);
    repo.write_file("plain.txt", "y\n");
    repo.write_file("has,comma.txt", "y\n");
    repo
}

#[test]
fn include_can_name_a_path_containing_a_comma() {
    let repo = comma_repo("comma-include");
    let out = repo.hunk_ok(&[
        "list",
        "--files",
        "--format",
        "text",
        "--include",
        "has,comma.txt",
    ]);
    assert!(out.contains("has,comma.txt"), "{out}");
    assert!(!out.contains("plain.txt"), "{out}");
}

#[test]
fn exclude_can_name_a_path_containing_a_comma() {
    let repo = comma_repo("comma-exclude");
    let out = repo.hunk_ok(&[
        "list",
        "--files",
        "--format",
        "text",
        "--exclude",
        "has,comma.txt",
    ]);
    assert!(out.contains("plain.txt"), "{out}");
    assert!(!out.contains("has,comma.txt"), "{out}");
}

#[test]
fn include_stays_repeatable() {
    let repo = TestRepo::new("comma-repeat");
    repo.write_file("a.txt", "x\n");
    repo.write_file("b.txt", "x\n");
    repo.write_file("c.txt", "x\n");
    repo.jj_ok(&["new", "-m", "base"]);
    repo.write_file("a.txt", "y\n");
    repo.write_file("b.txt", "y\n");
    repo.write_file("c.txt", "y\n");

    let out = repo.hunk_ok(&[
        "list", "--files", "--format", "text", "-i", "a.txt", "-i", "b.txt",
    ]);
    assert!(out.contains("a.txt"), "{out}");
    assert!(out.contains("b.txt"), "{out}");
    assert!(!out.contains("c.txt"), "{out}");
}

// --- binary files in spec templates -----------------------------------------

/// A repo with one binary file and one text file, both modified.
fn binary_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    std::fs::write(repo.path().join("bin.dat"), [0u8, 1, 2, 0, 255, 254]).unwrap();
    repo.write_file("t.txt", "a\nb\n");
    repo.jj_ok(&["new", "-m", "base"]);
    std::fs::write(repo.path().join("bin.dat"), [0u8, 1, 2, 0, 255, 253, 9, 9]).unwrap();
    repo.write_file("t.txt", "A\nb\n");
    repo
}

/// `--binary include` used to put hunk ids for the binary file in the template.
/// `select` reads files as UTF-8, so consuming that template died with a bare
/// "stream did not contain valid UTF-8" naming no file. A binary change cannot
/// be split hunk-wise at all, so the template keeps it wholesale instead.
#[test]
fn spec_template_with_binary_include_round_trips_through_split() {
    let repo = binary_repo("bin-template");

    let template = repo.hunk_ok(&["list", "--spec-template", "--binary", "include"]);
    let spec_path = repo.path().join("t.json");
    std::fs::write(&spec_path, &template).unwrap();

    repo.hunk_ok(&["split", "-f", spec_path.to_str().unwrap(), "everything"]);

    let log = repo.log_descriptions();
    assert!(log.iter().any(|d| d == "everything"), "{log:?}");

    let committed = repo.changed_files("@-");
    assert!(
        committed.iter().any(|l| l.contains("bin.dat")),
        "binary change should be in the commit: {committed:?}"
    );
    assert!(
        committed.iter().any(|l| l.contains("t.txt")),
        "text change should be in the commit: {committed:?}"
    );

    // The point of keeping a binary file wholesale: its bytes go in untouched.
    // Routing them through a lossy String would land replacement characters in
    // the commit, which is worse than the error this replaced.
    let out = Command::new("jj")
        .args(["file", "show", "-r", "@-", "bin.dat"])
        .current_dir(repo.path())
        .env("JJ_USER", "Test User")
        .env("JJ_EMAIL", "test@example.com")
        .env("JJ_CONFIG", &repo.config_path)
        .env("PATH", path_with_jj_hunk())
        .output()
        .expect("jj file show failed");
    assert_eq!(
        out.stdout,
        vec![0u8, 1, 2, 0, 255, 253, 9, 9],
        "binary content was not preserved byte for byte"
    );
}

#[test]
fn binary_files_are_kept_wholesale_in_a_spec_template() {
    let repo = binary_repo("bin-template-shape");
    let template = repo.hunk_ok(&["list", "--spec-template", "--binary", "include"]);
    let json: serde_json::Value = serde_json::from_str(&template).unwrap();
    assert_eq!(
        json["files"]["bin.dat"]["action"],
        serde_json::json!("keep"),
        "binary entry should be an action, not hunk ids: {template}"
    );
    assert!(
        json["files"]["t.txt"]["ids"].is_array(),
        "text entry should still carry ids: {template}"
    );
}

/// A hand-written spec can still point hunk ids at a non-UTF-8 file. That is a
/// real error, but it used to name nothing at all.
#[test]
fn a_spec_selecting_hunks_in_a_binary_file_names_the_file() {
    let repo = binary_repo("bin-handwritten");

    let listing: serde_json::Value =
        serde_json::from_str(&repo.hunk_ok(&["list", "--binary", "include"])).unwrap();
    let bin_id = listing["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "bin.dat")
        .expect("bin.dat should be listed")["hunks"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let spec = format!(r#"{{"files": {{"bin.dat": {{"ids": ["{bin_id}"]}}}}}}"#);
    let spec_path = repo.path().join("hand.json");
    std::fs::write(&spec_path, spec).unwrap();

    let err = repo.hunk_fail(&[
        "split",
        "-f",
        spec_path.to_str().unwrap(),
        "msg",
        "--allow-empty",
    ]);
    assert!(
        err.contains("bin.dat"),
        "error should name the offending file: {err}"
    );
}

// ---------------------------------------------------------------------------
// Hunk ids: one hunk per id, and an abbreviation you can actually type.
// ---------------------------------------------------------------------------

/// Every `(path, hunk)` pair in `list --format json`, as (path, id, short_id).
fn listed_ids(repo: &TestRepo, args: &[&str]) -> Vec<(String, String, String)> {
    let mut argv = vec!["list", "--format", "json"];
    argv.extend_from_slice(args);
    let listing: serde_json::Value = serde_json::from_str(&repo.hunk_ok(&argv)).unwrap();

    let mut out = Vec::new();
    for file in listing["files"].as_array().unwrap() {
        let path = file["path"].as_str().unwrap().to_string();
        for hunk in file["hunks"].as_array().unwrap() {
            out.push((
                path.clone(),
                hunk["id"].as_str().unwrap().to_string(),
                hunk["short_id"].as_str().unwrap().to_string(),
            ));
        }
    }
    out
}

/// Two byte-identical blocks in one file, each wrapped in identical lines, so
/// the hunks agree on everything the id is hashed from.
fn duplicate_block_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file(
        "dup.json",
        "{\n  \"a\": {\n    \"x\": 1,\n    \"drop\": 0\n  },\n  \"z\": 9,\n  \"b\": {\n    \"x\": 1,\n    \"drop\": 0\n  }\n}\n",
    );
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file(
        "dup.json",
        "{\n  \"a\": {\n    \"x\": 1\n  },\n  \"z\": 9,\n  \"b\": {\n    \"x\": 1\n  }\n}\n",
    );
    repo
}

#[test]
fn identical_hunks_in_one_file_do_not_share_an_id() {
    let repo = duplicate_block_repo("dup-ids");
    let listed = listed_ids(&repo, &[]);

    assert_eq!(listed.len(), 2, "fixture should produce two hunks: {listed:?}");
    assert_ne!(listed[0].1, listed[1].1, "full ids collided: {listed:?}");
    assert_ne!(listed[1].2, listed[0].2, "short ids collided: {listed:?}");
}

#[test]
fn an_id_selects_one_of_two_identical_hunks_through_split() {
    let repo = duplicate_block_repo("dup-split");
    let listed = listed_ids(&repo, &[]);
    let second = &listed[1].2;

    // Keep only the second of the two identical deletions.
    let spec = format!(r#"{{"files": {{"dup.json": {{"ids": ["{second}"]}}}}}}"#);
    repo.hunk_ok(&["split", &spec, "second only"]);

    // The first block's `drop` is still in the working copy; the second's is
    // gone. Sharing one id took both.
    let remaining = repo.jj_ok(&["diff", "--git"]);
    assert_eq!(
        remaining.matches("-    \"drop\": 0").count(),
        1,
        "exactly one deletion should be left over:\n{remaining}"
    );
}

#[test]
fn a_line_range_selector_does_not_reach_an_identical_later_hunk() {
    // `to_spec` turns matched hunks into ids, so a shared id let any selector
    // that matched one hunk quietly take its twin as well.
    let repo = duplicate_block_repo("dup-range");
    let out = repo.hunk_ok(&["list", "--format", "text", "--spec", "before_line(1..5)"]);

    assert_eq!(
        out.matches("  hunk ").count(),
        1,
        "only the first block is in lines 1..5:\n{out}"
    );
}

#[test]
fn the_same_edit_in_two_files_does_not_share_an_id() {
    let repo = TestRepo::new("cross-file-ids");
    repo.write_file("one.txt", "alpha\nbravo\ncharlie\n");
    repo.write_file("two.txt", "alpha\nbravo\ncharlie\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("one.txt", "alpha\nBRAVO\ncharlie\n");
    repo.write_file("two.txt", "alpha\nBRAVO\ncharlie\n");

    let listed = listed_ids(&repo, &[]);
    assert_eq!(listed.len(), 2);
    assert_ne!(
        listed[0].1, listed[1].1,
        "the same edit to two files must not be one id: {listed:?}"
    );
}

#[test]
fn json_keeps_the_full_id_and_adds_the_short_one() {
    let repo = duplicate_block_repo("id-json-shape");

    for (path, id, short_id) in listed_ids(&repo, &[]) {
        let hex = id.strip_prefix("hunk-").expect("full id keeps the prefix");
        assert_eq!(hex.len(), 64, "{path}: json `id` must stay the full digest");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));

        let short_hex = short_id.strip_prefix("hunk-").unwrap();
        assert!(short_hex.len() >= 8, "{path}: {short_id} is under the floor");
        assert!(id.starts_with(&short_id), "{path}: {short_id} must prefix {id}");
    }
}

#[test]
fn text_and_diff_output_print_the_short_id() {
    let repo = duplicate_block_repo("id-short-output");
    let listed = listed_ids(&repo, &[]);

    let text = repo.hunk_ok(&["list", "--format", "text"]);
    let diff = repo.hunk_ok(&["list", "--format", "diff"]);

    for (_, id, short_id) in &listed {
        assert!(text.contains(short_id.as_str()), "text should show {short_id}:\n{text}");
        assert!(!text.contains(id.as_str()), "text should not show the full id:\n{text}");
    }
    // The diff header names the first hunk of each block, unellipsised.
    assert!(
        diff.contains(&format!("[{}]", listed[0].2)),
        "diff header should carry the short id:\n{diff}"
    );
}

#[test]
fn a_short_id_from_list_works_in_a_hunkset_and_in_a_spec() {
    let repo = duplicate_block_repo("id-short-input");
    let listed = listed_ids(&repo, &[]);
    let (_, full, short) = &listed[0];

    let hunkset = repo.hunk_ok(&["list", "--format", "text", "--spec", &format!(r#"id("{short}")"#)]);
    assert_eq!(hunkset.matches("  hunk ").count(), 1, "{hunkset}");

    let by_full = repo.hunk_ok(&["list", "--format", "text", "--spec", &format!(r#"id("{full}")"#)]);
    assert_eq!(by_full, hunkset, "short and full ids must name the same hunk");

    let spec = format!(r#"{{"files": {{"dup.json": {{"ids": ["{short}"]}}}}}}"#);
    let by_spec = repo.hunk_ok(&["list", "--format", "text", "--spec", &spec]);
    assert_eq!(by_spec, hunkset, "a spec must accept the short form too");
}

/// A repo whose diff has more hunks than there are hex digits, so by pigeonhole
/// at least two ids start with the same one -- a prefix that is genuinely
/// ambiguous, without hardcoding any hash.
fn many_hunk_repo(name: &str) -> TestRepo {
    const HUNKS: usize = 24;
    let repo = TestRepo::new(name);

    // Four lines apart, so no two hunks share a context window or get merged.
    let base: String = (0..HUNKS).map(|i| format!("line{i}\npad\npad\npad\n")).collect();
    let edited: String = (0..HUNKS).map(|i| format!("LINE{i}\npad\npad\npad\n")).collect();
    repo.write_file("many.txt", &base);
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("many.txt", &edited);
    repo
}

#[test]
fn short_ids_stay_unique_across_a_diff_with_many_hunks() {
    let repo = many_hunk_repo("id-many");
    let listed = listed_ids(&repo, &[]);
    assert!(listed.len() > 16, "need more hunks than hex digits: {}", listed.len());

    let shorts: std::collections::HashSet<&str> = listed.iter().map(|(_, _, s)| s.as_str()).collect();
    assert_eq!(shorts.len(), listed.len(), "short ids must stay unique");
}

/// The one-hex-digit prefix shared by at least two hunks. Guaranteed to exist
/// once the diff has more than 16 hunks.
fn ambiguous_prefix(listed: &[(String, String, String)]) -> String {
    let mut seen: HashMap<char, usize> = HashMap::new();
    for (_, id, _) in listed {
        let first = id.strip_prefix("hunk-").unwrap().chars().next().unwrap();
        *seen.entry(first).or_default() += 1;
    }
    let (digit, _) = seen
        .iter()
        .find(|(_, count)| **count > 1)
        .expect("more hunks than hex digits guarantees a shared first digit");
    format!("hunk-{digit}")
}

#[test]
fn an_ambiguous_id_prefix_is_an_error_everywhere_it_is_accepted() {
    let repo = many_hunk_repo("id-ambiguous");
    let listed = listed_ids(&repo, &[]);
    let prefix = ambiguous_prefix(&listed);

    let hunkset_err = repo.hunk_fail(&["list", "--spec", &format!(r#"id("{prefix}")"#)]);
    assert!(
        hunkset_err.to_lowercase().contains("ambiguous"),
        "id() should reject an ambiguous prefix: {hunkset_err}"
    );

    let spec = format!(r#"{{"files": {{"many.txt": {{"ids": ["{prefix}"]}}}}}}"#);
    let list_err = repo.hunk_fail(&["list", "--spec", &spec]);
    assert!(
        list_err.to_lowercase().contains("ambiguous"),
        "a spec should reject an ambiguous prefix: {list_err}"
    );

    // Most important: it must never reach `select` and quietly keep both.
    let split_err = repo.hunk_fail(&["split", &spec, "msg"]);
    assert!(
        split_err.to_lowercase().contains("ambiguous"),
        "split should refuse an ambiguous prefix: {split_err}"
    );
}

#[test]
fn a_spec_template_of_a_many_hunk_diff_round_trips() {
    let repo = many_hunk_repo("id-template-roundtrip");
    let before = repo.hunk_ok(&["list", "--format", "diff"]);

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    // Outside the repo: a spec file dropped inside it is itself a change, and
    // this test asserts the working copy is empty afterwards.
    let template_path = std::env::temp_dir().join(format!("jj-hunk-tmpl-{}.json", std::process::id()));
    std::fs::write(&template_path, &template).unwrap();

    repo.hunk_ok(&["split", "-f", template_path.to_str().unwrap(), "all of it"]);
    let _ = std::fs::remove_file(&template_path);

    // Everything the template named ended up in the commit, and nothing is
    // left behind.
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "");
    let committed = repo.hunk_ok(&["list", "-r", "@-", "--format", "diff"]);
    assert_eq!(committed, before, "the split commit should hold the whole diff");
}

// ---------------------------------------------------------------------------
// diffedit / restore
//
// These two verbs disagree with each other, and with `split`/`squash`, about
// what a named hunk *means*:
//
//   split     the named hunks go into the split-off commit
//   squash    the named hunks move to the destination
//   diffedit  the named hunks are KEPT
//   restore   the named hunks are UNDONE
//
// The inversion is not a special case in jj-hunk: `jj restore` hands its diff
// editor the destination on the left and the source on the right, which is the
// reverse of `jj diff -r`, so "keep what is named" reads as "undo it".
// ---------------------------------------------------------------------------

/// `f.txt` before the change under test.
const TWO_HUNK_BASE: &str = "L1\nL2\nL3\nL4\nL5\nL6\nL7\n";
/// `f.txt` after it: two hunks, far enough apart not to merge into one.
const TWO_HUNK_CHANGED: &str = "X1\nL2\nL3\nL4\nL5\nL6\nX7\n";
/// Only the first hunk applied.
const TWO_HUNK_FIRST_ONLY: &str = "X1\nL2\nL3\nL4\nL5\nL6\nL7\n";
/// Only the second hunk applied.
const TWO_HUNK_SECOND_ONLY: &str = "L1\nL2\nL3\nL4\nL5\nL6\nX7\n";

/// A selector naming the first hunk and nothing else. `content()` looks at
/// both sides of a hunk, so the same expression picks out the same region
/// whichever way round the diff is presented.
const FIRST_HUNK: &str = r#"content(substring:"X1")"#;

/// A stack whose `@-` changes two well-separated lines of `f.txt`, so a
/// selector can name one hunk and leave the other alone.
///
/// ```text
/// @--  "base"    L1 L2 L3 L4 L5 L6 L7
/// @-   "change"  X1 L2 L3 L4 L5 L6 X7
/// @               (empty working copy)
/// ```
fn two_hunk_stack(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("f.txt", TWO_HUNK_BASE);
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("f.txt", TWO_HUNK_CHANGED);
    repo.jj_ok(&["commit", "-m", "change"]);
    repo
}

/// The two hunks of `f.txt` as `(id, short_id)`, in hunk order, as seen from
/// whichever side `args` selects.
fn two_hunk_ids(repo: &TestRepo, args: &[&str]) -> Vec<(String, String)> {
    let ids: Vec<(String, String)> = listed_ids(repo, args)
        .into_iter()
        .filter(|(path, _, _)| path == "f.txt")
        .map(|(_, id, short)| (id, short))
        .collect();
    assert_eq!(ids.len(), 2, "fixture should have two hunks: {ids:?}");
    ids
}

/// A spec naming exactly one hunk of `f.txt`.
fn one_hunk_spec(id: &str) -> String {
    format!(r#"{{"files": {{"f.txt": {{"ids": ["{id}"]}}}}, "default": "reset"}}"#)
}

#[test]
fn diffedit_keeps_the_hunks_it_names() {
    let repo = two_hunk_stack("diffedit-keeps");
    repo.hunk_ok(&["diffedit", FIRST_HUNK, "-r", "@-"]);
    assert_eq!(
        repo.file_at("@-", "f.txt"),
        TWO_HUNK_FIRST_ONLY,
        "diffedit must keep the named hunk and drop the other"
    );
}

#[test]
fn restore_undoes_the_hunks_it_names() {
    let repo = two_hunk_stack("restore-undoes");
    repo.hunk_ok(&["restore", FIRST_HUNK, "-c", "@-"]);
    assert_eq!(
        repo.file_at("@-", "f.txt"),
        TWO_HUNK_SECOND_ONLY,
        "restore must undo the named hunk and leave the other alone"
    );
}

/// The two verbs are near-inverses. One selector, one starting state, two
/// results that must be exact complements -- an assertion that merely checked
/// "something changed" would pass with the two implementations swapped.
#[test]
fn diffedit_and_restore_split_a_change_into_complementary_halves() {
    let kept = {
        let repo = two_hunk_stack("complement-diffedit");
        repo.hunk_ok(&["diffedit", FIRST_HUNK, "-r", "@-"]);
        repo.file_at("@-", "f.txt")
    };
    let undone = {
        let repo = two_hunk_stack("complement-restore");
        repo.hunk_ok(&["restore", FIRST_HUNK, "-c", "@-"]);
        repo.file_at("@-", "f.txt")
    };

    assert_eq!(kept, TWO_HUNK_FIRST_ONLY);
    assert_eq!(undone, TWO_HUNK_SECOND_ONLY);

    // Nothing fell through the gap between them: on every line the original
    // change touched, exactly one half carries the new text and the other
    // carries the old one.
    let lines = TWO_HUNK_BASE
        .lines()
        .zip(TWO_HUNK_CHANGED.lines())
        .zip(kept.lines().zip(undone.lines()));
    for (line_no, ((base, changed), (a, b))) in lines.enumerate() {
        if base == changed {
            assert_eq!(
                (a, b),
                (base, base),
                "line {line_no} was not part of the change"
            );
            continue;
        }
        assert_eq!(
            [a, b].iter().filter(|l| **l == changed).count(),
            1,
            "line {line_no}: exactly one half must carry the change ({a:?} / {b:?})"
        );
        assert_eq!(
            [a, b].iter().filter(|l| **l == base).count(),
            1,
            "line {line_no}: exactly one half must carry the original ({a:?} / {b:?})"
        );
    }
}

/// The same inversion on the working copy, where neither verb is given a
/// revision at all: `diffedit` keeps `a.txt`'s change and drops `b.txt`'s,
/// `restore` does exactly the opposite.
#[test]
fn diffedit_without_a_revision_keeps_the_named_working_copy_change() {
    let repo = TestRepo::new("diffedit-working-copy");
    repo.write_file("a.txt", "a\n");
    repo.write_file("b.txt", "b\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.txt", "A\n");
    repo.write_file("b.txt", "B\n");

    repo.hunk_ok(&["diffedit", r#"file("a.txt")"#]);

    assert_eq!(repo.file_at("@", "a.txt"), "A\n", "the named change is kept");
    assert_eq!(
        repo.file_at("@", "b.txt"),
        "b\n",
        "the unnamed change is dropped"
    );
}

#[test]
fn restore_without_a_target_undoes_the_named_working_copy_change() {
    let repo = TestRepo::new("restore-working-copy");
    repo.write_file("a.txt", "a\n");
    repo.write_file("b.txt", "b\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.txt", "A\n");
    repo.write_file("b.txt", "B\n");

    repo.hunk_ok(&["restore", r#"file("a.txt")"#]);

    assert_eq!(repo.file_at("@", "a.txt"), "a\n", "the named change is undone");
    assert_eq!(
        repo.file_at("@", "b.txt"),
        "B\n",
        "the unnamed change is left alone"
    );
}

#[test]
fn restore_from_alone_restores_into_the_working_copy() {
    let repo = TestRepo::new("restore-from-alone");
    repo.write_file("a.txt", "a\n");
    repo.write_file("b.txt", "b\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.txt", "A\n");
    repo.write_file("b.txt", "B\n");

    // `--into` defaults to the working copy.
    repo.hunk_ok(&["restore", r#"file("a.txt")"#, "--from", "@-"]);

    assert_eq!(repo.file_at("@", "a.txt"), "a\n");
    assert_eq!(repo.file_at("@", "b.txt"), "B\n");
}

// --- naming hunks by id and by index --------------------------------------

#[test]
fn diffedit_accepts_a_hunk_id_from_the_listing() {
    let repo = two_hunk_stack("diffedit-by-id");
    let ids = two_hunk_ids(&repo, &["-r", "@-"]);

    repo.hunk_ok(&["diffedit", &one_hunk_spec(&ids[1].1), "-r", "@-"]);
    assert_eq!(repo.file_at("@-", "f.txt"), TWO_HUNK_SECOND_ONLY);
}

/// A hunk's *index* is its position in the file's hunk list, and both
/// directions put the same regions in the same order -- so an index read off
/// `list -r @-` names the same region to `restore -c @-`.
#[test]
fn restore_hunk_indices_line_up_with_the_forward_listing() {
    let repo = two_hunk_stack("restore-by-index");
    repo.hunk_ok(&[
        "restore",
        r#"{"files": {"f.txt": {"hunks": [1]}}, "default": "reset"}"#,
        "-c",
        "@-",
    ]);
    assert_eq!(
        repo.file_at("@-", "f.txt"),
        TWO_HUNK_FIRST_ONLY,
        "index 1 is the X7 hunk, and restore undoes it"
    );
}

/// Hunk ids are hashes of the text they were computed from, so restore's view
/// -- the diff the other way round -- has ids of its own. `list --from/--to`
/// is how you see them.
#[test]
fn list_from_to_shows_the_reversed_diff_that_restore_edits() {
    let repo = two_hunk_stack("list-from-to");

    let forward = repo.hunk_ok(&["list", "-r", "@-", "--format", "text"]);
    assert!(forward.contains("- L1"), "forward diff removes L1:\n{forward}");
    assert!(forward.contains("+ X1"), "forward diff adds X1:\n{forward}");

    let reversed = repo.hunk_ok(&["list", "--from", "@-", "--to", "@--", "--format", "text"]);
    assert!(reversed.contains("- X1"), "reversed diff removes X1:\n{reversed}");
    assert!(reversed.contains("+ L1"), "reversed diff adds L1:\n{reversed}");
}

#[test]
fn restore_accepts_a_hunk_id_from_the_reversed_listing() {
    let repo = two_hunk_stack("restore-by-id");
    let ids = two_hunk_ids(&repo, &["--from", "@-", "--to", "@--"]);

    repo.hunk_ok(&["restore", &one_hunk_spec(&ids[0].1), "-c", "@-"]);
    assert_eq!(repo.file_at("@-", "f.txt"), TWO_HUNK_SECOND_ONLY);
}

/// An id from the forward listing describes text that does not exist in the
/// view restore edits, so it must be refused rather than quietly select
/// nothing -- and the error has to point at the listing that *would* work.
#[test]
fn restore_refuses_an_id_taken_from_the_forward_listing() {
    let repo = two_hunk_stack("restore-forward-id");
    let ids = two_hunk_ids(&repo, &["-r", "@-"]);

    let err = repo.hunk_fail(&["restore", &one_hunk_spec(&ids[0].0), "-c", "@-"]);
    assert!(err.contains(&ids[0].0), "error should name the id: {err}");
    assert!(
        err.contains("--from") && err.contains("--to"),
        "error should point at the listing that shows restore's ids: {err}"
    );
    assert_eq!(
        repo.file_at("@-", "f.txt"),
        TWO_HUNK_CHANGED,
        "nothing should have been rewritten"
    );
}

// --- targeting revisions other than the working copy ----------------------

#[test]
fn diffedit_from_to_targets_two_non_working_copy_revisions() {
    let repo = two_hunk_stack("diffedit-from-to");
    repo.write_file("g.txt", "G\n");
    repo.jj_ok(&["commit", "-m", "tip"]);
    // @ (empty) / @- "tip" / @-- "change" / @--- "base"

    repo.hunk_ok(&["diffedit", FIRST_HUNK, "--from", "@---", "--to", "@--"]);

    assert_eq!(repo.file_at("@--", "f.txt"), TWO_HUNK_FIRST_ONLY);
    assert_eq!(
        repo.file_at("@-", "g.txt"),
        "G\n",
        "the descendant is untouched"
    );
}

#[test]
fn restore_from_into_targets_two_non_working_copy_revisions() {
    let repo = two_hunk_stack("restore-from-into");
    repo.write_file("g.txt", "G\n");
    repo.jj_ok(&["commit", "-m", "tip"]);

    repo.hunk_ok(&["restore", FIRST_HUNK, "--from", "@---", "--into", "@--"]);

    assert_eq!(repo.file_at("@--", "f.txt"), TWO_HUNK_SECOND_ONLY);
    assert_eq!(
        repo.file_at("@-", "g.txt"),
        "G\n",
        "the descendant is untouched"
    );
}

/// Editing a revision in the middle of a stack must not cost the descendants
/// their own changes: what `@` adds on top has to survive the rebase intact.
#[test]
fn diffedit_on_a_mid_stack_revision_leaves_the_descendant_diff_intact() {
    let repo = two_hunk_stack("diffedit-descendant");
    // `@` changes L4 and adds a file of its own.
    repo.write_file("f.txt", "X1\nL2\nL3\nX4\nL5\nL6\nX7\n");
    repo.write_file("g.txt", "G\n");

    repo.hunk_ok(&["diffedit", FIRST_HUNK, "-r", "@-"]);

    assert_eq!(repo.file_at("@-", "f.txt"), TWO_HUNK_FIRST_ONLY);
    assert_eq!(
        repo.file_at("@", "f.txt"),
        "X1\nL2\nL3\nX4\nL5\nL6\nL7\n",
        "@ keeps its own L4 change on top of the edited parent"
    );
    assert_eq!(repo.file_at("@", "g.txt"), "G\n");
}

// --- selections that keep nothing -----------------------------------------

#[test]
fn diffedit_refuses_a_selector_that_matches_nothing() {
    let repo = two_hunk_stack("diffedit-empty-selection");
    let err = repo.hunk_fail(&["diffedit", r#"file("nope.txt")"#, "-r", "@-"]);
    assert!(err.contains("matched no hunks"), "got: {err}");
    assert_eq!(
        repo.file_at("@-", "f.txt"),
        TWO_HUNK_CHANGED,
        "the revision must be left alone"
    );
}

#[test]
fn restore_refuses_a_selector_that_matches_nothing() {
    let repo = two_hunk_stack("restore-empty-selection");
    let err = repo.hunk_fail(&["restore", r#"file("nope.txt")"#, "-c", "@-"]);
    assert!(err.contains("matched no hunks"), "got: {err}");
    assert_eq!(
        repo.file_at("@-", "f.txt"),
        TWO_HUNK_CHANGED,
        "the revision must be left alone"
    );
}

/// `--allow-empty` is the escape hatch, and for `diffedit` it means what it
/// says: keep nothing, i.e. throw the whole change away.
#[test]
fn diffedit_allow_empty_discards_the_whole_change() {
    let repo = two_hunk_stack("diffedit-allow-empty");
    repo.hunk_ok(&[
        "diffedit",
        r#"file("nope.txt")"#,
        "-r",
        "@-",
        "--allow-empty",
    ]);
    assert_eq!(repo.file_at("@-", "f.txt"), TWO_HUNK_BASE);
}

/// The mirror image: undoing nothing is a no-op, not a wipe.
#[test]
fn restore_allow_empty_undoes_nothing() {
    let repo = two_hunk_stack("restore-allow-empty");
    repo.hunk_ok(&[
        "restore",
        r#"file("nope.txt")"#,
        "-c",
        "@-",
        "--allow-empty",
    ]);
    assert_eq!(repo.file_at("@-", "f.txt"), TWO_HUNK_CHANGED);
}

#[test]
fn diffedit_keeping_everything_changes_nothing() {
    let repo = two_hunk_stack("diffedit-keep-all");
    repo.hunk_ok(&["diffedit", "all()", "-r", "@-"]);
    assert_eq!(repo.file_at("@-", "f.txt"), TWO_HUNK_CHANGED);
}

#[test]
fn restore_undoing_everything_empties_the_revision() {
    let repo = two_hunk_stack("restore-all");
    repo.hunk_ok(&["restore", "all()", "-c", "@-"]);
    assert_eq!(repo.file_at("@-", "f.txt"), TWO_HUNK_BASE);
}

// --- flag plumbing ---------------------------------------------------------

#[test]
fn diffedit_reads_a_spec_file() {
    let repo = two_hunk_stack("diffedit-spec-file");
    let ids = two_hunk_ids(&repo, &["-r", "@-"]);
    // Outside the repo: a spec file dropped inside it is a change of its own.
    let spec_path = std::env::temp_dir().join(format!(
        "jj-hunk-diffedit-spec-{}.json",
        std::process::id()
    ));
    std::fs::write(&spec_path, one_hunk_spec(&ids[0].1)).unwrap();

    repo.hunk_ok(&["diffedit", "-f", spec_path.to_str().unwrap(), "-r", "@-"]);
    let _ = std::fs::remove_file(&spec_path);

    assert_eq!(repo.file_at("@-", "f.txt"), TWO_HUNK_FIRST_ONLY);
}

#[test]
fn diffedit_rejects_a_revision_together_with_from() {
    let repo = two_hunk_stack("diffedit-flag-conflict");
    let err = repo.hunk_fail(&["diffedit", "all()", "-r", "@-", "--from", "@--"]);
    assert!(
        err.contains("cannot be used with"),
        "-r and --from should conflict: {err}"
    );
}

#[test]
fn restore_rejects_changes_in_together_with_into() {
    let repo = two_hunk_stack("restore-flag-conflict");
    let err = repo.hunk_fail(&["restore", "all()", "-c", "@-", "--into", "@--"]);
    assert!(
        err.contains("cannot be used with"),
        "-c and --into should conflict: {err}"
    );
}

#[test]
fn list_rejects_a_revision_together_with_from() {
    let repo = two_hunk_stack("list-flag-conflict");
    let err = repo.hunk_fail(&["list", "-r", "@-", "--from", "@--"]);
    assert!(
        err.contains("cannot be used with"),
        "-r and --from should conflict: {err}"
    );
}

// ---------------------------------------------------------------------------
// absorb
// ---------------------------------------------------------------------------

/// A three-commit stack over one file, where each commit owns known lines:
/// `A` owns every line except 3 and 8, `B` owns line 3, and `C` owns line 8.
///
/// The working copy is left clean, so each test stages exactly the change whose
/// routing it is about.
fn absorb_stack(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    build_absorb_stack(&repo);
    repo
}

/// The stack itself, so a test that has to configure the repo first (see
/// [`pin_immutable`]) can do that before anything is committed.
fn build_absorb_stack(repo: &TestRepo) {
    repo.write_file("f.txt", "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");
    repo.jj_ok(&["commit", "-m", "A: create f"]);
    repo.write_file("f.txt", "a\nb\nCCC\nd\ne\nf\ng\nh\ni\nj\n");
    repo.jj_ok(&["commit", "-m", "B: line 3"]);
    repo.write_file("f.txt", "a\nb\nCCC\nd\ne\nf\ng\nHHH\ni\nj\n");
    repo.jj_ok(&["commit", "-m", "C: line 8"]);
}

fn absorb_change_id(repo: &TestRepo, revset: &str) -> String {
    repo.jj_ok(&["log", "--no-graph", "-r", revset, "-T", "change_id.short(8)"])
        .trim()
        .to_string()
}

fn file_at(repo: &TestRepo, rev: &str, path: &str) -> String {
    repo.jj_ok(&["file", "show", "-r", rev, path])
}

/// Everything the stack adds up to, as one diff. Absorb only redistributes a
/// change between ancestors, so this is invariant under it.
fn cumulative_diff(repo: &TestRepo) -> String {
    repo.jj_ok(&["diff", "--from", "root()", "--to", "@", "--git"])
}

fn pin_immutable(repo: &TestRepo, revset: &str) {
    let mut config = std::fs::read_to_string(&repo.config_path).unwrap();
    config.push_str(&format!(
        "\n[revset-aliases]\n\"immutable_heads()\" = '{revset}'\n"
    ));
    std::fs::write(&repo.config_path, config).unwrap();
}

#[test]
fn absorb_routes_a_hunk_to_the_ancestor_that_owns_its_lines() {
    let repo = absorb_stack("absorb-one-owner");
    // Line 3 belongs to B and to nothing else.
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");

    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);

    assert!(plan.contains(&format!("move into {b}")), "{plan}");
    assert!(plan.contains("f.txt:3  -1 +1"), "{plan}");
    assert!(!plan.contains("stay in"), "nothing should stay: {plan}");

    repo.hunk_ok(&["absorb"]);
    assert_eq!(file_at(&repo, &b, "f.txt"), "a\nb\nCCC2\nd\ne\nf\ng\nh\ni\nj\n");
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "");
}

#[test]
fn absorb_leaves_a_hunk_whose_lines_have_several_owners() {
    let repo = absorb_stack("absorb-many-owners");
    // One hunk covering lines 3..8, which span B, A and C.
    repo.write_file("f.txt", "a\nb\nX\nX\nX\nX\nX\nX\ni\nj\n");

    let a = absorb_change_id(&repo, r#"description(substring:"A: ")"#);
    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let c = absorb_change_id(&repo, r#"description(substring:"C: ")"#);
    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);

    assert!(plan.contains("stay in"), "{plan}");
    assert!(plan.contains("3 commits"), "the reason should count the owners: {plan}");
    for owner in [&a, &b, &c] {
        assert!(plan.contains(owner.as_str()), "{owner} should be named: {plan}");
    }
    assert!(!plan.contains("move into"), "nothing should move: {plan}");

    // And a real run leaves the revision exactly as it was.
    let before = repo.jj_ok(&["diff", "--git"]);
    repo.hunk_ok(&["absorb"]);
    assert_eq!(repo.jj_ok(&["diff", "--git"]), before);
}

#[test]
fn absorb_leaves_a_pure_insertion_unless_asked_to_route_it() {
    let repo = absorb_stack("absorb-insertion");
    // Between lines 5 and 6, both of which belong to A.
    repo.write_file("f.txt", "a\nb\nCCC\nd\ne\nNEW\nf\ng\nHHH\ni\nj\n");

    let a = absorb_change_id(&repo, r#"description(substring:"A: ")"#);

    let default_plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(default_plan.contains("stay in"), "{default_plan}");
    assert!(default_plan.contains("only adds lines"), "{default_plan}");
    assert!(
        default_plan.contains("--insertions=surrounding"),
        "the default must point at the opt-in: {default_plan}"
    );

    let opted_in = repo.hunk_ok(&["absorb", "--dry-run", "--insertions", "surrounding"]);
    assert!(opted_in.contains(&format!("move into {a}")), "{opted_in}");
    assert!(
        opted_in.contains("--insertions=surrounding: insertions are routed"),
        "the opt-in must be labelled in the plan: {opted_in}"
    );

    repo.hunk_ok(&["absorb", "--insertions", "surrounding"]);
    assert_eq!(
        file_at(&repo, &a, "f.txt"),
        "a\nb\nc\nd\ne\nNEW\nf\ng\nh\ni\nj\n"
    );
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "");
}

#[test]
fn absorb_leaves_an_insertion_that_lands_between_two_owners() {
    let repo = absorb_stack("absorb-insertion-boundary");
    // Between line 2 (A) and line 3 (B): both have an equal claim on it.
    repo.write_file("f.txt", "a\nb\nNEW\nCCC\nd\ne\nf\ng\nHHH\ni\nj\n");

    let plan = repo.hunk_ok(&["absorb", "--dry-run", "--insertions", "surrounding"]);
    assert!(plan.contains("stay in"), "{plan}");
    assert!(plan.contains("boundary"), "{plan}");
    assert!(!plan.contains("move into"), "{plan}");
}

/// Two hunks in one file bound for two different ancestors. The second squash
/// only finds its hunk because the plan re-matches by fingerprint: the first
/// squash rewrote the file and moved the second hunk's context out from under
/// its id.
#[test]
fn absorb_moves_hunks_to_several_ancestors_in_one_run() {
    let repo = absorb_stack("absorb-multi-target");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH2\ni\nj\n");

    let a = absorb_change_id(&repo, r#"description(substring:"A: ")"#);
    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let c = absorb_change_id(&repo, r#"description(substring:"C: ")"#);

    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(plan.contains("2 moving into 2 ancestors"), "{plan}");
    // Oldest destination first, so the plan reads in the direction history flows.
    let b_at = plan.find(&format!("move into {b}")).expect(&plan);
    let c_at = plan.find(&format!("move into {c}")).expect(&plan);
    assert!(b_at < c_at, "destinations should be ordered oldest first: {plan}");

    repo.hunk_ok(&["absorb"]);

    // Each ancestor now writes the final text itself, and nothing is left over.
    assert_eq!(file_at(&repo, &a, "f.txt"), "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");
    assert_eq!(file_at(&repo, &b, "f.txt"), "a\nb\nCCC2\nd\ne\nf\ng\nh\ni\nj\n");
    assert_eq!(file_at(&repo, &c, "f.txt"), "a\nb\nCCC2\nd\ne\nf\ng\nHHH2\ni\nj\n");
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "");
}

#[test]
fn absorb_leaves_hunks_in_a_file_the_revision_itself_added() {
    let repo = absorb_stack("absorb-new-file");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");
    repo.write_file("new.txt", "one\ntwo\n");

    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);

    assert!(plan.contains(&format!("move into {b}")), "{plan}");
    assert!(
        plan.contains("new.txt is new in this revision"),
        "a file with no parent version has no history to blame: {plan}"
    );

    repo.hunk_ok(&["absorb"]);
    // The new file is still the revision's own change, whole.
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "A new.txt");
    assert_eq!(file_at(&repo, "@", "new.txt"), "one\ntwo\n");
}

#[test]
fn absorb_refuses_to_route_into_an_immutable_commit() {
    let repo = TestRepo::new("absorb-immutable");
    // Before the stack is built: the config file lives in the working copy, so
    // writing it afterwards would itself be a change for absorb to route.
    pin_immutable(&repo, r#"description(substring:"A: ")"#);
    build_absorb_stack(&repo);
    // Line 5 belongs to A, which is now immutable.
    repo.write_file("f.txt", "a\nb\nCCC\nd\nEEE\nf\ng\nHHH\ni\nj\n");

    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(plan.contains("immutable"), "{plan}");
    assert!(!plan.contains("move into"), "{plan}");

    // The real run is a no-op rather than an error, and says so.
    let before = repo.jj_ok(&["diff", "--git"]);
    let out = repo.hunk_ok(&["absorb"]);
    assert!(out.contains("Nothing to absorb"), "{out}");
    assert_eq!(repo.jj_ok(&["diff", "--git"]), before);
}

/// Two byte-identical edits in one file share a fingerprint, so no selection
/// can name one without naming the other. Sending them to different ancestors
/// is therefore not expressible, and absorb says so rather than moving one.
#[test]
fn absorb_leaves_identical_hunks_bound_for_different_ancestors() {
    let repo = TestRepo::new("absorb-identical-hunks");
    repo.write_file("f.txt", "p\nDUP\nq\nr\nOLD\ns\n");
    repo.jj_ok(&["commit", "-m", "A: create f"]);
    // Line 5 becomes a second, identical `DUP`, owned by B rather than A.
    repo.write_file("f.txt", "p\nDUP\nq\nr\nDUP\ns\n");
    repo.jj_ok(&["commit", "-m", "B: line 5"]);

    // The same one-line edit made to both of them.
    repo.write_file("f.txt", "p\nEDIT\nq\nr\nEDIT\ns\n");

    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(
        plan.contains("identical to another hunk"),
        "the collision should be the stated reason: {plan}"
    );
    assert!(!plan.contains("move into"), "{plan}");

    let before = repo.jj_ok(&["diff", "--git"]);
    repo.hunk_ok(&["absorb"]);
    assert_eq!(repo.jj_ok(&["diff", "--git"]), before);
}

/// The mirror of the test above: identical hunks that agree on where they
/// belong do move, together.
#[test]
fn absorb_moves_identical_hunks_that_agree_together() {
    let repo = TestRepo::new("absorb-identical-hunks-agree");
    repo.write_file("f.txt", "p\nq\nr\ns\nt\nu\n");
    repo.jj_ok(&["commit", "-m", "A: create f"]);
    repo.write_file("f.txt", "p\nDUP\nr\ns\nDUP\nu\n");
    repo.jj_ok(&["commit", "-m", "B: both DUP lines"]);

    repo.write_file("f.txt", "p\nEDIT\nr\ns\nEDIT\nu\n");

    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(plan.contains(&format!("move into {b}")), "{plan}");
    assert!(plan.contains("2 hunks: 2 moving into 1 ancestor"), "{plan}");

    repo.hunk_ok(&["absorb"]);
    assert_eq!(file_at(&repo, &b, "f.txt"), "p\nEDIT\nr\ns\nEDIT\nu\n");
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "");
}

#[test]
fn absorb_only_considers_the_hunks_the_spec_names() {
    let repo = absorb_stack("absorb-spec-filter");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH2\ni\nj\n");

    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let c = absorb_change_id(&repo, r#"description(substring:"C: ")"#);

    // Name only the first hunk. The second is not even considered, so it is
    // absent from the plan rather than listed as staying.
    let id = first_hunk_id(&repo, "f.txt");
    let spec = format!(r#"{{"files": {{"f.txt": {{"ids": ["{id}"]}}}}}}"#);
    let plan = repo.hunk_ok(&["absorb", "--dry-run", &spec]);

    assert!(plan.contains(&format!("move into {b}")), "{plan}");
    assert!(!plan.contains(&format!("move into {c}")), "{plan}");
    assert!(plan.contains("1 hunks: 1 moving"), "{plan}");

    repo.hunk_ok(&["absorb", &spec]);
    assert_eq!(file_at(&repo, &b, "f.txt"), "a\nb\nCCC2\nd\ne\nf\ng\nh\ni\nj\n");
    // The unnamed hunk is untouched, still in the working copy.
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "M f.txt");
}

#[test]
fn absorb_accepts_a_hunkset_expression() {
    let repo = absorb_stack("absorb-hunkset");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");
    repo.write_file("other.txt", "only me\n");

    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let plan = repo.hunk_ok(&["absorb", "--dry-run", r#"file("f.txt")"#]);

    assert!(plan.contains(&format!("move into {b}")), "{plan}");
    assert!(!plan.contains("other.txt"), "the expression excluded it: {plan}");
}

/// Same input, same plan, every time -- including the order the destinations
/// and the hunks under them come out in.
#[test]
fn absorb_plans_identically_across_repeated_runs() {
    let repo = absorb_stack("absorb-deterministic");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\nEEE\nf\ng\nHHH2\ni\nj\n");
    repo.write_file("new.txt", "one\n");

    let first = repo.hunk_ok(&["absorb", "--dry-run"]);
    for run in 1..5 {
        assert_eq!(
            repo.hunk_ok(&["absorb", "--dry-run"]),
            first,
            "run {run} produced a different plan"
        );
    }
    // More than one destination, or the ordering this guards is untested.
    assert_eq!(first.matches("move into ").count(), 3, "{first}");
}

#[test]
fn absorb_dry_run_changes_nothing() {
    let repo = absorb_stack("absorb-dry-run-is-inert");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH2\ni\nj\n");

    let commits_before = repo.jj_ok(&["log", "--no-graph", "-T", r#"commit_id ++ "\n""#]);
    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);

    assert!(plan.contains("--dry-run: nothing was changed"), "{plan}");
    assert_eq!(
        repo.jj_ok(&["log", "--no-graph", "-T", r#"commit_id ++ "\n""#]),
        commits_before
    );
}

/// What moved plus what stayed is what there was: absorb redistributes a change
/// across the stack, and must neither drop nor invent a line doing it.
#[test]
fn absorb_moves_nothing_out_of_the_overall_change() {
    let repo = absorb_stack("absorb-lossless");
    // A mix on purpose: two hunks that route, one insertion that does not, and
    // a whole file that cannot.
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nNEW\nf\ng\nHHH2\ni\nj\n");
    repo.write_file("new.txt", "fresh\n");

    let before = cumulative_diff(&repo);
    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(
        plan.contains("4 hunks: 2 moving into 2 ancestors, 2 staying"),
        "{plan}"
    );

    repo.hunk_ok(&["absorb"]);

    assert_eq!(
        cumulative_diff(&repo),
        before,
        "the stack must still add up to exactly the same thing"
    );
    // ... and the part that could not move is still the revision's own change.
    assert_eq!(
        repo.jj_ok(&["diff", "--summary"]).trim(),
        "M f.txt\nA new.txt"
    );
}

#[test]
fn absorb_reports_the_operation_to_undo_with() {
    let repo = absorb_stack("absorb-undo-hint");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");

    let before = cumulative_diff(&repo);
    let out = repo.hunk_ok(&["absorb"]);

    let op = out
        .lines()
        .find_map(|line| line.strip_prefix("Undo all of it with: jj op restore "))
        .unwrap_or_else(|| panic!("absorb should print an operation to restore: {out}"))
        .trim()
        .to_string();

    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "");
    repo.jj_ok(&["op", "restore", &op]);

    // Back to one revision holding the whole change, and the stack unchanged.
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "M f.txt");
    assert_eq!(cumulative_diff(&repo), before);
}

#[test]
fn absorb_refuses_a_revision_with_nothing_in_it() {
    let repo = absorb_stack("absorb-empty-revision");
    let err = repo.hunk_fail(&["absorb"]);
    assert!(err.contains("nothing to absorb"), "{err}");
}

#[test]
fn absorb_refuses_a_selection_that_names_nothing() {
    let repo = absorb_stack("absorb-empty-selection");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");

    let err = repo.hunk_fail(&["absorb", r#"file("nope.txt")"#]);
    assert!(err.contains("matched no hunks"), "{err}");
}

#[test]
fn absorb_refuses_a_revset_naming_several_revisions() {
    let repo = absorb_stack("absorb-many-revisions");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");

    let err = repo.hunk_fail(&["absorb", "-r", "all()"]);
    assert!(err.contains("exactly one"), "{err}");
}

#[test]
fn absorb_can_run_on_a_revision_other_than_the_working_copy() {
    let repo = absorb_stack("absorb-other-revision");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");
    repo.jj_ok(&["commit", "-m", "D: fixups"]);
    // `@` is now an empty commit on top of D, and D is what gets absorbed.
    repo.write_file("later.txt", "unrelated\n");

    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let d = absorb_change_id(&repo, r#"description(substring:"D: ")"#);
    let before = cumulative_diff(&repo);

    repo.hunk_ok(&["absorb", "-r", &d]);

    assert_eq!(file_at(&repo, &b, "f.txt"), "a\nb\nCCC2\nd\ne\nf\ng\nh\ni\nj\n");
    assert_eq!(repo.changed_files(&d), Vec::<String>::new(), "D should be emptied");
    // The working copy, which was never the source, is untouched.
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "A later.txt");
    assert_eq!(cumulative_diff(&repo), before);
}

/// Annotation is read one line per line of the parent, and a file whose last
/// line has no terminator is where an off-by-one would first show up.
#[test]
fn absorb_lines_up_with_annotation_without_a_trailing_newline() {
    let repo = TestRepo::new("absorb-no-trailing-newline");
    repo.write_file("f.txt", "one\ntwo\nthree");
    repo.jj_ok(&["commit", "-m", "A: create f"]);
    repo.write_file("f.txt", "one\nTWO\nthree");
    repo.jj_ok(&["commit", "-m", "B: line 2"]);
    repo.write_file("f.txt", "one\nTWO\nTHREE");
    repo.jj_ok(&["commit", "-m", "C: line 3"]);

    // The last line, unterminated, and owned by C alone.
    repo.write_file("f.txt", "one\nTWO\nTHREE4");

    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let c = absorb_change_id(&repo, r#"description(substring:"C: ")"#);
    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);

    assert!(plan.contains(&format!("move into {c}")), "{plan}");
    assert!(!plan.contains(&format!("move into {b}")), "{plan}");
    assert!(plan.contains("f.txt:3  -1 +1"), "{plan}");

    repo.hunk_ok(&["absorb"]);
    assert_eq!(file_at(&repo, &c, "f.txt"), "one\nTWO\nTHREE4");
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "");
}

/// Absorb rewrites history, so every file in the revision has to be accounted
/// for in the plan -- including the ones no hunk selection can touch.
#[test]
fn absorb_accounts_for_a_binary_file_it_cannot_split() {
    let repo = absorb_stack("absorb-binary-note");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");
    repo.write_file("blob.dat", "bin\u{0}ary\n");

    let b = absorb_change_id(&repo, r#"description(substring:"B: ")"#);
    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);

    assert!(plan.contains(&format!("move into {b}")), "{plan}");
    assert!(
        plan.contains("note: blob.dat is binary"),
        "a binary change must still be reported: {plan}"
    );

    repo.hunk_ok(&["absorb"]);
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), "A blob.dat");
}

#[test]
fn absorb_says_what_it_could_not_route_when_there_is_nothing_else() {
    let repo = absorb_stack("absorb-binary-only");
    repo.write_file("blob.dat", "bin\u{0}ary\n");

    let err = repo.hunk_fail(&["absorb"]);
    assert!(err.contains("nothing to absorb"), "{err}");
    assert!(
        err.contains("blob.dat is binary"),
        "the refusal must not read as 'nothing is there': {err}"
    );
}

// ---------------------------------------------------------------------------
// A change that produces no hunks is still a change.
//
// A pure rename, a pure copy and a mode-only flip all diff to zero hunks. The
// guard in `list` kept binary and mode-only files but dropped the rest, and
// `--spec-template` dropped every one of them -- and an unnamed file takes
// `default: reset`, which restores the old name and deletes the new one. The
// rename was thrown away at exit 0, having never been shown to anyone.
// ---------------------------------------------------------------------------

/// A working copy that renames `src.txt` to `dst.txt` without touching a byte
/// of it, alongside an ordinary edit so the diff is not empty on its own.
fn pure_rename_repo(name: &str) -> TestRepo {
    const BASE: &str = "aaaaaaaaaaaa\nbbbbbbbbbbbb\ncccccccccccc\ndddddddddddd\neeeeeeeeeeee\n";

    let repo = TestRepo::new(name);
    repo.write_file("src.txt", BASE);
    repo.write_file("other.txt", "other-line-1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    std::fs::rename(repo.path().join("src.txt"), repo.path().join("dst.txt")).unwrap();
    repo.write_file("other.txt", "other-line-1\nother-line-2\n");

    // Guard against a vacuous test: add+delete exercises none of this.
    let summary = repo.changed_files("@");
    assert!(
        summary
            .iter()
            .any(|l| l.starts_with('R') && l.contains("src.txt") && l.contains("dst.txt")),
        "jj did not detect a pure rename, the test would be vacuous: {summary:?}"
    );
    repo
}

#[test]
fn a_pure_rename_is_visible_in_list() {
    let repo = pure_rename_repo("pure-rename-list");
    let text = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(
        text.contains("dst.txt"),
        "a rename with no content edit was invisible in `list`:\n{text}"
    );
    assert!(
        text.contains("src.txt"),
        "`list` should say where the file came from:\n{text}"
    );
}

#[test]
fn a_pure_rename_survives_the_spec_template_workflow() {
    // The documented flow, and the one that lost the rename: template out,
    // feed it straight back in.
    let repo = pure_rename_repo("pure-rename-template");

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    assert!(
        template.contains("dst.txt"),
        "--spec-template emitted no entry for a pure rename:\n{template}"
    );

    repo.hunk_ok(&["split", &template, "everything the template names"]);

    let committed = repo.changed_files("@-");
    assert!(
        committed
            .iter()
            .any(|l| l.starts_with('R') && l.contains("src.txt") && l.contains("dst.txt")),
        "the rename did not survive the template round trip: {committed:?}"
    );
    assert!(
        repo.changed_files("@").is_empty(),
        "the template should have moved the whole diff: {:?}",
        repo.changed_files("@")
    );
}

#[test]
fn a_pure_rename_is_not_discarded_by_diffedit() {
    // The destructive shape. `diffedit` applies `default: reset` in place, so
    // an invisible rename was undone outright rather than left behind in a
    // second commit.
    let repo = pure_rename_repo("pure-rename-diffedit");
    repo.jj_ok(&["commit", "-m", "rename plus edit"]);

    let template = repo.hunk_ok(&["list", "-r", "@-", "--spec-template"]);
    repo.hunk_ok(&["diffedit", &template, "-r", "@-"]);

    let committed = repo.changed_files("@-");
    assert!(
        committed
            .iter()
            .any(|l| l.starts_with('R') && l.contains("src.txt") && l.contains("dst.txt")),
        "diffedit destroyed a rename its own template did not name: {committed:?}"
    );
}

#[test]
fn a_pure_rename_can_be_kept_by_a_hand_written_action_entry() {
    // A hand-written spec names the file and nothing else; the rename source
    // has to be filled in from the diff before `select` sees it.
    let repo = pure_rename_repo("pure-rename-action");
    repo.hunk_ok(&[
        "split",
        r#"{"files": {"dst.txt": {"action": "keep"}}, "default": "reset"}"#,
        "just the rename",
    ]);

    let committed = repo.changed_files("@-");
    assert!(
        committed
            .iter()
            .any(|l| l.starts_with('R') && l.contains("src.txt") && l.contains("dst.txt")),
        "an explicitly kept rename did not land in the commit: {committed:?}"
    );
    assert!(
        !committed.iter().any(|l| l.contains("other.txt")),
        "the unselected edit rode along: {committed:?}"
    );
}

/// jj only reports a copy when the source is still present on the right, so
/// the source is edited too. That also makes `src.txt` both a copy source and
/// a diff entry in its own right.
fn copy_repo(name: &str) -> TestRepo {
    const BASE: &str = "aaaaaaaaaaaa\nbbbbbbbbbbbb\ncccccccccccc\ndddddddddddd\neeeeeeeeeeee\n";
    const EDITED: &str = "aaaaaaaaaaaa\nbbbbbbbbbbbb\nSRC-CHANGED\ndddddddddddd\neeeeeeeeeeee\n";

    let repo = TestRepo::new(name);
    repo.write_file("src.txt", BASE);
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("copy.txt", BASE);
    repo.write_file("src.txt", EDITED);

    let summary = repo.changed_files("@");
    assert!(
        summary
            .iter()
            .any(|l| l.starts_with('C') && l.contains("src.txt") && l.contains("copy.txt")),
        "jj did not detect a copy, the test would be vacuous: {summary:?}"
    );
    repo
}

#[test]
fn a_pure_copy_is_visible_and_survives_the_spec_template_workflow() {
    // The copy of an unedited file has no hunks of its own, exactly like a
    // pure rename.
    let repo = copy_repo("pure-copy-template");

    let text = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(
        text.contains("copy.txt"),
        "a copy with no content edit was invisible in `list`:\n{text}"
    );

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    assert!(
        template.contains("copy.txt"),
        "--spec-template emitted no entry for a pure copy:\n{template}"
    );

    repo.hunk_ok(&["split", &template, "copy and edit"]);
    let committed = repo.changed_files("@-");
    assert!(
        committed.iter().any(|l| l.contains("copy.txt")),
        "the copy did not survive the template round trip: {committed:?}"
    );
    assert!(
        repo.changed_files("@").is_empty(),
        "the template should have moved the whole diff: {:?}",
        repo.changed_files("@")
    );
}

#[cfg(unix)]
#[test]
fn a_rename_with_a_mode_change_survives_the_spec_template_workflow() {
    let repo = pure_rename_repo("rename-plus-mode");
    let path = repo.path().join("dst.txt");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).unwrap();

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    assert!(
        template.contains("dst.txt"),
        "--spec-template dropped a rename that also changed mode:\n{template}"
    );

    repo.hunk_ok(&["split", &template, "rename and chmod"]);
    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("dst.txt") && committed.contains("100755"),
        "the rename and its mode change did not both land:\n{committed}"
    );
}

#[cfg(unix)]
#[test]
fn a_mode_only_change_reaches_the_spec_template() {
    // `list` already showed this one; the template did not, so the documented
    // round trip reset the mode back at exit 0.
    let repo = TestRepo::new("mode-only-template");
    repo.write_file("only.sh", "x\n");
    repo.write_file("other.txt", "o1\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    let path = repo.path().join("only.sh");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).unwrap();
    repo.write_file("other.txt", "o1\no2\n");

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    assert!(
        template.contains("only.sh"),
        "--spec-template emitted no entry for a mode-only change:\n{template}"
    );

    repo.hunk_ok(&["split", &template, "mode and edit"]);
    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("100755"),
        "the mode change did not survive the template round trip:\n{committed}"
    );
}

#[test]
fn an_added_empty_file_is_visible_and_survives_the_spec_template_workflow() {
    // The fourth hunkless shape: an empty file has nothing on either side to
    // diff. `diffedit` fed its own template deleted the file outright.
    let repo = TestRepo::new("empty-file-template");
    repo.write_file("other.txt", "o1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("added-empty.txt", "");
    repo.write_file("other.txt", "o1\no2\n");

    let text = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(
        text.contains("added-empty.txt"),
        "an added empty file was invisible in `list`:\n{text}"
    );

    repo.jj_ok(&["commit", "-m", "add empty file plus edit"]);
    let template = repo.hunk_ok(&["list", "-r", "@-", "--spec-template"]);
    assert!(
        template.contains("added-empty.txt"),
        "--spec-template emitted no entry for an added empty file:\n{template}"
    );

    repo.hunk_ok(&["diffedit", &template, "-r", "@-"]);
    let committed = repo.changed_files("@-");
    assert!(
        committed.iter().any(|l| l.contains("added-empty.txt")),
        "diffedit deleted an empty file its own template did not name: {committed:?}"
    );
}

#[test]
fn a_removed_empty_file_is_visible_and_survives_the_spec_template_workflow() {
    // The mirror: deleting a file that was empty produces no hunks either, so
    // the deletion was silently undone.
    let repo = TestRepo::new("empty-file-removed-template");
    repo.write_file("was-empty.txt", "");
    repo.write_file("other.txt", "o1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    std::fs::remove_file(repo.path().join("was-empty.txt")).unwrap();
    repo.write_file("other.txt", "o1\no2\n");

    let text = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(
        text.contains("was-empty.txt"),
        "a removed empty file was invisible in `list`:\n{text}"
    );

    repo.jj_ok(&["commit", "-m", "remove empty file plus edit"]);
    let template = repo.hunk_ok(&["list", "-r", "@-", "--spec-template"]);
    repo.hunk_ok(&["diffedit", &template, "-r", "@-"]);

    let committed = repo.changed_files("@-");
    assert!(
        committed.iter().any(|l| l.contains("was-empty.txt")),
        "diffedit resurrected a deleted empty file: {committed:?}"
    );
}

// ---------------------------------------------------------------------------
// The fifth hunkless shape: a symlink.
//
// `jj file show` refuses to print a link ("exists but is not a file"), so both
// sides of a *retargeted* link read as the empty string and `get_hunks` yields
// nothing. `changes_without_hunks` kept a link that was added or removed --
// its `added | removed` arm covers those -- but a modified one matched none of
// its arms and was dropped, so `list` never showed it and `--spec-template`
// never named it. An unnamed file takes `default: reset`: `split` quietly left
// the retarget behind at exit 0, and `diffedit`, which keeps no remainder,
// undid it outright.
//
// A link's target is one atomic value, so there is no half of it to select.
// It is carried the way a binary is: visible with zero hunks, and named by an
// action rather than by ids nothing could resolve.
// ---------------------------------------------------------------------------

/// A working copy that points `link` at a different file without touching a
/// byte of anything else it owns, alongside an ordinary edit so the diff is
/// not made entirely of things this bug hid.
#[cfg(unix)]
fn retargeted_symlink_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("old-target.txt", "OLD\n");
    repo.write_file("new-target.txt", "NEW\n");
    repo.write_file("other.txt", "other-line-1\n");
    std::os::unix::fs::symlink("old-target.txt", repo.path().join("link")).unwrap();
    repo.jj_ok(&["commit", "-m", "base"]);

    std::fs::remove_file(repo.path().join("link")).unwrap();
    std::os::unix::fs::symlink("new-target.txt", repo.path().join("link")).unwrap();
    repo.write_file("other.txt", "other-line-1\nother-line-2\n");

    // Guard against a vacuous test: if jj called this anything but a
    // modification, the shape under test is not the one that was broken.
    let summary = repo.changed_files("@");
    assert!(
        summary
            .iter()
            .any(|l| l.starts_with('M') && l.ends_with("link")),
        "jj did not report the link as modified, the test would be vacuous: {summary:?}"
    );
    repo
}

/// The link a `SelectPath` names, as the path it points at. Panics if what is
/// there is no longer a link at all, which is the failure worth naming loudly:
/// `select` writes files, and a link rebuilt as a regular file holding its
/// target's *bytes* is the shape that quietly corrupts a tree.
#[cfg(unix)]
fn link_target(repo: &TestRepo, name: &str) -> String {
    let path = repo.path().join(name);
    let meta = std::fs::symlink_metadata(&path)
        .unwrap_or_else(|e| panic!("{name} is not there at all: {e}"));
    assert!(
        meta.file_type().is_symlink(),
        "{name} is no longer a symlink -- it was rebuilt as a regular file"
    );
    std::fs::read_link(&path)
        .unwrap()
        .to_string_lossy()
        .to_string()
}

#[cfg(unix)]
#[test]
fn a_retargeted_symlink_is_visible_in_list() {
    let repo = retargeted_symlink_repo("symlink-retarget-list");
    let text = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(
        text.contains("link"),
        "a link pointed at a new target was invisible in `list`:\n{text}"
    );
    // Zero hunks with no explanation reads as a bug in this tool, so the
    // listing has to say why there is nothing to pick.
    assert!(
        text.contains("[symlink"),
        "`list` should say the entry is a link, not just show it with no hunks:\n{text}"
    );
}

#[cfg(unix)]
#[test]
fn a_retargeted_symlink_is_named_by_the_spec_template() {
    let repo = retargeted_symlink_repo("symlink-retarget-template");
    let template = repo.hunk_ok(&["list", "--spec-template"]);
    assert!(
        template.contains("\"link\""),
        "--spec-template emitted no entry for a retargeted link:\n{template}"
    );
    // An action, not ids: there are no ids for a link, and a template that
    // invented some would be one `split` is bound to reject.
    let entry = template
        .split("\"link\"")
        .nth(1)
        .expect("the template names the link");
    assert!(
        entry.contains("action") && !entry.split('}').next().unwrap().contains("ids"),
        "the link should be named by an action, not by hunk ids:\n{template}"
    );
}

#[cfg(unix)]
#[test]
fn a_retargeted_symlink_survives_the_spec_template_workflow() {
    // The documented flow, and the one that lost the retarget: template out,
    // feed it straight back in.
    let repo = retargeted_symlink_repo("symlink-retarget-roundtrip");

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    repo.hunk_ok(&["split", &template, "everything the template names"]);

    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("120000") && committed.contains("+new-target.txt"),
        "the retarget did not reach the commit as a link:\n{committed}"
    );
    assert!(
        repo.changed_files("@").is_empty(),
        "the template should have moved the whole diff: {:?}",
        repo.changed_files("@")
    );
    assert_eq!(
        link_target(&repo, "link"),
        "new-target.txt",
        "the working copy's link was not left pointing at the new target"
    );
}

#[cfg(unix)]
#[test]
fn a_retargeted_symlink_is_not_discarded_by_diffedit() {
    // The destructive shape. `diffedit` applies `default: reset` in place and
    // keeps no remainder, so a link its own template failed to name was not
    // merely left behind -- it was undone.
    let repo = retargeted_symlink_repo("symlink-retarget-diffedit");
    repo.jj_ok(&["commit", "-m", "retarget plus edit"]);

    let template = repo.hunk_ok(&["list", "-r", "@-", "--spec-template"]);
    repo.hunk_ok(&["diffedit", &template, "-r", "@-"]);

    let committed = repo.changed_files("@-");
    assert!(
        committed.iter().any(|l| l.ends_with("link")),
        "diffedit discarded a retarget its own template did not name: {committed:?}"
    );
    assert_eq!(
        link_target(&repo, "link"),
        "new-target.txt",
        "diffedit put the link back on its old target"
    );
}

#[cfg(unix)]
#[test]
fn keeping_a_retargeted_symlink_from_the_template_commits_the_new_target() {
    // The selective case: the template now names the link, so a user can
    // delete every other entry from it and commit the retarget on its own.
    // Before the link reached the template there was no entry to keep, and the
    // spec that came back kept nothing at all.
    let repo = retargeted_symlink_repo("symlink-retarget-keep");

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    assert!(
        template.contains("\"link\""),
        "the template must name the link for this test to mean anything:\n{template}"
    );
    repo.hunk_ok(&[
        "split",
        r#"{"files": {"link": {"action": "keep"}}, "default": "reset"}"#,
        "just the retarget",
    ]);

    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("120000") && committed.contains("+new-target.txt"),
        "the kept retarget did not land in the commit as a link:\n{committed}"
    );
    assert!(
        !committed.contains("other.txt"),
        "the unselected edit rode along:\n{committed}"
    );
    assert_eq!(
        link_target(&repo, "link"),
        "new-target.txt",
        "keeping the link left something other than a link on the new target"
    );
}

/// Resetting is the other half of the same decision, and it is the half that
/// writes: `reset_file` has to rebuild the link from the left side rather than
/// copy the bytes its old target happened to hold.
///
/// This one is a preservation guard, not a reproduction: `select` never
/// consulted `list`, so a hand-written reset already behaved before the
/// visibility fix. What is new is that a user can now *reach* this decision,
/// by editing an entry the template finally emits.
#[cfg(unix)]
#[test]
fn resetting_a_retargeted_symlink_restores_the_old_target() {
    let repo = retargeted_symlink_repo("symlink-retarget-reset");
    repo.jj_ok(&["commit", "-m", "retarget plus edit"]);

    // `diffedit`, because it applies the reset in place: the old target has to
    // really come back, not merely stay behind in a remainder commit.
    repo.hunk_ok(&[
        "diffedit",
        r#"{"files": {"other.txt": {"action": "keep"}, "link": {"action": "reset"}}, "default": "reset"}"#,
        "-r",
        "@-",
    ]);

    let committed = repo.changed_files("@-");
    assert!(
        !committed.iter().any(|l| l.ends_with("link")),
        "the reset link stayed in the commit: {committed:?}"
    );
    assert!(
        committed.iter().any(|l| l.contains("other.txt")),
        "the kept edit was reset too: {committed:?}"
    );
    assert_eq!(
        link_target(&repo, "link"),
        "old-target.txt",
        "reset did not put the link back on its old target"
    );
}

/// The `added | removed` arm always covered these two, which is why only the
/// retargeted link was ever invisible. A preservation guard: it could not fail
/// before the fix, and exists so that widening `changes_without_hunks` cannot
/// quietly change what already worked.
#[cfg(unix)]
#[test]
fn added_and_removed_symlinks_still_survive_the_spec_template_workflow() {
    let repo = TestRepo::new("symlink-added-removed");
    repo.write_file("a.txt", "AAA\n");
    repo.write_file("other.txt", "other-line-1\n");
    std::os::unix::fs::symlink("a.txt", repo.path().join("gone")).unwrap();
    repo.jj_ok(&["commit", "-m", "base"]);

    std::fs::remove_file(repo.path().join("gone")).unwrap();
    std::os::unix::fs::symlink("a.txt", repo.path().join("fresh")).unwrap();
    repo.write_file("other.txt", "other-line-1\nother-line-2\n");

    let text = repo.hunk_ok(&["list", "--format", "text"]);
    assert!(
        text.contains("fresh") && text.contains("gone"),
        "an added or removed link went missing from `list`:\n{text}"
    );

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    assert!(
        template.contains("\"fresh\"") && template.contains("\"gone\""),
        "--spec-template dropped an added or removed link:\n{template}"
    );

    repo.hunk_ok(&["split", &template, "everything the template names"]);
    let committed = repo.changed_files("@-");
    assert!(
        committed
            .iter()
            .any(|l| l.starts_with('A') && l.ends_with("fresh")),
        "the new link did not survive the round trip: {committed:?}"
    );
    assert!(
        committed
            .iter()
            .any(|l| l.starts_with('D') && l.ends_with("gone")),
        "the deletion did not survive the round trip: {committed:?}"
    );
    assert!(
        repo.changed_files("@").is_empty(),
        "the template should have moved the whole diff: {:?}",
        repo.changed_files("@")
    );
    assert_eq!(
        link_target(&repo, "fresh"),
        "a.txt",
        "the added link was rebuilt as something other than a link"
    );
}

/// A file that *becomes* a link, or a link that becomes a file, still carries
/// the old side's text as a real hunk -- so it must stay hunk-selectable
/// rather than collapse into a whole-file action. Naming the symlink-ness on
/// the entry must not take that away.
#[cfg(unix)]
#[test]
fn a_file_that_becomes_a_symlink_is_still_named_by_its_hunk_ids() {
    let repo = TestRepo::new("symlink-becomes-ids");
    repo.write_file("a.txt", "AAA\n");
    repo.write_file("f.txt", "FFF\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    std::fs::remove_file(repo.path().join("f.txt")).unwrap();
    std::os::unix::fs::symlink("a.txt", repo.path().join("f.txt")).unwrap();

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    let entry = template
        .split("\"f.txt\"")
        .nth(1)
        .expect("the template names the file");
    assert!(
        entry.contains("ids"),
        "a file that became a link lost the hunk it really has:\n{template}"
    );

    repo.hunk_ok(&["split", &template, "f.txt becomes a link"]);
    assert_eq!(
        link_target(&repo, "f.txt"),
        "a.txt",
        "the change did not land as a link"
    );
    assert!(
        repo.changed_files("@").is_empty(),
        "the template should have moved the whole diff: {:?}",
        repo.changed_files("@")
    );
}

// ---------------------------------------------------------------------------
// A spec keyed by a rename's OLD path.
//
// `validate_spec_resolves` indexed every entry of `all_paths()`, so the old
// path validated -- and then `fill_rename_sources` and `select` both look the
// entry up under the NEW path, found nothing, and reverted the rename. The
// tool said "this resolves" and then undid the change, at exit 0.
// ---------------------------------------------------------------------------

#[test]
fn a_spec_keyed_by_a_renames_old_path_is_rejected() {
    let (repo, _) = rename_repo("rename-old-path-key");
    let id = first_hunk_id(&repo, "dst.txt");
    let spec = format!(r#"{{"files": {{"src.txt": {{"ids": ["{id}"]}}}}, "default": "reset"}}"#);

    let err = repo.hunk_fail(&["split", &spec, "keyed by the old path"]);
    assert!(
        err.contains("dst.txt"),
        "the error must name the path to use instead: {err}"
    );
    assert!(
        !repo
            .log_descriptions()
            .iter()
            .any(|d| d.contains("keyed by the old path")),
        "a commit was created from a spec keyed by the old path"
    );
    assert!(
        repo.changed_files("@")
            .iter()
            .any(|l| l.contains("src.txt") && l.contains("dst.txt")),
        "the rename must be left exactly as it was"
    );
}

#[test]
fn a_spec_keyed_by_a_copys_source_still_validates_against_that_source() {
    // The old path of a COPY is a real diff entry of its own. Aliasing it to
    // the copy meant a spec naming the source's own hunk could be validated
    // against the copy's hunks instead -- and which one won depended on the
    // order jj happened to list them in.
    let repo = copy_repo("copy-source-key");
    let id = first_hunk_id(&repo, "src.txt");
    let spec = format!(r#"{{"files": {{"src.txt": {{"ids": ["{id}"]}}}}, "default": "reset"}}"#);

    repo.hunk_ok(&["split", &spec, "only the source edit"]);
    let committed = repo.changed_files("@-");
    assert!(
        committed.iter().any(|l| l.contains("src.txt")),
        "the source's own edit was not committed: {committed:?}"
    );
    assert!(
        !committed.iter().any(|l| l.contains("copy.txt")),
        "the unselected copy rode along: {committed:?}"
    );
}

// ---------------------------------------------------------------------------
// `--allow-empty` means "an empty RESULT is acceptable", not "skip checking
// whether what I wrote refers to anything". It used to gate the whole
// resolution check, so passing it once for a legitimately blanked entry threw
// away typo detection for every other entry in the same spec.
// ---------------------------------------------------------------------------

#[test]
fn allow_empty_still_rejects_a_stale_hunk_id() {
    let repo = strict_repo("allow-empty-stale-id");
    let bogus = format!("hunk-{}", "a".repeat(64));
    let spec = format!(r#"{{"files": {{"a.py": {{"ids": ["{bogus}"]}}}}, "default": "reset"}}"#);

    let err = repo.hunk_fail(&["split", "--allow-empty", &spec, "stale id"]);
    assert!(err.contains(&bogus), "the error must name the bad id: {err}");
    assert!(
        !repo.log_descriptions().iter().any(|d| d.contains("stale id")),
        "--allow-empty let a stale id through as an empty commit"
    );
}

#[test]
fn allow_empty_still_rejects_a_typo_beside_a_good_entry() {
    // The concrete harm: one entry is legitimately blank, so the user passes
    // --allow-empty, and the stale id in the entry next to it goes unreported.
    let repo = strict_repo("allow-empty-mixed");
    let py = first_hunk_id(&repo, "a.py");
    let bogus = format!("hunk-{}", "b".repeat(64));
    let spec = format!(
        r#"{{"files": {{"a.py": {{"ids": ["{py}"]}}, "a.rs": {{"ids": ["{bogus}"]}}}}, "default": "reset"}}"#
    );

    let err = repo.hunk_fail(&["split", "--allow-empty", &spec, "mixed spec"]);
    assert!(
        err.contains(&bogus),
        "the stale id beside a good entry must still be reported: {err}"
    );
}

#[test]
fn allow_empty_still_accepts_a_deliberately_blank_selection() {
    // The flag's real purpose: every entry names a path that is in the diff
    // and deliberately keeps none of it.
    let repo = strict_repo("allow-empty-blank");
    repo.hunk_ok(&[
        "split",
        "--allow-empty",
        r#"{"files": {"a.py": {"ids": []}, "a.rs": {"ids": []}}, "default": "reset"}"#,
        "deliberately empty",
    ]);
    assert!(
        repo.log_descriptions()
            .iter()
            .any(|d| d.contains("deliberately empty")),
        "--allow-empty must still allow an empty result"
    );
}

// ---------------------------------------------------------------------------
// A reusable spec names a stable allowlist of paths, most of which are absent
// from any one diff. Rejecting every absent path made that shape unusable;
// accepting all of them brings back the silent empty commit. The rule: an
// absent path is reported only when the spec names nothing that IS in the
// diff.
// ---------------------------------------------------------------------------

#[test]
fn a_spec_may_name_paths_this_diff_does_not_contain() {
    let repo = strict_repo("allowlist-partly-stale");
    repo.hunk_ok(&[
        "commit",
        r#"{"files": {"a.py": {"action": "keep"}, "untouched.txt": {"action": "keep"}}, "default": "reset"}"#,
        "allowlist",
    ]);

    let committed = repo.changed_files("@-");
    assert!(
        committed.iter().any(|l| l.contains("a.py")),
        "the one path that is in the diff was not committed: {committed:?}"
    );
    assert!(
        !committed.iter().any(|l| l.contains("a.rs")),
        "an unlisted path rode along: {committed:?}"
    );
}

#[test]
fn several_unchanged_paths_beside_one_changed_one_are_accepted() {
    let repo = strict_repo("allowlist-many-stale");
    let py = first_hunk_id(&repo, "a.py");
    let spec = format!(
        r#"{{"files": {{"gone1.txt": {{"action": "keep"}}, "gone2.txt": {{"action": "keep"}}, "gone3.txt": {{"ids": []}}, "a.py": {{"ids": ["{py}"]}}}}, "default": "reset"}}"#
    );
    repo.hunk_ok(&["split", &spec, "long allowlist"]);
    assert!(
        repo.log_descriptions()
            .iter()
            .any(|d| d.contains("long allowlist")),
        "a mostly-stale allowlist with one live entry must be accepted"
    );
}

#[test]
fn a_reset_allowlist_under_default_keep_may_name_unchanged_paths() {
    // The mirror shape of the allowlist: "keep everything except these". It
    // can never produce an empty result, so an entry naming a file this diff
    // does not touch is a no-op, not a mistake worth aborting over.
    let repo = strict_repo("denylist-default-keep");
    repo.hunk_ok(&[
        "split",
        r#"{"files": {"secrets.env": {"action": "reset"}, "a.rs": {"action": "reset"}}, "default": "keep"}"#,
        "everything but a.rs",
    ]);

    let committed = repo.changed_files("@-");
    assert!(
        committed.iter().any(|l| l.contains("a.py")),
        "the kept path is missing: {committed:?}"
    );
    assert!(
        !committed.iter().any(|l| l.contains("a.rs")),
        "the reset path rode along: {committed:?}"
    );
}

#[test]
fn an_absent_keep_path_is_reported_when_the_only_live_entry_resets() {
    // The forgiveness above must not extend this far: `a.py` is in the diff
    // but is being thrown away, so nothing at all is kept and the spec still
    // produces the silent empty commit. Tolerating the absent path because
    // *some* entry resolved would have reopened exactly that hole.
    let repo = strict_repo("allowlist-live-entry-resets");
    let err = repo.hunk_fail(&[
        "split",
        r#"{"files": {"a.py": {"action": "reset"}, "nope.txt": {"action": "keep"}}, "default": "reset"}"#,
        "nothing kept",
    ]);
    assert!(err.contains("nope.txt"), "the error must name the path: {err}");
    assert!(
        !repo.log_descriptions().iter().any(|d| d.contains("nothing kept")),
        "an empty commit was created from a spec that keeps nothing real"
    );
}

#[test]
fn a_spec_that_names_nothing_in_the_diff_is_rejected() {
    // Two typos, not one: a rule phrased as "the spec's only entry" would let
    // this straight through into an empty commit.
    let repo = strict_repo("allowlist-all-stale");
    let err = repo.hunk_fail(&[
        "commit",
        r#"{"files": {"untouched.txt": {"action": "keep"}, "also-gone.txt": {"action": "keep"}}, "default": "reset"}"#,
        "all stale",
    ]);
    assert!(
        err.contains("untouched.txt") && err.contains("also-gone.txt"),
        "the error must name every path that resolved to nothing: {err}"
    );
    assert!(
        !repo.log_descriptions().iter().any(|d| d.contains("all stale")),
        "an empty commit was created from a spec that names nothing in the diff"
    );
}

// ---------------------------------------------------------------------------
// The forgiveness above was keyed on the wrong thing: whether an absent path
// was reported depended on what ELSE the spec resolved, so a single entry that
// still matched silenced every entry that no longer did. The two tests below
// are the same absent path under the two entry shapes, and they must disagree:
// ids came off a real diff and cannot go missing innocently, a bare `keep`
// can.
// ---------------------------------------------------------------------------

#[test]
fn an_absent_path_that_names_hunks_is_reported_beside_a_live_entry() {
    // The silent subset: a spec asking for two files committed one, at exit 0,
    // with nothing on stderr. `a.py` resolved, and that alone was enough to
    // drop the report for the path carrying `a.rs`'s ids.
    let repo = strict_repo("stale-ids-beside-live");
    let py = first_hunk_id(&repo, "a.py");
    let rs = first_hunk_id(&repo, "a.rs");
    let spec = format!(
        r#"{{"files": {{"a.py": {{"ids": ["{py}"]}}, "typo-not-a-real-file.rs": {{"ids": ["{rs}"]}}}}, "default": "reset"}}"#
    );

    let err = repo.hunk_fail(&["commit", &spec, "half a spec"]);
    assert!(
        err.contains("typo-not-a-real-file.rs"),
        "the error must name the path whose ids resolved to nothing: {err}"
    );
    // Exit status alone would have missed this bug: the old code exited 0 AND
    // wrote a commit holding half of what was asked for, so what the commit
    // contains has to be asserted separately.
    assert!(
        !repo
            .log_descriptions()
            .iter()
            .any(|d| d.contains("half a spec")),
        "a commit was created from a spec half of which named nothing"
    );
    let left = repo.changed_files("@");
    assert!(
        left.iter().any(|l| l.contains("a.py")) && left.iter().any(|l| l.contains("a.rs")),
        "both edits must be left in the working copy untouched: {left:?}"
    );
}

#[test]
fn the_same_absent_path_is_forgiven_when_it_names_no_hunks() {
    // Why the leniency cannot just be deleted: the path that is an error above
    // is fine here. A bare `keep` claims nothing about this diff's contents, so
    // a reusable allowlist may carry any number of them beside one live entry
    // -- which is the shape a checked-in spec actually has.
    let repo = strict_repo("allowlist-absent-without-ids");
    let py = first_hunk_id(&repo, "a.py");
    let spec = format!(
        r#"{{"files": {{"a.py": {{"ids": ["{py}"]}}, "typo-not-a-real-file.rs": {{"action": "keep"}}, "also-absent.rs": {{"action": "keep"}}}}, "default": "reset"}}"#
    );

    repo.hunk_ok(&["commit", &spec, "allowlist beside a live id"]);
    let committed = repo.changed_files("@-");
    assert!(
        committed.iter().any(|l| l.contains("a.py")),
        "the live entry was not committed: {committed:?}"
    );
    assert!(
        !committed.iter().any(|l| l.contains("a.rs")),
        "an unlisted path rode along: {committed:?}"
    );
}

// ---------------------------------------------------------------------------
// The `select` path: what jj hands the tool, and what the tool may touch.
//
// `select` is the only verb that writes to a filesystem, and jj gives it two
// directories to write in. Everything below is a way it was caught reaching
// outside them, committing bytes it was never given, or applying a spec to a
// file the spec was not talking about.
// ---------------------------------------------------------------------------

/// Run jj-hunk with the process cwd set to `sub`, a directory relative to the
/// repo root.
///
/// Every path jj prints is relative to the cwd, so this is the only way to
/// exercise the frame `select` has to agree with: from the root the two
/// spellings coincide and every disagreement hides.
fn hunk_in(repo: &TestRepo, sub: &str, args: &[&str]) -> std::process::Output {
    Command::new(jj_hunk_bin())
        .args(args)
        .current_dir(repo.path().join(sub))
        .env("JJ_USER", "Test User")
        .env("JJ_EMAIL", "test@example.com")
        .env("JJ_CONFIG", &repo.config_path)
        .env("PATH", path_with_jj_hunk())
        .output()
        .expect("failed to run jj-hunk")
}

fn hunk_in_ok(repo: &TestRepo, sub: &str, args: &[&str]) -> String {
    let out = hunk_in(repo, sub, args);
    assert!(
        out.status.success(),
        "jj-hunk {:?} in {sub} failed: {}{}",
        args,
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn hunk_in_fail(repo: &TestRepo, sub: &str, args: &[&str]) -> String {
    let out = hunk_in(repo, sub, args);
    assert!(
        !out.status.success(),
        "jj-hunk {:?} in {sub} should have failed: {}",
        args,
        String::from_utf8_lossy(&out.stdout),
    );
    let mut combined = String::from_utf8_lossy(&out.stderr).to_string();
    combined.push_str(&String::from_utf8_lossy(&out.stdout));
    combined
}

/// The hunk ids `list` prints, keyed by the path it printed them under.
fn listed_ids_in(repo: &TestRepo, sub: &str, args: &[&str]) -> HashMap<String, Vec<String>> {
    let mut argv = vec!["list", "--format", "json"];
    argv.extend_from_slice(args);
    let listing: serde_json::Value = serde_json::from_str(&hunk_in_ok(repo, sub, &argv)).unwrap();
    let mut out = HashMap::new();
    for file in listing["files"].as_array().unwrap() {
        let path = file["path"].as_str().unwrap().to_string();
        let ids = file["hunks"]
            .as_array()
            .map(|hunks| {
                hunks
                    .iter()
                    .map(|h| h["id"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default();
        out.insert(path, ids);
    }
    out
}

/// A file's bytes at `rev`, without going through a lossy String.
fn file_bytes_at(repo: &TestRepo, rev: &str, path: &str) -> Vec<u8> {
    let out = Command::new("jj")
        .args(["file", "show", "-r", rev, &format!("file:{path}")])
        .current_dir(repo.path())
        .env("JJ_USER", "Test User")
        .env("JJ_EMAIL", "test@example.com")
        .env("JJ_CONFIG", &repo.config_path)
        .env("PATH", path_with_jj_hunk())
        .output()
        .expect("jj file show failed");
    assert!(
        out.status.success(),
        "jj file show -r {rev} {path}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[cfg(unix)]
fn chmod_exec(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

// ---------------------------------------------------------------------------
// Symlinks: a link has a target, not text, so no hunk selection can describe
// it -- and nothing may be written *through* it. `fs::write` and
// `fs::read_to_string` both traverse links, so a link committed in the repo
// aimed `select`'s write at a path in neither directory.
// ---------------------------------------------------------------------------

/// The worst shape: a link whose target is outside both directories jj
/// materialised. Asserting on the committed content would not catch this --
/// the damage lands on a file jj never sees.
#[cfg(unix)]
#[test]
fn select_never_writes_through_a_symlink_out_of_the_directories_it_was_given() {
    let repo = TestRepo::new("select-link-escape");
    let root = repo.path().to_path_buf();
    let left = root.join("L");
    let right = root.join("R");
    let outside = root.join("outside");
    for dir in [&left, &right, &outside] {
        std::fs::create_dir_all(dir).unwrap();
    }

    std::fs::write(outside.join("victim.txt"), "NEW CONTENT\n").unwrap();
    std::fs::write(left.join("old-target.txt"), "OLD CONTENT\n").unwrap();
    std::os::unix::fs::symlink("old-target.txt", left.join("link.txt")).unwrap();
    std::os::unix::fs::symlink("../outside/victim.txt", right.join("link.txt")).unwrap();

    let spec_path = root.join("sel.json");
    std::fs::write(&spec_path, r#"{"files": {"link.txt": {"ids": []}}}"#).unwrap();

    let out = Command::new(jj_hunk_bin())
        .args(["select", left.to_str().unwrap(), right.to_str().unwrap()])
        .current_dir(&root)
        .env("JJ_HUNK_SELECTION", &spec_path)
        .env("JJ_CONFIG", &repo.config_path)
        .output()
        .expect("failed to run jj-hunk select");
    assert!(
        out.status.success(),
        "select failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(outside.join("victim.txt")).unwrap(),
        "NEW CONTENT\n",
        "select followed the link and rewrote a file outside both directories"
    );
    let meta = std::fs::symlink_metadata(right.join("link.txt")).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "resetting a link must restore a link, not a regular file"
    );
    assert_eq!(
        std::fs::read_link(right.join("link.txt")).unwrap(),
        Path::new("old-target.txt"),
        "the link was not put back to the left side's target"
    );
}

/// The same write-through, aimed at a file jj *does* see: the link's neighbour
/// in the very same commit. `a.txt` is kept whole, so nothing rewrites it
/// afterwards and the damage is what lands in history.
#[cfg(unix)]
#[test]
fn a_symlink_in_a_selection_does_not_overwrite_the_file_it_points_at() {
    let repo = TestRepo::new("select-link-neighbour");
    repo.write_file("a.txt", "AAA-line1\n");
    repo.write_file("f.txt", "FFF\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("a.txt", "AAA-line1\nAAA-line2\n");
    std::fs::remove_file(repo.path().join("f.txt")).unwrap();
    std::os::unix::fs::symlink("a.txt", repo.path().join("f.txt")).unwrap();

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"a.txt": {"action": "keep"}, "f.txt": {"ids": []}}, "default": "reset"}"#,
        "keep a.txt",
    ]);

    assert_eq!(
        repo.file_at("@-", "a.txt"),
        "AAA-line1\nAAA-line2\n",
        "the link's reset was written through the link and clobbered a.txt"
    );
}

/// A link kept whole must stay a link. Flattening it to its target's bytes
/// would turn a 120000 entry into a 100644 one.
#[cfg(unix)]
#[test]
fn a_symlink_kept_whole_stays_a_link() {
    let repo = TestRepo::new("select-link-keep");
    repo.write_file("a.txt", "AAA-line1\n");
    repo.write_file("f.txt", "FFF\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("a.txt", "AAA-line1\nAAA-line2\n");
    std::fs::remove_file(repo.path().join("f.txt")).unwrap();
    std::os::unix::fs::symlink("a.txt", repo.path().join("f.txt")).unwrap();

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"a.txt": {"action": "keep"}, "f.txt": {"action": "keep"}}, "default": "reset"}"#,
        "keep both",
    ]);

    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("120000"),
        "the kept link was committed as something other than a link:\n{committed}"
    );
}

/// The same escape one component higher: the link is a *directory* on the
/// path, not the leaf. jj materialises exactly this whenever a commit replaces
/// a directory with a symlink -- one side has `conf` as a link, the other has
/// `conf/x.txt` as a file -- and every read, unlink and write under `conf/`
/// then lands wherever the link points. Keeping the link entry itself is what
/// makes it deterministic; otherwise resetting it errors first.
#[cfg(unix)]
#[test]
fn select_never_writes_through_a_symlinked_directory_on_the_path() {
    let repo = TestRepo::new("select-link-dir");
    let victim = std::env::temp_dir().join(format!(
        "jj-hunk-test-victim-{}-{}",
        std::process::id(),
        "link-dir"
    ));
    let _ = std::fs::remove_dir_all(&victim);
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("x.txt"), "UNTOUCHED\n").unwrap();

    repo.write_file("conf/x.txt", "SECRET\n");
    repo.write_file("a.txt", "a1\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    std::fs::remove_dir_all(repo.path().join("conf")).unwrap();
    std::os::unix::fs::symlink(&victim, repo.path().join("conf")).unwrap();
    repo.write_file("a.txt", "a1\na2\n");

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"a.txt": {"hunks": [0]}, "conf": {"action": "keep"}}, "default": "reset"}"#,
        "only a.txt",
    ]);

    let survived = std::fs::read_to_string(victim.join("x.txt")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&victim);
    assert_eq!(
        survived, "UNTOUCHED\n",
        "select walked through a symlinked directory and rewrote a file outside the repo"
    );
}

/// `jj file show` prints nothing for a symlink, so `list` describes a file that
/// *became* a link as its old text being deleted, and prints a hunk id for it.
/// Selecting that id has to keep the link -- refusing to write through it is
/// not licence to quietly reset it instead.
#[cfg(unix)]
#[test]
fn selecting_the_hunk_a_new_symlink_prints_keeps_the_link() {
    let repo = TestRepo::new("select-link-becomes");
    repo.write_file("a.txt", "AAA\n");
    repo.write_file("f.txt", "FFF\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    std::fs::remove_file(repo.path().join("f.txt")).unwrap();
    std::os::unix::fs::symlink("a.txt", repo.path().join("f.txt")).unwrap();

    let ids = listed_ids_in(&repo, ".", &[]);
    let spec = format!(
        r#"{{"files": {{"f.txt": {{"ids": ["{}"]}}}}, "default": "reset"}}"#,
        ids["f.txt"][0]
    );
    repo.hunk_ok(&["split", &spec, "f.txt becomes a link"]);

    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("f.txt") && committed.contains("120000"),
        "the selected change was not committed as a link:\n{committed}"
    );
    assert_eq!(
        repo.jj_ok(&["diff", "--summary"]).trim(),
        "",
        "the selected change was left behind in the working copy"
    );
}

/// The same the other way round: a link replaced by a regular file. `list`
/// describes it as an insertion, and selecting that hunk must write the file.
#[cfg(unix)]
#[test]
fn selecting_the_hunk_that_replaces_a_symlink_writes_the_file() {
    let repo = TestRepo::new("select-link-replaced");
    repo.write_file("a.txt", "AAA\n");
    std::os::unix::fs::symlink("a.txt", repo.path().join("g.txt")).unwrap();
    repo.jj_ok(&["commit", "-m", "base"]);

    std::fs::remove_file(repo.path().join("g.txt")).unwrap();
    repo.write_file("g.txt", "GGG\n");

    let ids = listed_ids_in(&repo, ".", &[]);
    let spec = format!(
        r#"{{"files": {{"g.txt": {{"ids": ["{}"]}}}}, "default": "reset"}}"#,
        ids["g.txt"][0]
    );
    repo.hunk_ok(&["split", &spec, "g.txt becomes a file"]);

    assert_eq!(
        repo.file_at("@-", "g.txt"),
        "GGG\n",
        "the selected change did not reach the commit"
    );
    assert_eq!(
        repo.jj_ok(&["diff", "--summary"]).trim(),
        "",
        "the selected change was left behind in the working copy"
    );
}

/// jj materialises only the files that changed, so a link's target is usually
/// absent on at least one side. Reading either side as text then fails -- and
/// the failure aborts jj's whole edit over a file the spec asked to reset.
#[cfg(unix)]
#[test]
fn a_dangling_symlink_in_a_selection_is_reset_rather_than_read() {
    let repo = TestRepo::new("select-link-dangling");
    repo.write_file("a.txt", "one\n");
    std::os::unix::fs::symlink("absent-target.txt", repo.path().join("s")).unwrap();
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("a.txt", "one\ntwo\n");
    std::fs::remove_file(repo.path().join("s")).unwrap();
    std::os::unix::fs::symlink("still-absent.txt", repo.path().join("s")).unwrap();

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"a.txt": {"hunks": [0]}, "s": {"ids": []}}, "default": "reset"}"#,
        "only a.txt",
    ]);

    let summary = repo.changed_files("@-");
    assert!(
        summary.iter().any(|l| l.contains("a.txt")),
        "the selected hunk is missing: {summary:?}"
    );
    assert!(
        !summary.iter().any(|l| l.ends_with(" s")),
        "the reset link rode along into the commit: {summary:?}"
    );
    let meta = std::fs::symlink_metadata(repo.path().join("s")).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "the working copy's link was replaced by a regular file"
    );
}

// ---------------------------------------------------------------------------
// Reading the parent. `fs::read_to_string(..).unwrap_or_default()` turned any
// read error into `""`, which is exactly what a newly added file reads as. A
// parent that is not valid UTF-8 therefore looked like "no earlier content",
// and `select` committed a zero-byte file over it at exit 0.
// ---------------------------------------------------------------------------

/// `{"ids": []}` is the documented "keep nothing from this file" spelling. It
/// needs no bytes from either side to carry out, so it must work on a file no
/// hunk selection could ever describe.
#[test]
fn an_empty_selection_restores_a_parent_that_is_not_valid_utf8() {
    let repo = TestRepo::new("select-nonutf8-parent");
    let original = vec![0x66u8, 0x6f, 0x6f, 0xff, 0xfe, 0x0a];
    std::fs::write(repo.path().join("bin.dat"), &original).unwrap();
    repo.write_file("t.txt", "a\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    // The working copy is valid text, so only the *parent* is unreadable.
    repo.write_file("bin.dat", "now plain text\n");
    repo.write_file("t.txt", "a\nb\n");

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"t.txt": {"hunks": [0]}, "bin.dat": {"ids": []}}, "default": "reset"}"#,
        "only t.txt",
    ]);

    let summary = repo.changed_files("@-");
    assert!(
        !summary.iter().any(|l| l.contains("bin.dat")),
        "a file the spec kept nothing from was rewritten into the commit: {summary:?}"
    );
    assert_eq!(
        file_bytes_at(&repo, "@-", "bin.dat"),
        original,
        "the parent's bytes were destroyed"
    );
    assert_eq!(
        file_bytes_at(&repo, "@", "bin.dat"),
        b"now plain text\n".to_vec(),
        "the working copy's change was not left behind"
    );
}

/// The mirror image: the *right* side is the unreadable one. Erroring there
/// aborted jj's whole edit over a file the spec had asked to reset.
#[test]
fn an_empty_selection_resets_a_working_copy_that_is_not_valid_utf8() {
    let repo = TestRepo::new("select-nonutf8-child");
    repo.write_file("bin.dat", "plain\n");
    repo.write_file("t.txt", "a\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    let garbled = vec![0x00u8, 0x01, 0xff, 0xfe];
    std::fs::write(repo.path().join("bin.dat"), &garbled).unwrap();
    repo.write_file("t.txt", "a\nb\n");

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"t.txt": {"hunks": [0]}, "bin.dat": {"ids": []}}, "default": "reset"}"#,
        "only t.txt",
    ]);

    let summary = repo.changed_files("@-");
    assert!(
        !summary.iter().any(|l| l.contains("bin.dat")),
        "the reset binary rode along into the commit: {summary:?}"
    );
    assert_eq!(
        file_bytes_at(&repo, "@", "bin.dat"),
        garbled,
        "the working copy's binary change was not left behind"
    );
}

/// A selection that really does name hunks in a file whose parent cannot be
/// read is a mistake, and it has to say so. Silently reading the parent as
/// empty made the whole file look newly added.
#[test]
fn a_selection_against_an_unreadable_parent_names_the_file() {
    let repo = TestRepo::new("select-nonutf8-named");
    std::fs::write(repo.path().join("bin.dat"), [0x66u8, 0xff, 0xfe, 0x0a]).unwrap();
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("bin.dat", "now plain text\n");

    let ids = listed_ids_in(&repo, ".", &["--binary", "include"]);
    let id = ids["bin.dat"][0].clone();
    let spec = format!(r#"{{"files": {{"bin.dat": {{"ids": ["{id}"]}}}}}}"#);

    let err = repo.hunk_fail(&["split", &spec, "msg", "--allow-empty"]);
    assert!(
        err.contains("bin.dat"),
        "the failure must name the offending file: {err}"
    );
    let log = repo.log_descriptions();
    assert!(
        !log.iter().any(|d| d == "msg"),
        "nothing should have been committed: {log:?}"
    );
}

// ---------------------------------------------------------------------------
// Path frames. jj materialises the diff directories with repo-relative paths,
// but every path jj *prints* -- and so every spec key -- is relative to the
// cwd. From a subdirectory the two disagree, and `select` matched nothing: an
// empty commit at exit 0.
//
// Hunk ids are hashed in the repo-relative frame instead, which is the only one
// that reads the same from every directory. Hashing the printed path made an id
// a property of where the question was asked from, so a spec produced at the
// root named nothing one level down -- and its keys, still cwd-relative, failed
// first. Both halves are exercised below.
// ---------------------------------------------------------------------------

/// Two edited files under `pkg/`, to be split from inside `pkg/`.
fn subdir_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("pkg/a.txt", "a1\n");
    repo.write_file("pkg/b.txt", "b1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("pkg/a.txt", "a1\na2\n");
    repo.write_file("pkg/b.txt", "b1\nb2\n");
    repo
}

#[test]
fn a_split_from_a_subdirectory_commits_the_file_the_listing_named() {
    let repo = subdir_repo("subdir-split");

    hunk_in_ok(&repo, "pkg", &["split", r#"file("a.txt")"#, "only-a"]);

    let committed = repo.changed_files("@-");
    assert_eq!(
        committed,
        vec!["M pkg/a.txt".to_string()],
        "the selection did not reach the file it named: {committed:?}"
    );
    let remaining = repo.changed_files("@");
    assert_eq!(
        remaining,
        vec!["M pkg/b.txt".to_string()],
        "the unselected file did not stay behind: {remaining:?}"
    );
}

/// The spec keys a user copies out of `list` are the ones that have to work.
#[test]
fn a_hand_written_spec_from_a_subdirectory_uses_the_printed_keys() {
    let repo = subdir_repo("subdir-keys");
    let ids = listed_ids_in(&repo, "pkg", &[]);
    assert!(
        ids.contains_key("a.txt"),
        "precondition: `list` from pkg/ prints cwd-relative keys: {ids:?}"
    );
    let spec = format!(
        r#"{{"files": {{"a.txt": {{"ids": ["{}"]}}}}, "default": "reset"}}"#,
        ids["a.txt"][0]
    );

    hunk_in_ok(&repo, "pkg", &["split", &spec, "only-a"]);

    assert_eq!(repo.changed_files("@-"), vec!["M pkg/a.txt".to_string()]);
}

/// Two levels down, and a file *above* the cwd. jj spells that one `../a.txt`,
/// and so must everything that reads or writes it.
#[test]
fn a_split_two_directories_down_reaches_files_above_the_cwd() {
    let repo = TestRepo::new("subdir-nested");
    repo.write_file("pkg/deep/d.txt", "d1\n");
    repo.write_file("pkg/a.txt", "a1\n");
    repo.write_file("root.txt", "r1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("pkg/deep/d.txt", "d1\nd2\n");
    repo.write_file("pkg/a.txt", "a1\na2\n");
    repo.write_file("root.txt", "r1\nr2\n");

    let ids = listed_ids_in(&repo, "pkg/deep", &[]);
    assert!(
        ids.contains_key("d.txt") && ids.contains_key("../a.txt"),
        "precondition: jj prints paths relative to the cwd: {ids:?}"
    );

    let spec = format!(
        r#"{{"files": {{"d.txt": {{"ids": ["{}"]}}, "../a.txt": {{"ids": ["{}"]}}}}, "default": "reset"}}"#,
        ids["d.txt"][0], ids["../a.txt"][0]
    );
    hunk_in_ok(&repo, "pkg/deep", &["split", &spec, "two of three"]);

    let mut committed = repo.changed_files("@-");
    committed.sort();
    assert_eq!(
        committed,
        vec!["M pkg/a.txt".to_string(), "M pkg/deep/d.txt".to_string()],
        "the nested selection landed somewhere else: {committed:?}"
    );
    assert_eq!(repo.changed_files("@"), vec!["M root.txt".to_string()]);
}

/// A root-relative key used to be refused from a subdirectory, on the reasoning
/// that there was one frame and every other spelling was a mistake. That made
/// every spec bound to the directory it was written in: the keys `list`
/// prints at the repo root are root-relative, so a root-generated spec named
/// nothing one level down and `split` reported `pkg/a.txt: no such path in the
/// diff` for a path plainly in the diff.
///
/// Root-relative is now accepted as well, because it is the one spelling that
/// means the same file from every directory. The cwd-relative spelling still
/// wins where both would match, so nothing that resolved before resolves
/// differently now.
#[test]
fn a_root_relative_key_resolves_from_a_subdirectory() {
    let repo = subdir_repo("subdir-root-frame-key");
    let ids = listed_ids_in(&repo, "pkg", &[]);
    let spec = format!(
        r#"{{"files": {{"pkg/a.txt": {{"ids": ["{}"]}}}}, "default": "reset"}}"#,
        ids["a.txt"][0]
    );

    hunk_in_ok(&repo, "pkg", &["split", &spec, "root frame"]);

    assert_eq!(
        repo.changed_files("@-"),
        vec!["M pkg/a.txt".to_string()],
        "the root-relative key did not reach the file it named"
    );
}

/// Accepting a second frame must not turn a typo into a silent no-op. A key
/// that names no file in *either* spelling is still refused, and still refused
/// loudly enough that no empty commit is made.
#[test]
fn a_key_in_no_frame_at_all_is_still_refused() {
    let repo = subdir_repo("subdir-no-frame");
    let ids = listed_ids_in(&repo, "pkg", &[]);
    let spec = format!(
        r#"{{"files": {{"nowhere/a.txt": {{"ids": ["{}"]}}}}, "default": "reset"}}"#,
        ids["a.txt"][0]
    );

    let err = hunk_in_fail(&repo, "pkg", &["split", &spec, "no frame"]);
    assert!(
        err.contains("nowhere/a.txt"),
        "the refusal must name the key that did not resolve: {err}"
    );
    let log = repo.log_descriptions();
    assert!(
        !log.iter().any(|d| d == "no frame"),
        "an empty commit was made anyway: {log:?}"
    );
}

/// A hunk's id is a property of the hunk. It used to be a property of the
/// directory the question was asked from: the path is hashed into the id, and
/// the path that arrived was the cwd-relative one jj prints, so `sub/f.txt`
/// listed at the root and `f.txt` listed from `sub/` -- one hunk, one file --
/// were given two different ids. Every id below is listed from a different
/// directory and every one of them has to be the same string.
#[test]
fn a_hunk_id_is_the_same_from_every_directory_it_is_listed_from() {
    let repo = TestRepo::new("id-frame-independent");
    repo.write_file("top.txt", "t1\n");
    repo.write_file("sub/mid.txt", "m1\n");
    repo.write_file("sub/deep/low.txt", "l1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("top.txt", "t1\nt2\n");
    repo.write_file("sub/mid.txt", "m1\nm2\n");
    repo.write_file("sub/deep/low.txt", "l1\nl2\n");

    let at_root = listed_ids_in(&repo, "", &[]);
    let at_sub = listed_ids_in(&repo, "sub", &[]);
    let at_deep = listed_ids_in(&repo, "sub/deep", &[]);

    // Each file under the spelling that directory prints for it -- including
    // the `..` forms, which are the ones that used to differ most.
    for (root_key, sub_key, deep_key) in [
        ("top.txt", "../top.txt", "../../top.txt"),
        ("sub/mid.txt", "mid.txt", "../mid.txt"),
        ("sub/deep/low.txt", "deep/low.txt", "low.txt"),
    ] {
        assert_eq!(
            at_root[root_key], at_sub[sub_key],
            "{root_key} changed id between the root and sub/"
        );
        assert_eq!(
            at_root[root_key], at_deep[deep_key],
            "{root_key} changed id between the root and sub/deep/"
        );
    }
}

/// The workflow the id bug actually broke, end to end: generate a spec where
/// the docs generate one -- at the repo root -- and run it from somewhere else.
///
/// Both halves of the bug are in the way of this. The keys are cwd-relative, so
/// `sub/mid.txt` matched nothing from `sub/` and the spec was rejected before
/// any id was looked at; and had it resolved, the ids were hashed per-directory
/// and would have matched nothing either. Fixing only one of the two leaves
/// this test failing, which is why it is the one that speaks for the fix.
#[test]
fn a_spec_generated_at_the_root_applies_from_a_subdirectory() {
    let repo = TestRepo::new("root-spec-from-sub");
    repo.write_file("top.txt", "t1\n");
    repo.write_file("sub/mid.txt", "m1\n");
    repo.write_file("sub/other.txt", "o1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("top.txt", "t1\nt2\n");
    repo.write_file("sub/mid.txt", "m1\nm2\n");
    repo.write_file("sub/other.txt", "o1\no2\n");

    // Keys and ids exactly as a user reads them at the root.
    let at_root = listed_ids_in(&repo, "", &[]);
    let spec = format!(
        r#"{{"files": {{"top.txt": {{"ids": ["{}"]}}, "sub/mid.txt": {{"ids": ["{}"]}}}}, "default": "reset"}}"#,
        at_root["top.txt"][0], at_root["sub/mid.txt"][0]
    );

    hunk_in_ok(&repo, "sub", &["split", &spec, "from the root spec"]);

    let mut committed = repo.changed_files("@-");
    committed.sort();
    assert_eq!(
        committed,
        vec!["M sub/mid.txt".to_string(), "M top.txt".to_string()],
        "the root-generated spec selected the wrong files from sub/: {committed:?}"
    );
    assert_eq!(
        repo.changed_files("@"),
        vec!["M sub/other.txt".to_string()],
        "the unselected file did not stay behind"
    );
}

/// `list` and `select` have to agree on a hunk's id, and this is the case that
/// tells whether they agree on the *frame* rather than merely on the string.
/// Both running in one directory agreed even while both were wrong, because
/// they were wrong identically. Here the ids come from the root and `select`
/// runs from `sub/`, so nothing but a directory-independent id resolves.
#[test]
fn select_resolves_an_id_that_list_printed_from_another_directory() {
    let repo = subdir_repo("subdir-cross-frame-ids");

    let at_root = listed_ids_in(&repo, "", &[]);
    let at_sub = listed_ids_in(&repo, "pkg", &[]);
    assert_eq!(
        at_root["pkg/a.txt"], at_sub["a.txt"],
        "precondition: the id is the same from both directories"
    );

    // Keyed the way `list` printed it in `pkg/`, but carrying the id the root
    // invocation handed out.
    let spec = format!(
        r#"{{"files": {{"a.txt": {{"ids": ["{}"]}}}}, "default": "reset"}}"#,
        at_root["pkg/a.txt"][0]
    );

    hunk_in_ok(&repo, "pkg", &["split", &spec, "cross frame"]);

    assert_eq!(
        repo.changed_files("@-"),
        vec!["M pkg/a.txt".to_string()],
        "an id printed at the root did not resolve for a select run from pkg/"
    );
}

/// A backslash is an ordinary character in a Unix filename. Rewriting `\` to
/// `/` on the way into the hash -- which Windows genuinely needs, because there
/// it is the separator `select` reads out of the filesystem -- silently moved
/// the id of every such file at the repo root, which is exactly what this
/// change must never do. The id has to be both stable and frame-independent.
#[cfg(unix)]
#[test]
fn a_backslash_in_a_filename_does_not_disturb_the_id() {
    let repo = TestRepo::new("backslash-filename");
    repo.write_file(r"sub/back\slash.txt", "one\ntwo\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file(r"sub/back\slash.txt", "one\nTWO\n");

    let at_root = listed_ids_in(&repo, "", &[]);
    let at_sub = listed_ids_in(&repo, "sub", &[]);

    // Pinned to the value this hunk had before hashing moved frames. Equality
    // between the two directories is not enough on its own: rewriting `\` to
    // `/` moves both of them *together*, so they agree on an id that no
    // previously written spec, and no earlier release, would recognise.
    assert_eq!(
        at_root[r"sub/back\slash.txt"][0],
        "hunk-36879bfd49553dfc70108ab2a551692acc3f4e9781c5ac5edcfb1441e8dc1e42",
        "the id of a backslash-named file at the root moved"
    );
    assert_eq!(
        at_root[r"sub/back\slash.txt"],
        at_sub[r"back\slash.txt"],
        "the backslash was read as a directory separator: {at_root:?} vs {at_sub:?}"
    );
}

/// The counter-guard, and the only test here that is meant to pass both before
/// and after the change: ids generated at the repo root must not move at all.
/// They are the ones README.md and SKILL.md quote, and a fix that made a hunk's
/// id independent of the cwd by changing the id everyone already has would be
/// no fix. This pins one to its literal value so that any future reworking of
/// the hash has to be a deliberate act.
#[test]
fn a_root_generated_id_keeps_its_documented_value() {
    let repo = TestRepo::new("root-id-is-pinned");
    repo.write_file("sub/f.txt", "one\ntwo\nthree\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("sub/f.txt", "one\nTWO\nthree\n");

    let at_root = listed_ids_in(&repo, "", &[]);
    assert_eq!(
        at_root["sub/f.txt"][0],
        "hunk-335a56dd88e461360387e7cb456cad6cfa27797cdb07c91b66ad8a380728136f"
    );
}

// ---------------------------------------------------------------------------
// The executable bit is not a hunk, so it cannot be selected on its own. It
// follows the content: kept with the hunks that were kept, reset with them
// when nothing was.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_chmod_rides_along_with_the_hunks_it_accompanies() {
    let repo = TestRepo::new("exec-rides-along");
    repo.write_file("s.sh", "S1\nS2\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("s.sh", "S1\nS2\nS3\n");
    chmod_exec(&repo.path().join("s.sh"));

    repo.hunk_ok(&[
        "split",
        r#"{"files": {"s.sh": {"hunks": [0]}}, "default": "reset"}"#,
        "content and mode",
    ]);

    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("+S3"),
        "the selected content hunk is missing:\n{committed}"
    );
    assert!(
        committed.contains("new mode 100755"),
        "the chmod was stripped from the hunks it came with:\n{committed}"
    );
    assert_eq!(
        repo.jj_ok(&["diff", "--summary"]).trim(),
        "",
        "something was left behind in the working copy"
    );
}

/// Selecting only part of a file still keeps that file's change, so the mode
/// goes with it. There is nowhere else for it to go: the remainder is what is
/// left over, and a mode has no halves.
#[cfg(unix)]
#[test]
fn a_chmod_rides_along_when_only_some_hunks_are_selected() {
    let repo = TestRepo::new("exec-rides-partial");
    repo.write_file("s.sh", "L1\nL2\nL3\nL4\nL5\nL6\nL7\nL8\nL9\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("s.sh", "TOP\nL1\nL2\nL3\nL4\nL5\nL6\nL7\nL8\nL9\nBOTTOM\n");
    chmod_exec(&repo.path().join("s.sh"));

    let listing: serde_json::Value =
        serde_json::from_str(&repo.hunk_ok(&["list", "--format", "json"])).unwrap();
    let hunks = listing["files"][0]["hunks"].as_array().unwrap();
    assert_eq!(hunks.len(), 2, "precondition: two separate hunks");
    let first = hunks[0]["id"].as_str().unwrap();

    let spec = format!(r#"{{"files": {{"s.sh": {{"ids": ["{first}"]}}}}, "default": "reset"}}"#);
    repo.hunk_ok(&["split", &spec, "top only"]);

    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("+TOP") && !committed.contains("+BOTTOM"),
        "the wrong hunk was selected:\n{committed}"
    );
    assert!(
        committed.contains("new mode 100755"),
        "the chmod was stripped from a partially selected file:\n{committed}"
    );
}

/// `diffedit` rewrites a revision in place, so a mode dropped here is dropped
/// from history: there is no remainder commit left holding it.
#[cfg(unix)]
#[test]
fn diffedit_keeping_every_hunk_keeps_the_exec_bit() {
    let repo = TestRepo::new("exec-diffedit");
    repo.write_file("s.sh", "S1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("s.sh", "S1\nS2\n");
    chmod_exec(&repo.path().join("s.sh"));
    repo.jj_ok(&["commit", "-m", "edit"]);

    let before = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        before.contains("new mode 100755"),
        "precondition: the revision carries the chmod:\n{before}"
    );

    let ids = listed_ids_in(&repo, ".", &["-r", "@-"]);
    let spec = format!(
        r#"{{"files": {{"s.sh": {{"ids": ["{}"]}}}}, "default": "reset"}}"#,
        ids["s.sh"][0]
    );
    repo.hunk_ok(&["diffedit", "-r", "@-", &spec]);

    let after = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(after.contains("+S2"), "the kept hunk was dropped:\n{after}");
    assert!(
        after.contains("new mode 100755"),
        "keeping every hunk still discarded the chmod from history:\n{after}"
    );
}

// ---------------------------------------------------------------------------
// --format diff under a spec: after-side coordinates
// ---------------------------------------------------------------------------

/// As [`patch_for`], but the listing is filtered first, so the emitted patch is
/// a strict subset of the diff. That is the case where the after-side line
/// numbers stop being readable off the working copy.
fn patch_for_spec(repo: &TestRepo, name: &str, base: &str, modified: &str, spec: &str) -> String {
    repo.write_file(name, base);
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file(name, modified);
    let patch = repo.hunk_ok(&["list", "--format", "diff", "--spec", spec]);
    repo.write_file(name, base); // reset to base for a clean apply
    patch
}

/// `n` copies of one 7-line block, all identical.
///
/// The repetition is the point. `git apply` anchors its search on the `@@`
/// after-side start and slides to the nearest match, so against unique context
/// a wrong start still lands on the right lines and no assertion about the file
/// could see it. Identical blocks make a wrong start rewrite a *different*
/// block, which the content does show.
fn repeated_blocks(n: usize) -> String {
    "X\nY\nZ\nMARK\nP\nQ\nR\n".repeat(n)
}

fn lines_of(text: &str) -> Vec<String> {
    text.lines().map(|l| format!("{l}\n")).collect()
}

/// Just the ranges of each `@@` header, without the trailing scope and id.
fn hunk_headers(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter(|l| l.starts_with("@@"))
        .map(|l| format!("{} @@", l.split(" @@").next().unwrap_or(l)))
        .collect()
}

/// A spec that drops an *insertion* sitting before the hunk it keeps.
///
/// The after-side start used to be read straight off the working-copy
/// coordinate, so it still counted the 20 lines this patch does not carry.
/// `git apply` reported "Hunk #1 succeeded at 36 (offset -13 lines) ... Applied
/// patch cleanly" and rewrote the sixth block instead of the fifth, at exit 0.
#[test]
fn diff_format_spec_filtered_patch_edits_the_hunks_own_lines() {
    let repo = TestRepo::new("diff-fmt-spec-dropped-insert");
    let base = repeated_blocks(6);

    let mut body = lines_of(&base);
    body[31] = "MARK-EDITED\n".to_string(); // the fifth MARK: parent line 32
    let top: String = (1..=20).map(|i| format!("TOP{i}\n")).collect();
    let modified = format!("{top}{}", body.concat());

    let patch = patch_for_spec(&repo, "f.txt", &base, &modified, "type(replace)");

    // The file first: a header nobody applies proves nothing, and asserting on
    // the header alone is how this survived a suite that already checked it.
    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected:\n{err}\npatch:\n{patch}");

    let mut expected = lines_of(&base);
    expected[31] = "MARK-EDITED\n".to_string();
    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(
        got,
        expected.concat(),
        "the patch rewrote a different block\npatch:\n{patch}"
    );

    assert_eq!(
        hunk_headers(&patch),
        vec!["@@ -29,7 +29,7 @@"],
        "the dropped insertion must not be counted on the + side\n{patch}"
    );
}

/// The same, with the dropped hunk a *deletion*: that skews the after side the
/// other way, putting the `+` start 20 lines too early rather than too late.
#[test]
fn diff_format_spec_filtered_patch_survives_a_dropped_deletion() {
    let repo = TestRepo::new("diff-fmt-spec-dropped-delete");
    let head: String = (1..=20).map(|i| format!("DROPME{i}\n")).collect();
    let blocks = repeated_blocks(6);
    let base = format!("{head}{blocks}");

    // Delete the head, and edit the fifth MARK -- parent line 20 + 32 = 52.
    let mut body = lines_of(&blocks);
    body[31] = "MARK-EDITED\n".to_string();
    let modified = body.concat();

    let patch = patch_for_spec(&repo, "f.txt", &base, &modified, "type(replace)");

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected:\n{err}\npatch:\n{patch}");

    let mut expected = lines_of(&base);
    expected[51] = "MARK-EDITED\n".to_string();
    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(
        got,
        expected.concat(),
        "the patch rewrote a different block\npatch:\n{patch}"
    );

    assert_eq!(
        hunk_headers(&patch),
        vec!["@@ -49,7 +49,7 @@"],
        "the dropped deletion must not be counted on the + side\n{patch}"
    );
}

/// Several dropped hunks, and two kept ones: the second kept block has to be
/// offset by what the *first kept block* added and by nothing else. Excluding
/// the dropped hunks but forgetting the kept ones leaves this one wrong.
#[test]
fn diff_format_spec_filtered_patch_accumulates_only_what_it_emits() {
    let repo = TestRepo::new("diff-fmt-spec-many-dropped");
    let base = repeated_blocks(12);

    let mut body = lines_of(&base);
    // Kept: one MARK grows into two lines (block 4, parent line 25).
    body[24] = "M4a\nM4b\n".to_string();
    // Kept: another MARK is replaced one for one (block 11, parent line 74).
    body[73] = "M11\n".to_string();
    // Dropped: an insertion between the two, and one at the very top.
    body.insert(49, "MID1\nMID2\nMID3\nMID4\nMID5\nMID6\n".to_string());
    let top: String = (1..=4).map(|i| format!("TOP{i}\n")).collect();
    let modified = format!("{top}{}", body.concat());

    let patch = patch_for_spec(&repo, "f.txt", &base, &modified, "type(replace)");

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected:\n{err}\npatch:\n{patch}");

    let mut expected = lines_of(&base);
    expected[24] = "M4a\nM4b\n".to_string();
    expected[73] = "M11\n".to_string();
    let got = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(
        got,
        expected.concat(),
        "the patch rewrote different blocks\npatch:\n{patch}"
    );

    assert_eq!(
        hunk_headers(&patch),
        // The second block starts one line later than its before-side twin:
        // the one line the first block adds, and none of the 10 dropped.
        vec!["@@ -22,7 +22,8 @@", "@@ -71,7 +72,7 @@"],
        "after-side starts must count only the emitted hunks\n{patch}"
    );
}

/// Two files in one patch. The running offset belongs to a file, not to the
/// patch: carrying it across the `---` boundary would skew the second file by
/// whatever the first one happened to add.
#[test]
fn diff_format_spec_filtered_patch_restarts_the_offset_per_file() {
    let repo = TestRepo::new("diff-fmt-spec-two-files");
    let base = repeated_blocks(6);

    let mut body = lines_of(&base);
    // The fifth MARK becomes six lines. The size matters: a carried-over offset
    // has to be big enough to put the next file's anchor closer to the *next*
    // identical block than to its own, or `git apply` slides back and the file
    // comes out right by luck.
    body[31] = "E1a\nE1b\nE1c\nE1d\nE1e\nE1f\n".to_string();
    let top: String = (1..=9).map(|i| format!("TOP{i}\n")).collect();
    let first = format!("{top}{}", body.concat());

    let mut other = lines_of(&base);
    other[31] = "E2\n".to_string();
    let second = format!("{top}{}", other.concat());

    repo.write_file("a.txt", &base);
    repo.write_file("b.txt", &base);
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.txt", &first);
    repo.write_file("b.txt", &second);
    let patch = repo.hunk_ok(&["list", "--format", "diff", "--spec", "type(replace)"]);
    repo.write_file("a.txt", &base);
    repo.write_file("b.txt", &base);

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected:\n{err}\npatch:\n{patch}");

    let mut expected_a = lines_of(&base);
    expected_a[31] = "E1a\nE1b\nE1c\nE1d\nE1e\nE1f\n".to_string();
    let mut expected_b = lines_of(&base);
    expected_b[31] = "E2\n".to_string();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        expected_a.concat(),
        "patch:\n{patch}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("b.txt")).unwrap(),
        expected_b.concat(),
        "patch:\n{patch}"
    );

    assert_eq!(
        hunk_headers(&patch),
        vec!["@@ -29,7 +29,12 @@", "@@ -29,7 +29,7 @@"],
        "each file's offset starts at zero\n{patch}"
    );
}

/// Dropping the *middle* of three hunks close enough to share one `@@` block.
/// The two survivors still merge, and the lines between them -- including the
/// ones the dropped hunk would have changed -- have to be emitted as context,
/// because this patch does not change them.
#[test]
fn diff_format_spec_filtered_patch_drops_a_hunk_from_inside_a_block() {
    let repo = TestRepo::new("diff-fmt-spec-middle-dropped");
    let base: String = (1..=20).map(|i| format!("L{i}\n")).collect();
    let modified: String = (1..=20)
        .map(|i| match i {
            8 => "R8\n".to_string(),   // kept   (replace)
            10 => "L10\nINS\n".to_string(), // dropped (insert)
            12 => "R12\n".to_string(), // kept   (replace)
            _ => format!("L{i}\n"),
        })
        .collect();

    let patch = patch_for_spec(&repo, "f.txt", &base, &modified, "type(replace)");
    assert_eq!(
        patch.lines().filter(|l| l.starts_with("@@")).count(),
        1,
        "the survivors are 4 lines apart and must stay one block\n{patch}"
    );

    let (ok, err) = git_apply(repo.path(), &patch);
    assert!(ok, "git apply rejected:\n{err}\npatch:\n{patch}");

    let expected: String = (1..=20)
        .map(|i| match i {
            8 => "R8\n".to_string(),
            12 => "R12\n".to_string(),
            _ => format!("L{i}\n"),
        })
        .collect();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("f.txt")).unwrap(),
        expected,
        "patch:\n{patch}"
    );
}

// ---------------------------------------------------------------------------
// hunksets and binary files
// ---------------------------------------------------------------------------

/// `all()` used to mean "all except the binaries": the expression was evaluated
/// over a diff with binary files skipped, so nothing could name one, while the
/// spec it produced still said `default: reset`. The binary change was then
/// dropped from whatever the verb went on to do, with no warning.
#[test]
fn hunkset_all_selects_a_binary_files_change_too() {
    let repo = binary_repo("hunkset-binary-all");

    repo.hunk_ok(&["commit", "all()", "commit everything"]);

    let mut committed = repo.changed_files("@-");
    committed.sort();
    assert_eq!(
        committed,
        vec!["M bin.dat", "M t.txt"],
        "the commit is missing the binary change"
    );
    assert!(
        repo.changed_files("@").is_empty(),
        "all() left something behind: {:?}",
        repo.changed_files("@")
    );
}

/// And what lands in the commit is the bytes, not a lossy re-encoding of them:
/// `select` rebuilds a text file line by line, which is exactly what a binary
/// file must not be put through.
#[test]
fn hunkset_all_keeps_binary_bytes_intact() {
    let repo = binary_repo("hunkset-binary-bytes");
    let expected = std::fs::read(repo.path().join("bin.dat")).unwrap();

    repo.hunk_ok(&["commit", "all()", "commit everything"]);

    let shown = repo.jj(&["file", "show", "-r", "@-", "file:bin.dat"]);
    assert!(shown.status.success(), "jj file show failed");
    assert_eq!(
        shown.stdout, expected,
        "the committed binary is not byte for byte what was in the working copy"
    );
}

/// `list --spec all()` has to show what the unfiltered listing shows. It used
/// to hide the binary file that plain `list` reports.
#[test]
fn hunkset_list_spec_shows_the_binary_file_all_selects() {
    let repo = binary_repo("hunkset-binary-list");

    let plain = repo.hunk_ok(&["list", "--format", "text"]);
    let filtered = repo.hunk_ok(&["list", "--format", "text", "--spec", "all()"]);

    assert!(plain.contains("bin.dat"), "{plain}");
    assert_eq!(filtered, plain, "all() must not hide the binary file");
}

/// A file-level predicate names a binary file, and only that file.
#[test]
fn hunkset_file_predicate_can_name_a_binary_file_on_its_own() {
    let repo = binary_repo("hunkset-binary-file");

    repo.hunk_ok(&["commit", "file(\"bin.dat\")", "just the blob"]);

    assert_eq!(repo.changed_files("@-"), vec!["M bin.dat"]);
    assert_eq!(repo.changed_files("@"), vec!["M t.txt"]);
}

/// A content-level predicate must not reach one: there are no lines in a binary
/// file for it to have matched, so selecting it would be a guess.
#[test]
fn hunkset_content_predicate_does_not_reach_a_binary_file() {
    let repo = binary_repo("hunkset-binary-content");

    repo.hunk_ok(&["commit", "added(\"A\")", "just the text"]);

    assert_eq!(repo.changed_files("@-"), vec!["M t.txt"]);
    assert_eq!(repo.changed_files("@"), vec!["M bin.dat"]);
}

/// Nor a line-range one. A binary file occupies no line the user could have
/// been asking about, so `lines()` over the whole file still means the text.
#[test]
fn hunkset_line_range_predicate_does_not_reach_a_binary_file() {
    let repo = binary_repo("hunkset-binary-lines");

    repo.hunk_ok(&["commit", "lines(1..10000)", "just the text"]);

    assert_eq!(repo.changed_files("@-"), vec!["M t.txt"]);
    assert_eq!(repo.changed_files("@"), vec!["M bin.dat"]);
}

/// Negation reaches it, though: `~file("t.txt")` is a file-level question, and
/// the binary file is one of the answers.
#[test]
fn hunkset_negation_reaches_a_binary_file() {
    let repo = binary_repo("hunkset-binary-negation");

    repo.hunk_ok(&["commit", "~file(\"t.txt\")", "everything but the text"]);

    assert_eq!(repo.changed_files("@-"), vec!["M bin.dat"]);
    assert_eq!(repo.changed_files("@"), vec!["M t.txt"]);
}

/// A binary file that is *added* rather than modified: `status()` and
/// `extension()` are file-level too, and `type()` reports what happened to the
/// file as a whole -- an added binary is an insertion.
#[test]
fn hunkset_status_and_extension_name_an_added_binary_file() {
    let repo = TestRepo::new("hunkset-binary-added");
    repo.write_file("t.txt", "a\nb\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("t.txt", "A\nb\n");
    std::fs::write(repo.path().join("new.dat"), [0u8, 1, 2, 0, 255]).unwrap();

    let listed = repo.hunk_ok(&["list", "--format", "text", "--spec", "status(\"added\")"]);
    assert!(listed.contains("new.dat"), "{listed}");
    assert!(!listed.contains("t.txt"), "{listed}");

    repo.hunk_ok(&["commit", "extension(\"dat\")", "just the blob"]);
    assert_eq!(repo.changed_files("@-"), vec!["A new.dat"]);
    assert_eq!(repo.changed_files("@"), vec!["M t.txt"]);
}

// ---------------------------------------------------------------------------
// hunksets and the rest of the hunkless shapes
//
// A binary was only the narrowest instance of the bug the stand-in above was
// written to stop. Every predicate matches *hunks*, so the other four hunkless
// shapes -- a retargeted symlink, a mode-only flip, a rename whose text did not
// move, an empty file added or removed -- could not be named by any expression
// at all, while the spec that came back still said `default: reset`. `split
// 'all()'`, which reads as "commit everything", therefore committed a subset
// and left the rest in the working copy at exit 0.
//
// The generalisation is the same trade the binary made: give every hunkless
// change a stand-in hunk carrying only what a *file-level* predicate reads, so
// it can be named, and rewrite a selected one into a whole-file action, because
// its id names nothing `select` could resolve.
// ---------------------------------------------------------------------------

/// The only way to produce a mode-only change: jj's exec bit is not part of any
/// hunk, so flipping it is a change with nothing to select inside it.
#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// The five hunkless shapes in one working copy, in `jj diff --summary` order,
/// plus the one ordinary edit that shares it.
#[cfg(unix)]
const EVERY_SHAPE: [&str; 6] = [
    "M blob.bin",
    "A empty-add.txt",
    "M link",
    "R {moved.txt => moved_elsewhere.txt}",
    "M script.sh",
    "M text.txt",
];

/// A working copy holding one of every shape at once: five changes no hunk can
/// express, and one ordinary edit so that a selector always has something real
/// it could have preferred instead.
///
/// The precondition is asserted, not assumed, because two of these shapes are
/// only what they look like by jj's leave. Rename detection has a similarity
/// threshold, and a file below it is reported as a delete plus an add -- two
/// entries that carry their own hunks, so every test here would still pass
/// while exercising a shape that was never broken. `moved.txt` is renamed with
/// its bytes untouched, which is as far above the threshold as it goes.
#[cfg(unix)]
fn hunkless_shapes_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("text.txt", "line one\n");
    std::fs::write(repo.path().join("blob.bin"), [0u8, 1, 2, 0, 255, 254]).unwrap();
    repo.write_file("old-target.txt", "OLD\n");
    repo.write_file("new-target.txt", "NEW\n");
    std::os::unix::fs::symlink("old-target.txt", repo.path().join("link")).unwrap();
    repo.write_file("script.sh", "echo hi\n");
    repo.write_file("moved.txt", "content that does not move\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    repo.write_file("text.txt", "line one\nline two\n");
    std::fs::write(repo.path().join("blob.bin"), [0u8, 1, 2, 0, 255, 253, 9, 9]).unwrap();
    std::fs::remove_file(repo.path().join("link")).unwrap();
    std::os::unix::fs::symlink("new-target.txt", repo.path().join("link")).unwrap();
    repo.write_file("empty-add.txt", "");
    std::fs::rename(
        repo.path().join("moved.txt"),
        repo.path().join("moved_elsewhere.txt"),
    )
    .unwrap();
    make_executable(&repo.path().join("script.sh"));

    let mut summary = repo.changed_files("@");
    summary.sort();
    let mut expected = EVERY_SHAPE.to_vec();
    expected.sort();
    assert_eq!(
        summary, expected,
        "jj does not see the six shapes this fixture is made of, so every test \
         built on it would pass for the wrong reason"
    );
    repo
}

/// `jj diff --summary` for a revision, sorted, so a test can name a set rather
/// than depend on the order jj happens to print.
fn sorted_changes(repo: &TestRepo, rev: &str) -> Vec<String> {
    let mut changes = repo.changed_files(rev);
    changes.sort();
    changes
}

/// Everything in `EVERY_SHAPE` except the entries whose text contains one of
/// `without`.
#[cfg(unix)]
fn every_shape_but(without: &[&str]) -> Vec<String> {
    let mut kept: Vec<String> = EVERY_SHAPE
        .iter()
        .filter(|line| !without.iter().any(|drop| line.contains(drop)))
        .map(|line| line.to_string())
        .collect();
    kept.sort();
    kept
}

/// The reproduction. `all()` reads as "commit everything" and used to commit
/// two of six changes -- the binary, which had a stand-in, and the one file
/// with a real hunk -- leaving a retargeted link, a rename, a mode flip and an
/// added empty file in the working copy. At exit 0, with no warning, which is
/// why this asserts on what landed rather than on the status.
#[cfg(unix)]
#[test]
fn hunkset_all_commits_every_hunkless_shape_not_just_the_binary() {
    let repo = hunkless_shapes_repo("hunkless-all");
    let expected_blob = std::fs::read(repo.path().join("blob.bin")).unwrap();

    repo.hunk_ok(&["split", "all()", "committed by all()"]);

    let mut expected = EVERY_SHAPE.to_vec();
    expected.sort();
    assert_eq!(
        sorted_changes(&repo, "@-"),
        expected,
        "all() committed a subset of the diff"
    );
    assert!(
        repo.changed_files("@").is_empty(),
        "all() left changes in the working copy: {:?}",
        repo.changed_files("@")
    );

    // Contents, not just names: a shape can reach the commit as the wrong
    // thing. A link is the one that fails silently -- rebuilt as a regular
    // file holding its target's bytes, it still shows up as `M link`.
    assert_eq!(repo.file_at("@-", "text.txt"), "line one\nline two\n");
    let shown = repo.jj(&["file", "show", "-r", "@-", "file:blob.bin"]);
    assert_eq!(
        shown.stdout, expected_blob,
        "the binary did not land byte for byte"
    );
    let committed = repo.jj_ok(&["diff", "-r", "@-", "--git"]);
    assert!(
        committed.contains("120000") && committed.contains("+new-target.txt"),
        "the retarget did not reach the commit as a link:\n{committed}"
    );
    assert!(
        committed.contains("100755"),
        "the exec bit did not reach the commit:\n{committed}"
    );

    // And the working copy is still the tree the user had, not a rebuild of it.
    assert_eq!(link_target(&repo, "link"), "new-target.txt");
    assert!(
        std::fs::read_to_string(repo.path().join("moved_elsewhere.txt")).is_ok(),
        "the renamed file is not on disk under its new name"
    );
}

/// Each hunkless shape has to be nameable on its own, by each kind of
/// file-level predicate -- that is the whole point of giving it a stand-in.
/// Before the fix every one of these matched nothing and was refused as an
/// empty selection.
#[cfg(unix)]
#[test]
fn a_file_level_predicate_reaches_each_hunkless_shape_on_its_own() {
    for (expr, expected) in [
        (r#"file("link")"#, "M link"),
        (r#"file("script.sh")"#, "M script.sh"),
        (r#"status("added")"#, "A empty-add.txt"),
        (r#"glob("moved_*")"#, "R {moved.txt => moved_elsewhere.txt}"),
    ] {
        let repo = hunkless_shapes_repo("hunkless-each");
        repo.hunk_ok(&["split", expr, "one shape"]);
        assert_eq!(
            repo.changed_files("@-"),
            vec![expected],
            "{expr} did not commit exactly the shape it names"
        );
        assert_eq!(
            sorted_changes(&repo, "@"),
            every_shape_but(&[expected]),
            "{expr} took something else with it"
        );
    }
}

/// Negation is the dangerous direction: `~file("text.txt")` reads as "all the
/// rest", and used to mean the binary alone. The four shapes it silently
/// dropped are exactly the ones a user asking for "everything but that file"
/// would never think to check on.
#[cfg(unix)]
#[test]
fn negation_reaches_every_hunkless_shape() {
    let repo = hunkless_shapes_repo("hunkless-negation");

    repo.hunk_ok(&["split", r#"~file("text.txt")"#, "everything but the text"]);

    assert_eq!(
        sorted_changes(&repo, "@-"),
        every_shape_but(&["text.txt"]),
        "~file() reached only some of the rest"
    );
    assert_eq!(repo.changed_files("@"), vec!["M text.txt"]);
}

/// The guard against a fix that makes the stand-ins too real. A stand-in
/// carries no text and occupies no line, so a predicate that asks about
/// content must go on missing it: there are no bytes it ever looked at, and
/// selecting on a guess is how a link or a rename would ride along with a
/// selector that never mentioned it.
///
/// Preservation guard: it passed before the stand-ins were generalised, when
/// these shapes were unreachable by *every* predicate, and so cannot have
/// failed beforehand. Its whole value is in what it forbids next.
#[cfg(unix)]
#[test]
fn content_and_line_predicates_still_cannot_reach_a_hunkless_change() {
    for expr in [
        r#"content("line two")"#,
        r#"added("line two")"#,
        "lines(1..100000)",
    ] {
        let repo = hunkless_shapes_repo("hunkless-content-guard");
        repo.hunk_ok(&["split", expr, "just the text"]);
        assert_eq!(
            repo.changed_files("@-"),
            vec!["M text.txt"],
            "{expr} reached a change whose bytes it never saw"
        );
        assert_eq!(
            sorted_changes(&repo, "@"),
            every_shape_but(&["text.txt"]),
            "{expr} moved more than the text"
        );
    }
}

/// And the text a stand-in might plausibly have been given -- the link's new
/// target, the renamed file's contents, the added file's name -- matches
/// nothing at all. Each of these is a string that exists somewhere in the
/// change; none of it is in a hunk, so none of it is matchable.
///
/// Preservation guard, like the test above: unreachable before and unreachable
/// after, so it could not have failed beforehand.
#[cfg(unix)]
#[test]
fn no_content_predicate_matches_the_text_surrounding_a_hunkless_change() {
    // One repo for all of them: an unmatched selector is refused, so nothing
    // here mutates the working copy.
    let repo = hunkless_shapes_repo("hunkless-content-none");

    for expr in [
        r#"content("new-target.txt")"#,
        r#"added("new-target.txt")"#,
        r#"removed("old-target.txt")"#,
        r#"content("content that does not move")"#,
        r#"removed("content that does not move")"#,
        r#"added("empty-add.txt")"#,
    ] {
        let err = repo.hunk_fail(&["split", expr, "should not match"]);
        assert!(
            err.contains("selection matched no hunks"),
            "{expr} matched a hunkless change: {err}"
        );
    }
    assert_eq!(
        sorted_changes(&repo, "@"),
        every_shape_but(&[]),
        "a refused selector still moved something"
    );
}

/// The same question asked with the arguments that make a content predicate
/// degenerate. The guard above is real but was answered by the stand-in's
/// *data* -- empty `added`/`removed`, both line ranges at `(0, 0)` -- and
/// emptiness is not unmatchability:
///
///   * the empty substring is inside every string, so `content("")` matched
///     every stand-in there was, making it a synonym for `all()`;
///   * line 0 is inside `0..100000`, so `lines(0..100000)` did too. The guard
///     above uses `lines(1..100000)`, which is clean for exactly the reason the
///     old comment gave -- and that is why it read as passing.
///
/// This is not a new leak. It pre-existed for binaries, the one shape that
/// already had a stand-in, and `blob.bin` in this fixture is that case;
/// generalising the stand-in multiplied it across the other four. Without the
/// fix each of these four expressions commits all six shapes, at exit 0, so the
/// assertion is on what landed.
#[cfg(unix)]
#[test]
fn a_degenerate_content_argument_cannot_reach_a_hunkless_change_either() {
    for expr in [
        r#"content("")"#,
        r#"added("")"#,
        r#"removed("")"#,
        "lines(0..100000)",
    ] {
        let repo = hunkless_shapes_repo("hunkless-degenerate");
        repo.hunk_ok(&["split", expr, "just the text"]);
        assert_eq!(
            repo.changed_files("@-"),
            vec!["M text.txt"],
            "{expr} reached a change whose bytes it never saw"
        );
        assert_eq!(
            sorted_changes(&repo, "@"),
            every_shape_but(&["text.txt"]),
            "{expr} moved more than the text"
        );
    }
}

/// The half that stops the fix from being "a content predicate matches
/// nothing". Each expression above is degenerate, not meaningless: over
/// ordinary hunks `content("")` really does select every one of them, because
/// the empty substring really is inside their text. Only the stand-ins, which
/// have no text for the question to have been asked about, drop out.
///
/// Withholding a stand-in's content is what makes that distinction expressible
/// at all. Rejecting `content("")` and `lines(0..N)` as arguments -- the other
/// way to make the test above pass -- would take these four cases with it, and
/// would still leave whatever degenerate argument turns up next.
///
/// Preservation guard: this passed before the change as well, since nothing
/// here involves a hunkless shape, so it cannot have failed beforehand. Its
/// value is entirely in what it forbids a later fix from doing.
#[test]
fn a_degenerate_content_argument_still_matches_every_ordinary_hunk() {
    for expr in [
        r#"content("")"#,
        r#"added("")"#,
        r#"removed("")"#,
        "lines(0..100000)",
    ] {
        let repo = TestRepo::new("degenerate-ordinary");
        repo.write_file("a.txt", "one\ntwo\n");
        repo.write_file("b.txt", "three\nfour\n");
        repo.jj_ok(&["commit", "-m", "base"]);
        // A replacement and a pure insertion, so that `removed("")` is asked
        // about a hunk with removed text and about one without.
        repo.write_file("a.txt", "one\nEDITED\n");
        repo.write_file("b.txt", "three\nfour\nfive\n");

        repo.hunk_ok(&["split", expr, "every ordinary hunk"]);
        assert_eq!(
            sorted_changes(&repo, "@-"),
            vec!["M a.txt", "M b.txt"],
            "{expr} stopped reaching ordinary hunks"
        );
        assert!(
            repo.changed_files("@").is_empty(),
            "{expr} left an ordinary hunk behind: {:?}",
            repo.changed_files("@")
        );
    }
}

/// Nor can `id()`. The stand-in's id is deliberately not in `hunk-<hex>` form,
/// so it is rejected as an argument rather than merely failing to match -- a
/// hunkless change cannot be selected by an identity it does not have. And
/// there is no id to copy in the first place: `list` prints none for any of
/// them.
///
/// Preservation guard: before the fix these shapes had no stand-in to be named
/// by at all, so this could not have failed. It pins the one property of the
/// stand-in that a later "make `id()` work on it" change would quietly undo.
#[cfg(unix)]
#[test]
fn id_cannot_name_a_hunkless_stand_in() {
    let repo = hunkless_shapes_repo("hunkless-id");

    let err = repo.hunk_fail(&["split", r#"id("whole-file:link")"#, "no"]);
    assert!(
        err.contains("id() does not accept"),
        "a stand-in's own id was accepted as an argument: {err}"
    );

    let listed = repo.hunk_ok(&["list", "--format", "text"]);
    assert_eq!(
        listed.matches("hunk-").count(),
        1,
        "only text.txt has an id to name; the stand-ins must print none:\n{listed}"
    );
}

/// `list --spec all()` has to show what the unfiltered listing shows. The
/// hunkless shapes were visible in `list` but vanished from `list --spec`,
/// which is the command the docs point at for checking a selector before
/// running it -- so the check agreed with the bug rather than exposing it.
#[cfg(unix)]
#[test]
fn list_spec_all_shows_every_hunkless_shape_that_plain_list_shows() {
    let repo = hunkless_shapes_repo("hunkless-list-spec");

    let plain = repo.hunk_ok(&["list", "--format", "text"]);
    let filtered = repo.hunk_ok(&["list", "--format", "text", "--spec", "all()"]);

    assert_eq!(filtered, plain, "all() hid part of the diff:\n{plain}");
    for name in [
        "blob.bin",
        "empty-add.txt",
        "link",
        "moved_elsewhere.txt",
        "script.sh",
    ] {
        assert!(
            plain.contains(name),
            "{name} missing from the listing:\n{plain}"
        );
    }
}

/// Selecting a pure rename by expression has to carry its source path, or
/// `select` looks for the "before" content under the new name, finds nothing,
/// and writes an empty file where the rename should be.
#[cfg(unix)]
#[test]
fn a_hunkset_selected_rename_carries_its_source_path() {
    let repo = hunkless_shapes_repo("hunkless-rename-from");

    let spec = repo.hunk_ok(&["list", "--spec", r#"glob("moved_*")"#, "--spec-template"]);
    assert!(
        spec.contains("moved.txt"),
        "the evaluated spec dropped the rename source:\n{spec}"
    );

    repo.hunk_ok(&["split", r#"glob("moved_*")"#, "just the rename"]);
    assert_eq!(
        repo.file_at("@-", "moved_elsewhere.txt"),
        "content that does not move\n",
        "the rename landed without its content"
    );
}

/// Preservation guard: this passed before the change too, and cannot fail
/// beforehand. Binaries were the one hunkless shape that already worked, and
/// generalising the stand-in must leave them exactly as they were -- including
/// a binary that is *also* renamed, where the whole-file action the stand-in is
/// rewritten into has to keep carrying `from`.
#[test]
fn a_renamed_binary_still_rides_along_with_all() {
    let repo = TestRepo::new("hunkless-binary-rename");
    // Large enough that jj's rename detection is in no doubt about it.
    let bytes: Vec<u8> = (0..400u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(repo.path().join("bin.dat"), &bytes).unwrap();
    repo.write_file("t.txt", "a\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    std::fs::rename(repo.path().join("bin.dat"), repo.path().join("moved.dat")).unwrap();
    repo.write_file("t.txt", "a\nb\n");
    assert!(
        repo.changed_files("@")
            .iter()
            .any(|l| l.starts_with('R') && l.contains("moved.dat")),
        "precondition: jj must call this a rename: {:?}",
        repo.changed_files("@")
    );

    repo.hunk_ok(&["split", "all()", "everything"]);

    assert_eq!(
        sorted_changes(&repo, "@-"),
        vec!["M t.txt", "R {bin.dat => moved.dat}"]
    );
    assert!(repo.changed_files("@").is_empty());
    let shown = repo.jj(&["file", "show", "-r", "@-", "file:moved.dat"]);
    assert_eq!(
        shown.stdout, bytes,
        "the renamed binary did not land byte for byte"
    );
}

/// Preservation guard: `--spec-template` reaches these shapes through
/// `build_spec_template`, a path this change does not touch, and it must go on
/// naming every one of them. It cannot fail before the fix -- it is here so
/// that the two ways of covering a whole diff are checked against the same
/// fixture, and a later narrowing of either is caught.
#[cfg(unix)]
#[test]
fn the_spec_template_still_names_every_hunkless_shape() {
    let repo = hunkless_shapes_repo("hunkless-template");

    let template = repo.hunk_ok(&["list", "--spec-template"]);
    for name in [
        "blob.bin",
        "empty-add.txt",
        "link",
        "moved_elsewhere.txt",
        "script.sh",
        "text.txt",
    ] {
        assert!(
            template.contains(name),
            "--spec-template dropped {name}:\n{template}"
        );
    }
    assert!(
        template.contains("moved.txt"),
        "--spec-template dropped the rename source:\n{template}"
    );

    repo.hunk_ok(&["split", &template, "everything the template names"]);
    assert!(
        repo.changed_files("@").is_empty(),
        "the template left something behind: {:?}",
        repo.changed_files("@")
    );
}

// ---------------------------------------------------------------------------
// the `select` child is this binary
// ---------------------------------------------------------------------------

/// The ids in a spec are hashes computed by *this* build over *this* diff, so
/// the process that computed them has to be the process that applies them.
///
/// A user who follows the docs and persists `[merge-tools.jj-hunk] program =
/// "jj-hunk"` gets back whatever PATH resolves -- an upstream `cargo install`ed
/// copy, say, which hashes ids differently. The fork's own guard passes (the
/// ids are real in its view), the child resolves none of them, and jj commits
/// the empty selection at exit 0 with every edit still in the working copy. The
/// decoy below stands in for that child.
#[test]
fn split_uses_the_running_binary_not_the_program_named_in_the_config() {
    let repo = TestRepo::new("split-pins-current-exe");
    repo.write_file("f.txt", "a\nb\nc\nd\ne\nf\ng\nh\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("f.txt", "A\nb\nc\nd\ne\nf\ng\nH\n");

    let decoy = repo.path().join("decoy-jj-hunk.sh");
    let marker = repo.path().join("decoy-was-run");
    std::fs::write(
        &decoy,
        format!("#!/bin/sh\necho ran >> {:?}\nexit 0\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        &repo.config_path,
        format!(
            "[merge-tools.jj-hunk]\nprogram = {:?}\nedit-args = [\"select\", \"$left\", \"$right\"]\n",
            decoy,
        ),
    )
    .unwrap();

    let out = repo.hunk(&["split", "lines(1)", "first"]);
    assert!(
        !marker.exists(),
        "the configured program ran instead of the binary that computed the ids"
    );
    assert!(
        out.status.success(),
        "split failed: {}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );

    assert_eq!(
        repo.file_at("@-", "f.txt"),
        "A\nb\nc\nd\ne\nf\ng\nh\n",
        "only the first hunk should have been split off"
    );
    assert_eq!(repo.file_at("@", "f.txt"), "A\nb\nc\nd\ne\nf\ng\nH\n");
}

/// `edit-args` is pinned for the same reason and not merely for tidiness. A
/// stale pair with `$left` and `$right` the wrong way round would hand `select`
/// the two directories reversed, which inverts the entire selection without
/// failing: the "kept" hunks would be the ones the user asked to leave behind.
#[test]
fn commit_ignores_reversed_edit_args_in_the_config() {
    let repo = TestRepo::new("commit-pins-edit-args");
    repo.write_file("f.txt", "a\nb\nc\nd\ne\nf\ng\nh\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("f.txt", "A\nb\nc\nd\ne\nf\ng\nH\n");

    std::fs::write(
        &repo.config_path,
        format!(
            "[merge-tools.jj-hunk]\nprogram = {:?}\nedit-args = [\"select\", \"$right\", \"$left\"]\n",
            jj_hunk_bin(),
        ),
    )
    .unwrap();

    repo.hunk_ok(&["commit", "lines(1)", "just the first hunk"]);

    assert_eq!(
        repo.file_at("@-", "f.txt"),
        "A\nb\nc\nd\ne\nf\ng\nh\n",
        "the selection came out inverted"
    );
    assert_eq!(repo.file_at("@", "f.txt"), "A\nb\nc\nd\ne\nf\ng\nH\n");
}

// ---------------------------------------------------------------------------
// absorb and renames
// ---------------------------------------------------------------------------

/// Absorb moves lines; a rename is a whole-file change no hunk selection can
/// express. `select` performs it as part of whichever spec names the file, so
/// it rode into the ancestor with the *first* squash -- putting the new name in
/// a commit every later one still refers to by the old one. Absorb reported "2
/// moving into 2 ancestors, 0 staying", conflicted that first destination on
/// the rebase, and gave up with "half-moved / moved: nothing".
#[test]
fn absorb_leaves_a_renamed_file_alone_instead_of_carrying_the_rename() {
    let repo = absorb_stack("absorb-rename-two-targets");
    std::fs::rename(repo.path().join("f.txt"), repo.path().join("g.txt")).unwrap();
    // One line B owns and one line C owns: two destinations, which a rename
    // cannot be split across at all.
    repo.write_file("g.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH2\ni\nj\n");

    let before = repo.jj_ok(&["diff", "--summary"]);
    assert!(before.contains("g.txt"), "expected a rename: {before}");

    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(
        plan.contains("2 hunks: 0 moving into 0 ancestors, 2 staying"),
        "{plan}"
    );
    assert!(plan.contains("g.txt was renamed from f.txt"), "{plan}");

    let out = repo.hunk_ok(&["absorb"]);
    assert!(out.contains("Nothing to absorb"), "{out}");

    assert_eq!(
        repo.jj_ok(&["diff", "--summary"]).trim(),
        before.trim(),
        "absorb changed the working copy after saying nothing would move"
    );
    assert_eq!(conflicted_changes(&repo), Vec::<String>::new());
}

/// Routing to a single ancestor is not the safe case it looks like: the rename
/// still rides along, into a commit every later one names the old way.
#[test]
fn absorb_leaves_a_renamed_file_alone_even_with_one_destination() {
    let repo = absorb_stack("absorb-rename-one-target");
    std::fs::rename(repo.path().join("f.txt"), repo.path().join("g.txt")).unwrap();
    repo.write_file("g.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");

    let before = repo.jj_ok(&["diff", "--summary"]);
    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(
        plan.contains("1 hunks: 0 moving into 0 ancestors, 1 staying"),
        "{plan}"
    );
    assert!(plan.contains("g.txt was renamed from f.txt"), "{plan}");

    repo.hunk_ok(&["absorb"]);
    assert_eq!(repo.jj_ok(&["diff", "--summary"]).trim(), before.trim());
    assert_eq!(conflicted_changes(&repo), Vec::<String>::new());
}

/// A renamed file alongside one that does route. This is where the rename used
/// to do its damage most quietly: the squash is driven by the *other* file, and
/// the rename attached itself to it on the way through, so nothing in the plan
/// ever mentioned the file it moved.
#[test]
fn absorb_does_not_let_a_rename_ride_on_another_files_squash() {
    let repo = absorb_stack("absorb-rename-alongside");
    repo.write_file("other.txt", "p\nq\nr\n");
    repo.jj_ok(&["commit", "-m", "D: add other"]);

    std::fs::rename(repo.path().join("f.txt"), repo.path().join("g.txt")).unwrap();
    repo.write_file("g.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH\ni\nj\n");
    repo.write_file("other.txt", "p\nQ\nr\n");

    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(
        plan.contains("2 hunks: 1 moving into 1 ancestor, 1 staying"),
        "{plan}"
    );

    repo.hunk_ok(&["absorb"]);

    // The one routable hunk moved; the rename did not go with it.
    assert_eq!(
        repo.jj_ok(&["diff", "--summary"]).trim(),
        "R {f.txt => g.txt}",
        "the rename left the working copy"
    );
    assert_eq!(
        repo.jj_ok(&["diff", "--summary", "-r", "description(substring:\"D: \")"]).trim(),
        "A other.txt",
        "the rename was carried into an ancestor"
    );
    assert_eq!(conflicted_changes(&repo), Vec::<String>::new());
}

/// The very same edits without the rename still absorb: the refusal above is
/// about the rename, not about the file.
#[test]
fn absorb_still_routes_a_file_that_was_not_renamed() {
    let repo = absorb_stack("absorb-rename-control");
    repo.write_file("f.txt", "a\nb\nCCC2\nd\ne\nf\ng\nHHH2\ni\nj\n");

    let plan = repo.hunk_ok(&["absorb", "--dry-run"]);
    assert!(
        plan.contains("2 hunks: 2 moving into 2 ancestors, 0 staying"),
        "{plan}"
    );

    repo.hunk_ok(&["absorb"]);
    assert!(repo.jj_ok(&["diff", "--summary"]).trim().is_empty());
    assert_eq!(conflicted_changes(&repo), Vec::<String>::new());
}

/// Change ids of every conflicted commit at or below `@`.
fn conflicted_changes(repo: &TestRepo) -> Vec<String> {
    repo.jj_ok(&[
        "log",
        "--no-graph",
        "-r",
        "::@",
        "-T",
        r#"if(conflict, change_id.short(8) ++ "\n", "")"#,
    ])
    .lines()
    .map(str::trim)
    .filter(|l| !l.is_empty())
    .map(str::to_string)
    .collect()
}

// ---------------------------------------------------------------------------
// Selectors that used to fail *open*: they matched nothing, and `~` turned
// that into the whole diff -- so the one clause holding a change back was the
// clause that had silently stopped working.
// ---------------------------------------------------------------------------

/// A working copy with a change the user wants and a vendored change they do
/// not, which is the shape every selector below is written to separate.
fn vendored_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("src.txt", "src line\n");
    repo.write_file("vendor/lib.txt", "vendor line\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("src.txt", "src line\nsrc change\n");
    repo.write_file("vendor/lib.txt", "vendor line\nvendor change\n");
    repo
}

/// The reported crash: an unterminated character class matched nothing, `~`
/// inverted that into everything, and `split` committed the vendored change
/// the selector existed to exclude -- at exit 0, with no diagnostic.
#[test]
fn a_malformed_glob_stops_the_split_instead_of_committing_everything() {
    let repo = vendored_repo("glob-malformed-split");
    let err = repo.hunk_fail(&["split", r#"~glob("vendor/[a-z*.txt")"#, "malformed"]);
    assert!(
        err.contains("vendor/[a-z*.txt"),
        "the error must quote the pattern: {err}"
    );
    assert!(
        err.contains("unclosed '['"),
        "the error must say what is wrong with it: {err}"
    );
    // Nothing was committed, so both changes are still in the working copy.
    let changed = repo.changed_files("@");
    assert_eq!(changed.len(), 2, "the split must not have run: {changed:?}");
}

/// Not only under `~`. A malformed glob is a typo wherever it appears, and
/// `list` exiting 0 with an empty result is how the typo goes unnoticed.
#[test]
fn a_malformed_glob_is_an_error_in_list_too() {
    let repo = vendored_repo("glob-malformed-list");
    for spec in [
        r#"glob("vendor/[a-z*.txt")"#,
        r#"glob("vendor/{a,b")"#,
        r#"~glob("vendor/[a-z*.txt")"#,
        r#"file(glob:"vendor/[a-")"#,
    ] {
        let err = repo.hunk_fail(&["list", "--spec", spec, "--format", "text"]);
        assert!(
            err.contains("invalid glob"),
            "{spec} should be rejected: {err}"
        );
    }
    // The well-formed pattern the user meant still works.
    let out = repo.hunk_ok(&["list", "--spec", r#"~glob("vendor/**")"#, "--format", "text"]);
    assert!(out.contains("src.txt"), "{out}");
    assert!(!out.contains("vendor/lib.txt"), "{out}");
}

/// `--include`/`--exclude` reach the glob matcher by a different route than
/// the `glob()` predicate, and had the same hole. A dropped `--exclude`
/// excludes nothing, and `list --spec-template` bakes that listing into a spec
/// that then drives `split`.
#[test]
fn a_malformed_exclude_pattern_stops_the_listing() {
    let repo = vendored_repo("glob-malformed-exclude");

    let err = repo.hunk_fail(&["list", "--exclude", "vendor/[a-z*.txt", "--format", "text"]);
    assert!(err.contains("vendor/[a-z*.txt"), "{err}");
    assert!(err.contains("unclosed '['"), "{err}");

    let err = repo.hunk_fail(&["list", "--include", "src/{a,b", "--format", "text"]);
    assert!(err.contains("unclosed '{'"), "{err}");

    // The template a split would be built from is refused for the same reason,
    // rather than being generated from an unfiltered listing.
    let err = repo.hunk_fail(&["list", "--spec-template", "--exclude", "vendor/[a-"]);
    assert!(err.contains("invalid glob"), "{err}");

    // Well-formed patterns are unaffected.
    let out = repo.hunk_ok(&["list", "--exclude", "vendor/**", "--format", "text"]);
    assert!(out.contains("src.txt"), "{out}");
    assert!(!out.contains("vendor/lib.txt"), "{out}");
}

/// The abbreviation `list` prints is a name for the hunk. Quoting it cannot
/// change what it means -- unquoted, the parser's inferred `Exact` kind was
/// read as a demand for whole-id equality, so it matched nothing.
#[test]
fn an_unquoted_abbreviated_id_selects_the_same_hunk_as_a_quoted_one() {
    let repo = vendored_repo("id-unquoted");
    let listing = repo.hunk_ok(&["list", "--format", "text"]);
    let short = listing
        .split_whitespace()
        .find(|word| word.starts_with("hunk-"))
        .expect("list should print a short id")
        .to_string();
    // Shorter than the printed abbreviation, so only prefix resolution finds it.
    let abbreviated = &short[.."hunk-".len() + 4];

    let quoted = repo.hunk_ok(&[
        "list",
        "--spec",
        &format!(r#"id("{abbreviated}")"#),
        "--format",
        "text",
    ]);
    let bare = repo.hunk_ok(&["list", "--spec", &format!("id({abbreviated})"), "--format", "text"]);
    assert!(quoted.contains(&short), "the quoted form should resolve: {quoted}");
    assert_eq!(
        bare, quoted,
        "an unquoted abbreviation must select what the quoted one does"
    );

    // And the negation excludes exactly that hunk rather than nothing.
    let negated =
        repo.hunk_ok(&["list", "--spec", &format!("~id({abbreviated})"), "--format", "text"]);
    assert!(!negated.contains(&short), "{negated}");
    assert!(negated.contains("hunk-"), "the other hunk should remain: {negated}");
}

/// An id naming no hunk is a stale or mistyped name, not an empty answer.
/// Silently, `~id("hunk-...")` was the entire diff.
#[test]
fn an_id_that_names_no_hunk_is_refused() {
    let repo = vendored_repo("id-unknown");
    for spec in [
        r#"id("hunk-ffffffffff")"#,
        "id(hunk-ffffffffff)",
        r#"~id("hunk-ffffffffff")"#,
    ] {
        let err = repo.hunk_fail(&["list", "--spec", spec, "--format", "text"]);
        assert!(
            err.contains("matches no hunk"),
            "{spec} should be refused: {err}"
        );
    }
}

/// A chain of operators is built in a loop, so the parser's nesting limit
/// never sees it -- but evaluating it, and freeing it, walked its left spine
/// recursively and aborted the process with a stack overflow. `--spec-file`
/// takes an expression far longer than any argv limit.
#[test]
fn a_very_long_operator_chain_does_not_crash() {
    let repo = vendored_repo("chain-long");
    for (name, joiner) in [("union", " | "), ("intersection", " & ")] {
        let file = format!("chain-{name}.txt");
        repo.write_file(&file, &vec!["all()"; 100_000].join(joiner));
        let path = repo.path().join(&file);
        let out = repo.hunk(&[
            "list",
            "--spec-file",
            path.to_str().unwrap(),
            "--format",
            "text",
        ]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("stack overflow"),
            "a {name} chain crashed the process: {stderr}"
        );
        assert!(out.status.success(), "{name} chain failed: {stderr}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("src.txt"),
            "{name} chain selected nothing"
        );
    }
}

/// `--spec-template` serialises its file map, and a `HashMap` there let the key
/// order change on every run -- 10 distinct orders in 10 runs on an unchanged
/// working copy. That made two templates of the same state compare unequal, so
/// a checked-in template produced a spurious diff and `diff`ing two templates
/// said nothing. The order must be stable, and it must agree with `list`, which
/// has always been path-sorted.
///
/// The assertion reads the *raw text*, not a parsed object: `serde_json::Map`
/// is a `BTreeMap` by default, so parsing sorts the keys and would pass no
/// matter what order the binary actually wrote.
#[test]
fn spec_template_file_order_is_deterministic_and_sorted() {
    const NAMES: &[&str] = &[
        "zeta.txt",
        "alpha.txt",
        "mike.txt",
        "bravo.txt",
        "yank.txt",
        "delta.txt",
        "charlie.txt",
        "oscar.txt",
    ];
    let repo = metachar_repo("spec-template-order", NAMES);

    let textual_order = |template: &str| -> Vec<String> {
        let mut found: Vec<(usize, String)> = NAMES
            .iter()
            .map(|n| {
                let at = template
                    .find(&format!("\"{n}\""))
                    .unwrap_or_else(|| panic!("{n} missing from template:\n{template}"));
                (at, (*n).to_string())
            })
            .collect();
        found.sort_by_key(|(at, _)| *at);
        found.into_iter().map(|(_, n)| n).collect()
    };

    let first = textual_order(&repo.hunk_ok(&["list", "--spec-template"]));

    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(first, sorted, "template file keys must be path-sorted");

    for run in 1..8 {
        let again = textual_order(&repo.hunk_ok(&["list", "--spec-template"]));
        assert_eq!(first, again, "template key order changed on run {run}");
    }
}

// ---------------------------------------------------------------------------
// A renamed file has two names, and a path predicate has to answer to both.
//
// `--include`/`--exclude` filter through `FileHunks::all_paths()`, which is the
// primary path *plus* the rename source, so `--include 'secret/*'` finds a file
// that has since been renamed out of `secret/`. The hunkset predicates saw only
// `EnrichedHunk::file_path` -- the new path -- so `glob("secret/*")` came back
// empty on the same diff. Two ways of asking the same question, two answers.
//
// The direction the disagreement failed in is the bad one: the old path is the
// one you type when you are looking for what *used to be* somewhere, and the
// rename is exactly the case where you cannot know the new name to type
// instead. `~glob("secret/*")` had the mirror problem -- it claimed to exclude
// the secret directory while keeping a hunk whose diff still spells
// `secret/keys.txt` on its left side.
// ---------------------------------------------------------------------------

/// A repo that renames `secret/keys.txt` to `exposed.txt` and edits one line of
/// it, alongside an unrelated edit to `public.txt`.
///
/// Forty lines with a single changed one, because jj's rename detection is a
/// similarity threshold: a short file with a proportionally large edit degrades
/// into separate `D` + `A` entries, which take completely different code paths
/// and would leave these tests quietly asserting nothing. The `R {...}` check
/// below is a precondition, not an assertion about the fix -- if a jj upgrade
/// moves the threshold, every test built on this helper fails loudly instead of
/// turning into a test of add-plus-delete.
fn renamed_out_of_dir_repo(name: &str) -> TestRepo {
    let base: String = (1..=40).map(|i| format!("secret line {i}\n")).collect();
    let edited = base.replace("secret line 7\n", "secret line 7 EDITED\n");
    let public: String = (1..=40).map(|i| format!("public line {i}\n")).collect();
    let public_edited = public.replace("public line 3\n", "public line 3 EDITED\n");

    let repo = TestRepo::new(name);
    repo.write_file("secret/keys.txt", &base);
    repo.write_file("public.txt", &public);
    repo.jj_ok(&["commit", "-m", "base"]);
    std::fs::remove_file(repo.path().join("secret/keys.txt")).unwrap();
    repo.write_file("exposed.txt", &edited);
    repo.write_file("public.txt", &public_edited);

    let summary = repo.changed_files("@");
    assert!(
        reports_rename(&summary, "secret/keys.txt", "exposed.txt"),
        "jj reported no rename, so this fixture tests something else entirely: {summary:?}"
    );

    repo
}

/// Whether a `jj diff --summary` listing reports `from` renamed to `to`.
///
/// `R `, not just "both names appear": a degraded rename shows up as separate
/// `D` and `A` lines that also mention both paths, which is the exact shape
/// these tests must not silently become.
fn reports_rename(summary: &[String], from: &str, to: &str) -> bool {
    summary
        .iter()
        .any(|l| l.starts_with("R ") && l.contains(from) && l.contains(to))
}

/// Paths `list` reports, in the order it reports them.
fn listed_paths(repo: &TestRepo, args: &[&str]) -> Vec<String> {
    let out = repo.hunk_ok(args);
    let json: serde_json::Value = serde_json::from_str(&out).expect("list did not emit JSON");
    json["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .map(|f| f["path"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_hunkset_path_predicate_finds_a_renamed_file_by_its_old_path() {
    // The gap this whole change closes. `--include 'secret/*'` finds the file;
    // `glob("secret/*")` used to come back empty and `file("secret/keys.txt")`
    // with it, so the hunkset language could not name a change that the flag
    // filtering right next to it could.
    let repo = renamed_out_of_dir_repo("rename-oldpath-glob");

    let by_flag = listed_paths(&repo, &["list", "--include", "secret/*"]);
    assert_eq!(
        by_flag,
        vec!["exposed.txt"],
        "--include lost its old-path match"
    );

    let by_glob = listed_paths(&repo, &["list", "--spec", r#"glob("secret/*")"#]);
    assert_eq!(
        by_glob, by_flag,
        "glob() disagreed with --include about a renamed file"
    );

    let by_file = listed_paths(&repo, &["list", "--spec", r#"file("secret/keys.txt")"#]);
    assert_eq!(
        by_file, by_flag,
        "file() could not name a renamed file by the path it was renamed from"
    );
}

#[test]
fn a_hunkset_path_predicate_still_finds_a_renamed_file_by_its_new_path() {
    // A preservation guard: this passed before the old-path lookup was added
    // and cannot be made to fail by reverting it, so it proves nothing about
    // the fix. It is here for the other direction -- "match either path" is one
    // `||` away from "match the old path *instead of* the new one", which would
    // break every ordinary use of `file()` on a renamed file while leaving the
    // test above green.
    let repo = renamed_out_of_dir_repo("rename-newpath-glob");

    assert_eq!(
        listed_paths(&repo, &["list", "--spec", r#"glob("exposed*")"#]),
        vec!["exposed.txt"],
    );
    assert_eq!(
        listed_paths(&repo, &["list", "--spec", r#"file("exposed.txt")"#]),
        vec!["exposed.txt"],
    );
}

#[test]
fn negating_a_path_predicate_excludes_a_file_renamed_out_of_that_path() {
    // The consequence of matching either path, and the one worth stating out
    // loud because it *removes* a file from a selection that used to contain
    // it. It is the safe direction: the diff for this change still spells
    // `secret/keys.txt` on its left side, so a user who asked for "everything
    // except secret/" and got the rename anyway was handed the thing they
    // excluded. `--exclude` had always read it this way; `~glob()` now agrees.
    let repo = renamed_out_of_dir_repo("rename-oldpath-negation");

    let by_flag = listed_paths(&repo, &["list", "--exclude", "secret/*"]);
    assert_eq!(
        by_flag,
        vec!["public.txt"],
        "--exclude lost its old-path match"
    );

    let by_negation = listed_paths(&repo, &["list", "--spec", r#"~glob("secret/*")"#]);
    assert_eq!(
        by_negation, by_flag,
        "~glob() disagreed with --exclude about a renamed file"
    );
}

#[test]
fn a_rename_matches_the_extension_on_either_side_of_the_rename() {
    // A deliberate choice, not a side effect. `a.txt` renamed to `b.rs` is a
    // change that removed a .txt file and created a .rs one, and `extension()`
    // is `glob("*.<ext>")` with the globbing spelled out -- so if `glob` reads
    // both paths and `extension` reads one, the language contradicts itself
    // rather than the flags. Both must match, and the cost is understood:
    // `extension(txt)` reaches a file that is no longer a .txt file.
    let base: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    let edited = base.replace("line 7\n", "line 7 EDITED\n");

    let repo = TestRepo::new("rename-extension-change");
    repo.write_file("mod.txt", &base);
    repo.jj_ok(&["commit", "-m", "base"]);
    std::fs::remove_file(repo.path().join("mod.txt")).unwrap();
    repo.write_file("mod.rs", &edited);

    let summary = repo.changed_files("@");
    assert!(
        reports_rename(&summary, "mod.txt", "mod.rs"),
        "jj reported no rename, so this fixture tests something else entirely: {summary:?}"
    );

    assert_eq!(
        listed_paths(&repo, &["list", "--spec", r#"extension("rs")"#]),
        vec!["mod.rs"],
        "extension() lost the side of the rename the file now has"
    );
    assert_eq!(
        listed_paths(&repo, &["list", "--spec", r#"extension("txt")"#]),
        vec!["mod.rs"],
        "extension() did not reach the side of the rename the file came from"
    );
}

#[test]
fn selecting_a_rename_by_its_old_path_commits_it_under_the_new_one() {
    // Matching by the old path must not leak the old path into the spec. The
    // spec key is what `select` resolves a file by, and the rename source rides
    // along in `from:` -- swap the two and `select` looks under the old name on
    // the right-hand side, finds nothing, and the rename is undone by
    // `default: reset` instead of committed. So this asserts the whole round
    // trip: named by `secret/keys.txt`, keyed by `exposed.txt`, committed as a
    // rename with the edit intact and the unselected file left behind.
    //
    // (`to_spec` keying by the new path is asserted directly in the unit test
    // `to_spec_keys_a_rename_by_its_new_path_when_matched_by_the_old_one`; this
    // is the end-to-end half, which is what actually notices if `select` cannot
    // resolve the key.)
    let repo = renamed_out_of_dir_repo("rename-oldpath-commit");
    let expected = repo.file_at("@", "exposed.txt");

    let template = repo.hunk_ok(&["list", "--spec", r#"glob("secret/*")"#, "--spec-template"]);
    let parsed: serde_json::Value = serde_json::from_str(&template).expect("spec is not JSON");
    let keys: Vec<&str> = parsed["files"]
        .as_object()
        .expect("spec has no files")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["exposed.txt"],
        "the spec was keyed by the path the file was renamed from, which `select` cannot resolve"
    );
    assert_eq!(
        parsed["files"]["exposed.txt"]["from"], "secret/keys.txt",
        "the spec dropped the rename source"
    );

    repo.hunk_ok(&["commit", r#"glob("secret/*")"#, "keep the rename"]);

    let committed = repo.changed_files("@-");
    assert!(
        reports_rename(&committed, "secret/keys.txt", "exposed.txt"),
        "the commit did not carry the rename: {committed:?}"
    );
    assert_eq!(
        repo.file_at("@-", "exposed.txt"),
        expected,
        "the renamed file was committed with the wrong content"
    );
    assert!(
        !committed.iter().any(|l| l.contains("public.txt")),
        "an unselected file rode along with the rename: {committed:?}"
    );
}

#[test]
fn a_copy_is_matched_by_the_path_it_was_copied_from() {
    // Copies take the same route as renames -- `FileHunks::rename_source` is
    // filled in for both -- so the gap was the same and so is the fix. Worth
    // testing on its own because it is *not* untestable here: jj 0.44 on the
    // git backend does detect a copy, as long as the source is also modified
    // (git only considers modified files as copy sources). A copy whose source
    // is untouched degrades to a plain `A`, which is a different shape.
    //
    // The answer has two entries on purpose: `orig.txt` matches by its own path
    // and `copy.txt` by the path it was copied from, which is what "changes
    // involving orig.txt" should mean -- and exactly what `--include` returns.
    let base: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    let repo = TestRepo::new("copy-source-path");
    repo.write_file("orig.txt", &base);
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("copy.txt", &base);
    repo.write_file("orig.txt", &base.replace("line 3\n", "line 3 EDITED\n"));

    let summary = repo.changed_files("@");
    assert!(
        summary
            .iter()
            .any(|l| l.starts_with("C ") && l.contains("orig.txt") && l.contains("copy.txt")),
        "jj reported no copy, so this fixture tests something else entirely: {summary:?}"
    );

    let by_flag = listed_paths(&repo, &["list", "--include", "orig.txt"]);
    assert_eq!(by_flag, vec!["copy.txt", "orig.txt"]);
    assert_eq!(
        listed_paths(&repo, &["list", "--spec", r#"file("orig.txt")"#]),
        by_flag,
        "file() did not reach the copy made from orig.txt"
    );
}

/// `--version` has to work on the installed binary, and has to report the
/// version cargo actually built.
///
/// It did not exist at all until late: clap only derives `--version` when the
/// `version` attribute is present, so the flag errored with "unexpected
/// argument". That matters more for a fork than it would upstream. The crate
/// version carries a fork suffix (`0.4.1-my-jj-hunk`), and once the binary is
/// installed onto a PATH there is otherwise no way to tell a fork build from
/// upstream's — which is exactly when you want to ask.
///
/// Asserting against `CARGO_PKG_VERSION` rather than a literal means the test
/// cannot drift out of step with a version bump.
#[test]
fn version_flag_reports_the_built_version() {
    for flag in ["--version", "-V"] {
        let out = std::process::Command::new(jj_hunk_bin())
            .arg(flag)
            .output()
            .expect("failed to run jj-hunk");
        assert!(
            out.status.success(),
            "{flag} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "{flag} did not report {}: {stdout:?}",
            env!("CARGO_PKG_VERSION")
        );
    }
}

// ---------------------------------------------------------------------------
// The shape of a spec key.
//
// A spec key is a path an agent wrote, and an agent writes paths that were
// never in any diff. Every consumer resolves one by looking it up in a set of
// paths that came off the diff, so a key naming `/etc/passwd` has always been
// inert -- but inert by good luck of the lookup, not by refusal, and the two
// stop being the same thing the moment a lookup joins a key onto a directory
// instead of comparing it against a set.
//
// Most of what follows is a regression test in the plain sense: before the
// check these tests were written for, `split` took every one of these specs and
// exited 0. Two of them are not, and are worth telling apart --
// `a_pattern_reaching_outside_the_workspace_matches_nothing_rather_than_failing`
// and the first half of `a_climb_to_the_workspace_root_is_kept_...` record
// behaviour that was already right, so they cannot fail against the commit that
// added them. Their job is to fail against a later one that widens the refusal
// into a ban on `..`, or narrows it onto patterns, and so breaks a working
// feature in the name of hardening.
// ---------------------------------------------------------------------------

/// The traversal in every spec shape that carries one.
///
/// The shapes matter because the older check that catches some of these --
/// `validate_spec_resolves` -- deliberately holds its tongue for an entry that
/// keeps nothing, and for an absent path in a spec that keeps something real
/// elsewhere. Both silences are right for a path that is merely not in *this*
/// diff and wrong for a path that could not be in any diff, so a key of this
/// shape used to be accepted at exit 0 in four of the five spellings below.
#[test]
fn a_spec_key_that_climbs_out_of_the_workspace_is_refused_in_every_spec_shape() {
    let repo = TestRepo::new("key-escape-shapes");
    repo.write_file("top.txt", "t1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("top.txt", "t1\nt2\n");

    let escaping = "../../.ssh/id_rsa";
    for entry in [
        // Never reported before: the entry keeps nothing, so the reference
        // check had nothing to say about where it pointed.
        r#"{"action": "reset"}"#,
        r#"{"ids": []}"#,
        r#"{"hunks": []}"#,
        // Reported before only because no other entry kept anything real.
        r#"{"action": "keep"}"#,
    ] {
        let spec = format!(
            r#"{{"files": {{"{escaping}": {entry}, "top.txt": {{"action": "keep"}}}}, "default": "keep"}}"#
        );
        let err = repo.hunk_fail(&["split", &spec, "escaped"]);
        assert!(
            err.contains("climbs above the workspace root"),
            "{entry} was not refused for climbing out: {err}"
        );
    }

    // And with nothing else in the spec at all.
    let alone = format!(r#"{{"files": {{"{escaping}": {{"action": "keep"}}}}, "default": "reset"}}"#);
    let err = repo.hunk_fail(&["split", &alone, "escaped"]);
    assert!(err.contains("climbs above the workspace root"), "{err}");

    assert!(
        !repo.log_descriptions().iter().any(|d| d == "escaped"),
        "a commit was made from a spec that named a path outside the workspace"
    );
}

/// The discriminating pair, from one directory, in one repo: `..` is ordinary
/// here and jj really does print it, so the test cannot be "does the key
/// contain `..`". It is where the climb lands.
///
/// Banning `..` outright would pass the second half of this test and fail the
/// first, which is the whole reason the check counts components against the
/// depth of the cwd instead.
#[test]
fn a_climb_to_the_workspace_root_is_kept_and_a_climb_past_it_is_refused() {
    let repo = TestRepo::new("key-escape-boundary");
    repo.write_file("top.txt", "t1\n");
    repo.write_file("sub/mid.txt", "m1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("top.txt", "t1\nt2\n");
    repo.write_file("sub/mid.txt", "m1\nm2\n");

    // Exactly to the root: this is how jj spells `top.txt` from `sub/`, and it
    // has to keep selecting that file.
    let reaching = r#"{"files": {"../top.txt": {"action": "keep"}}, "default": "reset"}"#;
    hunk_in_ok(&repo, "sub", &["split", reaching, "reached the root"]);
    assert_eq!(
        repo.changed_files("@-"),
        vec!["M top.txt".to_string()],
        "the legitimate climb stopped selecting the file above the cwd"
    );

    // One component further, from the same directory, is outside the workspace.
    let escaping = r#"{"files": {"../../top.txt": {"action": "keep"}}, "default": "reset"}"#;
    let err = hunk_in_fail(&repo, "sub", &["split", escaping, "climbed past"]);
    assert!(
        err.contains("climbs above the workspace root"),
        "a climb past the root was accepted: {err}"
    );
}

/// jj prints relative paths, so an absolute key came from somewhere that was
/// not a diff. `/etc/passwd` is the one an agent writes.
#[test]
fn an_absolute_spec_key_is_refused() {
    let repo = TestRepo::new("key-absolute");
    repo.write_file("top.txt", "t1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("top.txt", "t1\nt2\n");

    let spec = r#"{"files": {"/etc/passwd": {"action": "reset"}, "top.txt": {"action": "keep"}}, "default": "keep"}"#;
    let err = repo.hunk_fail(&["split", spec, "absolute"]);
    assert!(err.contains("is an absolute path"), "{err}");
}

/// A key that names a real file by a path jj would never print for it. The
/// file is right there, so nothing about this is *unresolvable* -- it is
/// refused because one file answering to two keys is how one entry starts
/// shadowing another.
#[test]
fn a_dotdot_after_a_named_directory_is_refused_even_though_it_names_a_real_file() {
    let repo = TestRepo::new("key-interior-dotdot");
    repo.write_file("top.txt", "t1\n");
    repo.write_file("sub/mid.txt", "m1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("top.txt", "t1\nt2\n");
    repo.write_file("sub/mid.txt", "m1\nm2\n");

    let spec = r#"{"files": {"sub/../top.txt": {"action": "keep"}}, "default": "reset"}"#;
    let err = repo.hunk_fail(&["split", spec, "interior"]);
    assert!(
        err.contains("has a `..` after"),
        "an un-normalised key naming a real file was accepted: {err}"
    );
}

/// No path jj can print holds a C0 control character. Refusing the key also
/// keeps it from reaching a terminal through the message that reports it,
/// which is why the reported spelling is escaped rather than echoed.
#[test]
fn a_control_character_in_a_spec_key_is_refused_and_not_echoed_raw() {
    let repo = TestRepo::new("key-control-chars");
    repo.write_file("top.txt", "t1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("top.txt", "t1\nt2\n");

    for (escape, code) in [
        (r"\u0000", "U+0000"),
        (r"\u001b", "U+001B"),
        (r"\n", "U+000A"),
    ] {
        let spec = format!(
            r#"{{"files": {{"top{escape}.txt": {{"action": "keep"}}, "top.txt": {{"action": "keep"}}}}, "default": "keep"}}"#
        );
        let err = repo.hunk_fail(&["split", &spec, "control"]);
        assert!(
            err.contains(&format!("contains the control character {code}")),
            "a {code} in a spec key was accepted: {err:?}"
        );
    }

    let spec = r#"{"files": {"top\u001b[31m.txt": {"action": "keep"}}, "default": "reset"}"#;
    let err = repo.hunk_fail(&["split", spec, "control"]);
    assert!(
        !err.contains('\u{1b}'),
        "the refusal echoed the escape byte it was refusing: {err:?}"
    );
}

/// A rename's `from` is a path written by the same hand as the key, and it is
/// the one path in a spec that no other check looks at: `validate_spec_resolves`
/// walks keys, ids and indices and never reads it.
#[test]
fn a_rename_source_that_climbs_out_of_the_workspace_is_refused() {
    let repo = TestRepo::new("from-escape");
    repo.write_file("top.txt", "t1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("top.txt", "t1\nt2\n");

    let spec = r#"{"files": {"top.txt": {"action": "keep", "from": "../../../../etc/passwd"}}, "default": "reset"}"#;
    let err = repo.hunk_fail(&["split", spec, "from-escaped"]);
    assert!(
        err.contains("its `from`") && err.contains("climbs above the workspace root"),
        "a rename source pointing outside the workspace was accepted: {err}"
    );
}

/// `select` is normally handed a spec a verb has already checked. Reached
/// directly -- `jj --tool=jj-hunk`, with `JJ_HUNK_SELECTION` pointing wherever
/// the user put it -- there is no verb in front of it, so it has to refuse the
/// same keys on its own.
#[test]
fn select_refuses_a_spec_key_that_names_a_path_outside_the_workspace() {
    let repo = TestRepo::new("select-key-escape");
    let root = repo.path().to_path_buf();
    let left = root.join("L");
    let right = root.join("R");
    for dir in [&left, &right] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(left.join("a.txt"), "one\n").unwrap();
    std::fs::write(right.join("a.txt"), "one\ntwo\n").unwrap();

    let spec_path = root.join("sel.json");
    std::fs::write(
        &spec_path,
        r#"{"files": {"../../victim.txt": {"action": "reset"}}, "default": "keep"}"#,
    )
    .unwrap();

    let out = Command::new(jj_hunk_bin())
        .args(["select", left.to_str().unwrap(), right.to_str().unwrap()])
        .current_dir(&root)
        .env("JJ_HUNK_SELECTION", &spec_path)
        .env("JJ_CONFIG", &repo.config_path)
        .output()
        .expect("failed to run jj-hunk select");

    assert!(
        !out.status.success(),
        "select accepted a key naming a path outside the workspace"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("climbs above the workspace root"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Patterns are deliberately *not* held to the rule above, and this records the
/// choice so that it is a choice and not an oversight.
///
/// A key names one file and is resolved to it. A pattern names a set and is
/// only ever asked "does this path match?", against paths that came off the
/// diff -- so a pattern reaching outside the workspace cannot resolve to
/// anything even in principle, and matching nothing is the same answer it gives
/// for any other path that is not in the diff. Refusing it would buy nothing
/// and would cost the legitimate case in the second half of this test.
#[test]
fn a_pattern_reaching_outside_the_workspace_matches_nothing_rather_than_failing() {
    let repo = TestRepo::new("pattern-outside");
    repo.write_file("top.txt", "t1\n");
    repo.write_file("sub/mid.txt", "m1\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("top.txt", "t1\nt2\n");
    repo.write_file("sub/mid.txt", "m1\nm2\n");

    for args in [
        vec!["list", "--format", "json", "--files", "--include", "/etc/passwd"],
        vec!["list", "--format", "json", "--files", "--include", "../../etc/passwd"],
        vec!["list", "--format", "json", "--files", "--spec", r#"file("../../etc/passwd")"#],
        vec!["list", "--format", "json", "--files", "--spec", r#"glob("/etc/*")"#],
    ] {
        let listing: serde_json::Value =
            serde_json::from_str(&repo.hunk_ok(&args)).expect("listing should be json");
        assert_eq!(
            listing["files"].as_array().unwrap().len(),
            0,
            "{args:?} matched something"
        );
    }

    // The same `..` spelling, landing inside the workspace, still matches --
    // which is what a blanket refusal of `..` in a pattern would have cost.
    for args in [
        vec!["list", "--format", "json", "--files", "--include", "../top.txt"],
        vec!["list", "--format", "json", "--files", "--spec", r#"file("../top.txt")"#],
    ] {
        let listing: serde_json::Value =
            serde_json::from_str(&hunk_in_ok(&repo, "sub", &args)).expect("listing should be json");
        let files = listing["files"].as_array().unwrap();
        assert_eq!(files.len(), 1, "{args:?} stopped matching the file above sub/");
        assert_eq!(files[0]["path"].as_str().unwrap(), "../top.txt");
    }
}

// ---------------------------------------------------------------------------
// Structured errors: --error-format json / JJ_HUNK_ERROR_FORMAT
// ---------------------------------------------------------------------------

/// Run a command that must fail, and parse the JSON object it writes to
/// stderr.
///
/// Asserts the two invariants that hold for every structured failure: exit 1,
/// and *nothing* on stdout. Stdout is where `list --format json` puts its
/// result, so an error object leaking there would look to a caller like a
/// successful run carrying unexpected fields.
fn error_json(repo: &TestRepo, args: &[&str]) -> serde_json::Value {
    let mut argv = vec!["--error-format", "json"];
    argv.extend_from_slice(args);
    let out = repo.hunk(&argv);

    assert_eq!(out.status.code(), Some(1), "every failure exits 1");
    assert!(
        out.stdout.is_empty(),
        "an error must not write to stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    serde_json::from_str(&stderr)
        .unwrap_or_else(|e| panic!("stderr is not one JSON object ({e}): {stderr}"))
}

/// A repo with two well-separated hunks in one file.
fn error_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("a.txt", "a\nb\nc\nd\ne\nf\ng\nh\n");
    repo.jj_ok(&["commit", "-m", "base"]);
    repo.write_file("a.txt", "a\nB\nc\nd\ne\nf\ng\nH\n");
    repo
}

/// **The regression that would silently break every existing caller.**
///
/// `list --format json` writes to stdout and callers read non-empty stdout as
/// "here are the hunks". If a failure ever started writing its object there,
/// or if the prose on stderr changed shape, every script and agent driving
/// this binary today would misread a failed run as a successful one.
///
/// So the default is asserted to be exactly what it always was: exit 1, empty
/// stdout, prose beginning `Error: ` on stderr -- and *not* JSON, since a
/// caller sniffing "does stderr parse as an object?" to decide whether the new
/// format is in use must get a straight no.
///
/// This is a preservation guard: it passes before the change as well as after,
/// which is the whole point of it.
#[test]
fn without_opting_in_failures_look_exactly_as_they_always_did() {
    let repo = error_repo("errfmt-default");

    for args in [
        vec!["list", "--spec", "id(hunk-deadbeef)"],
        vec!["list", "--spec", "type(insert"],
        vec!["list", "--spec", "nosuchpred()"],
        vec!["list", "--rev", "bogus"],
        vec!["commit", r#"file("nope.txt")"#, "msg"],
    ] {
        let out = repo.hunk(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?} must still exit 1");
        assert!(out.stdout.is_empty(), "{args:?} must write nothing to stdout");

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with("Error: "),
            "{args:?} must still print prose: {stderr}"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&stderr).is_err(),
            "{args:?} must not emit JSON unless asked: {stderr}"
        );
    }
}

/// Opting in must not disturb the success path either. `list` writes its
/// result to stdout and exits 0 whether or not the flag is present -- the flag
/// only ever governs how a *failure* is rendered.
#[test]
fn opting_in_does_not_change_successful_output() {
    let repo = error_repo("errfmt-success");

    let plain = repo.hunk_ok(&["list"]);
    let opted_in = repo.hunk_ok(&["--error-format", "json", "list"]);
    assert_eq!(plain, opted_in, "the flag must not touch a successful run");

    let via_env = repo.hunk_with_env(&[("JJ_HUNK_ERROR_FORMAT", "json")], &["list"]);
    assert!(via_env.status.success());
    assert_eq!(String::from_utf8_lossy(&via_env.stdout), plain);
}

/// A stale id is the single most common thing an agent hits: ids are computed
/// from file content, so one copied from a listing taken before an edit names
/// nothing. `details.id` hands back the id that failed, so the agent can tell
/// "I used a stale id" from "I mistyped a predicate" without reading prose.
#[test]
fn an_unknown_id_reports_the_id_it_could_not_find() {
    let repo = error_repo("errfmt-unknown-id");
    let json = error_json(&repo, &["list", "--spec", r#"id("hunk-deadbeef")"#]);

    assert_eq!(json["error"], "selection");
    assert_eq!(json["code"], "UNKNOWN_ID");
    assert_eq!(json["details"]["id"], "hunk-deadbeef");
}

/// The payload that justifies the whole `details` field: the ids to retry
/// with. Recovering them used to mean splitting the rendered message on `", "`
/// and stripping the parenthesised path off each piece -- and the wording of
/// that message has already changed once.
#[test]
fn an_ambiguous_id_reports_the_candidates_to_retry_with() {
    let repo = many_hunk_repo("errfmt-ambiguous");
    let listed = listed_ids(&repo, &[]);
    let prefix = ambiguous_prefix(&listed);

    let json = error_json(&repo, &["list", "--spec", &format!(r#"id("{prefix}")"#)]);
    assert_eq!(json["code"], "AMBIGUOUS_ID");
    assert_eq!(json["details"]["prefix"], prefix);

    let candidates = json["details"]["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("candidates must be an array: {json}"));
    assert!(candidates.len() > 1, "{json}");
    assert_eq!(json["details"]["count"], candidates.len());

    // Each candidate must be usable as-is: it is an id the very next call can
    // pass to `id()` and get exactly one hunk back.
    for candidate in candidates {
        let id = candidate["short_id"].as_str().expect("short_id");
        assert_eq!(candidate["path"], "many.txt", "{json}");
        let listing =
            repo.hunk_ok(&["list", "--format", "text", "--spec", &format!(r#"id("{id}")"#)]);
        assert_eq!(listing.matches("  hunk ").count(), 1, "{listing}");
    }
}

/// A syntax error is the one failure where the actionable fact is a *position*
/// rather than a name. Human mode draws a caret; structured mode has to hand
/// over the offset the caret was drawn from, or a caller has to count the
/// spaces in front of a `^`.
#[test]
fn a_syntax_error_reports_where_the_caret_points() {
    let repo = error_repo("errfmt-parse");
    let json = error_json(&repo, &["list", "--spec", "type(insert"]);

    assert_eq!(json["error"], "parse");
    assert_eq!(json["code"], "PARSE_ERROR");
    assert_eq!(json["details"]["input"], "type(insert");
    assert_eq!(json["details"]["position"], 11);
    assert_eq!(json["details"]["line"], 1);
    assert_eq!(json["details"]["column"], 11);

    // A multi-line expression: `position` is an offset into the whole input,
    // `column` is within the offending line. Reporting only the first would
    // put the caret in the wrong place on every expression but a one-liner.
    let json = error_json(&repo, &["list", "--spec", "type(insert)\n  | file(\"a\""]);
    assert_eq!(json["details"]["line"], 2, "{json}");
    assert_ne!(json["details"]["column"], json["details"]["position"], "{json}");
}

/// A malformed glob is refused rather than left to match nothing, because
/// `~glob("vendor/**")` with a typo means "keep everything". The pattern comes
/// back verbatim so the caller can see which of several patterns was rejected.
#[test]
fn an_invalid_glob_reports_the_pattern_and_the_reason() {
    let repo = error_repo("errfmt-glob");
    let json = error_json(&repo, &["list", "--spec", r#"glob("[")"#]);

    assert_eq!(json["code"], "INVALID_GLOB");
    assert_eq!(json["details"]["pattern"], "[");
    assert!(
        json["details"]["reason"].as_str().unwrap().contains("unclosed"),
        "{json}"
    );

    // Same shape for the other pattern language, so a caller handling one
    // handles both.
    let json = error_json(&repo, &["list", "--spec", r#"content(regex:"[")"#]);
    assert_eq!(json["code"], "INVALID_REGEX");
    assert_eq!(json["details"]["pattern"], "[");
}

/// Told apart from a *stale* selector, which is the distinction that decides
/// what an agent does next: a misspelled predicate is fixed by rewriting the
/// query, a stale id by re-listing. Both used to be one exit code and a
/// paragraph.
#[test]
fn an_unknown_predicate_and_a_bad_argument_report_what_was_wrong_with_them() {
    let repo = error_repo("errfmt-predicate");

    let json = error_json(&repo, &["list", "--spec", "nosuchpred()"]);
    assert_eq!(json["code"], "UNKNOWN_FUNCTION");
    assert_eq!(json["details"]["function"], "nosuchpred");

    let json = error_json(&repo, &["list", "--spec", "type(nope)"]);
    assert_eq!(json["code"], "INVALID_ARGUMENT");
    assert_eq!(json["details"]["function"], "type");
    assert_eq!(json["details"]["value"], "nope");
    assert!(
        json["details"]["valid"].as_str().unwrap().contains("insert"),
        "the accepted values are the actionable part: {json}"
    );
}

/// "jj cannot resolve this" and "this names several revisions" want opposite
/// responses -- fix the revset, versus narrow it -- and both used to arrive as
/// exit 1 with a paragraph. `resolved` is the count either way, so a caller
/// can branch on the number rather than on the code alone.
#[test]
fn an_unresolvable_revset_is_told_apart_from_one_naming_several() {
    let repo = error_repo("errfmt-revset");

    let json = error_json(&repo, &["list", "--rev", "bogus"]);
    assert_eq!(json["error"], "revset");
    assert_eq!(json["code"], "REVSET_UNRESOLVED");
    assert_eq!(json["details"]["revset"], "bogus");
    assert_eq!(json["details"]["resolved"], 0);
    // jj's own explanation, passed through: it is the only thing that says why.
    assert!(json["details"]["jj_stderr"].is_string(), "{json}");

    let json = error_json(&repo, &["list", "--rev", "all()"]);
    assert_eq!(json["code"], "REVSET_AMBIGUOUS");
    let resolved = json["details"]["resolved"].as_u64().expect("resolved");
    assert!(resolved > 1, "{json}");
    assert!(
        !json["details"]["revisions"].as_array().expect("revisions").is_empty(),
        "{json}"
    );
}

/// An empty selection is refused rather than committed, and the selector that
/// matched nothing comes back so the caller can retry a widened version of it
/// without having to remember what it sent.
#[test]
fn an_empty_selection_reports_the_selector_that_matched_nothing() {
    let repo = error_repo("errfmt-empty");
    let json = error_json(&repo, &["commit", r#"file("nope.txt")"#, "msg"]);

    assert_eq!(json["code"], "EMPTY_SELECTION");
    assert_eq!(json["details"]["selector"], r#"file("nope.txt")"#);
    // Which listing to check it against -- `restore` reads its ids from the
    // reversed diff, so this is not always plain `jj-hunk list`.
    assert_eq!(json["details"]["listing_command"], "jj-hunk list");
}

/// A spec fails as a whole document, and every entry that did not resolve is
/// named. `path` is on each problem because that is what the caller has to
/// edit; `kind` says whether the path, the id or the index was the wrong part.
#[test]
fn a_spec_that_does_not_resolve_names_every_entry_that_failed() {
    let repo = error_repo("errfmt-unresolved-spec");
    let listed = listed_ids(&repo, &[]);
    let (_, good_id, _) = &listed[0];

    let spec = format!(
        r#"{{"files": {{"nope.txt": {{"ids": ["hunk-aaaaaaaa"]}},
                       "a.txt": {{"hunks": [0, 99], "ids": ["{good_id}"]}}}}}}"#
    );
    let json = error_json(&repo, &["commit", &spec, "msg"]);

    assert_eq!(json["code"], "PATH_NOT_IN_DIFF");
    let problems = json["details"]["problems"].as_array().expect("problems");

    let absent = problems
        .iter()
        .find(|p| p["kind"] == "no-such-path")
        .unwrap_or_else(|| panic!("{json}"));
    assert_eq!(absent["path"], "nope.txt");

    let bad_index = problems
        .iter()
        .find(|p| p["kind"] == "no-such-index")
        .unwrap_or_else(|| panic!("{json}"));
    assert_eq!(bad_index["path"], "a.txt");
    assert_eq!(bad_index["index"], 99);
    assert_eq!(bad_index["hunk_count"], 2);
}

/// Ids are hashes of the text they were diffed from, so a template built over
/// a file `--max-bytes` cut short would name hunks the real diff does not
/// have. The refusal lists the files to drop the limit for.
#[test]
fn a_spec_template_over_truncated_files_names_the_files() {
    let repo = error_repo("errfmt-truncated");
    let json = error_json(&repo, &["list", "--spec-template", "--max-bytes", "4"]);

    assert_eq!(json["error"], "usage");
    assert_eq!(json["code"], "TRUNCATED_SPEC_TEMPLATE");
    assert_eq!(json["details"]["paths"], serde_json::json!(["a.txt"]));
}

/// Not every failure has a code yet -- an I/O error opening a spec file has
/// none. Those must still arrive as a parseable object carrying the full
/// prose, or a caller that opted in has to keep a second, text-scraping code
/// path for exactly the failures nobody has classified, which is the fallback
/// this feature exists to retire.
#[test]
fn an_uncoded_failure_is_still_a_parseable_object() {
    let repo = error_repo("errfmt-uncoded");
    let json = error_json(&repo, &["list", "--spec-file", "/nonexistent/spec.json"]);

    assert_eq!(json["error"], "internal");
    assert_eq!(json["code"], "UNKNOWN");
    assert_eq!(json["details"], serde_json::json!({}));
    // The whole chain, exactly as human mode prints it, so nothing is lost.
    let message = json["message"].as_str().expect("message");
    assert!(message.contains("/nonexistent/spec.json"), "{message}");
    assert!(message.contains("Caused by"), "{message}");
}

/// The environment variable exists because the flag cannot reach everywhere it
/// is needed: the mutating verbs re-enter this binary as `jj-hunk select`
/// through `jj --tool=jj-hunk`, and nothing threads an argv across that hop.
/// An agent driving many invocations also sets it once rather than per call.
///
/// The flag wins when both are given, so a single call can opt back out of a
/// session-wide setting.
#[test]
fn the_env_var_opts_in_and_the_flag_overrides_it() {
    let repo = error_repo("errfmt-env");
    let args = ["list", "--spec", "nosuchpred()"];

    let via_env = repo.hunk_with_env(&[("JJ_HUNK_ERROR_FORMAT", "json")], &args);
    assert_eq!(via_env.status.code(), Some(1));
    assert!(via_env.stdout.is_empty());
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&via_env.stderr)).expect("JSON on stderr");
    assert_eq!(json["code"], "UNKNOWN_FUNCTION");

    // Case is forgiven. An agent that exports JSON rather than json has still
    // opted in, and silently handing it prose instead is the worst outcome:
    // it would parse the prose as JSON forever.
    let upper = repo.hunk_with_env(&[("JJ_HUNK_ERROR_FORMAT", "JSON")], &args);
    assert_eq!(
        String::from_utf8_lossy(&upper.stderr),
        String::from_utf8_lossy(&via_env.stderr)
    );

    let mut with_flag = vec!["--error-format", "human"];
    with_flag.extend_from_slice(&args);
    let overridden = repo.hunk_with_env(&[("JJ_HUNK_ERROR_FORMAT", "json")], &with_flag);
    let stderr = String::from_utf8_lossy(&overridden.stderr);
    assert!(stderr.starts_with("Error: "), "the flag must win: {stderr}");
}

/// The flag is global, so it may be written before or after the subcommand.
/// An agent assembling an argv should not have to know which position clap
/// wants, and getting it wrong would exit 2 with a usage error rather than the
/// structured object it asked for.
#[test]
fn the_flag_is_accepted_on_either_side_of_the_subcommand() {
    let repo = error_repo("errfmt-position");

    let before = repo.hunk(&["--error-format", "json", "list", "--spec", "nosuchpred()"]);
    let after = repo.hunk(&["list", "--spec", "nosuchpred()", "--error-format", "json"]);

    assert_eq!(before.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&before.stderr),
        String::from_utf8_lossy(&after.stderr)
    );
}

// ---------------------------------------------------------------------------
// jj-hunk schema
// ---------------------------------------------------------------------------
//
// `--help` describes the command line. It cannot describe the hunkset
// language, and the part of that language most expensive to get wrong -- which
// predicates can reach a change that produced no hunks -- is invisible until it
// produces a wrong answer at exit 0. These tests hold the published description
// against what the binary really does.

/// Parse `jj-hunk schema`, asserting the invariants that hold for every run.
fn schema_json() -> serde_json::Value {
    // Deliberately not in a repo: a caller reads the schema to find out how to
    // talk to this tool, which it may well do before it has a workspace, and
    // before it has learnt that most subcommands need one.
    let out = Command::new(jj_hunk_bin())
        .arg("schema")
        .current_dir(std::env::temp_dir())
        .env_remove("JJ_CONFIG")
        .output()
        .expect("failed to run jj-hunk schema");

    assert_eq!(
        out.status.code(),
        Some(0),
        "schema must exit 0 outside a repo: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "schema wrote to stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("schema did not emit JSON on stdout")
}

fn published_predicate<'a>(schema: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    schema["hunkset"]["predicates"]
        .as_array()
        .expect("predicates must be an array")
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("the schema does not publish {name}()"))
}

/// The output has to be machine-readable without a repo, and the fields a
/// caller is told to branch on have to be there under those names.
///
/// `schema_version` is the one an agent pins: it is bumped when a field is
/// removed or changes meaning, and left alone when one is added, so a caller
/// can refuse a shape it does not understand instead of misreading it.
#[test]
fn schema_is_json_on_stdout_and_needs_no_repo() {
    let schema = schema_json();

    assert_eq!(schema["schema_version"], 1);
    assert_eq!(schema["tool"]["name"], "jj-hunk");
    assert_eq!(schema["tool"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(schema["hunkset"]["predicates"].is_array());
    assert!(schema["errors"].is_array());
    assert!(schema["commands"].is_array());
}

/// **The claim this feature exists to make, checked against the binary.**
///
/// A change that produced no hunks -- here a pure rename -- is reachable by a
/// file-level predicate and by nothing else. The schema says so in two fields;
/// this drives the real binary over a real rename and confirms both.
///
/// Without this, `class` and `reaches_hunkless_changes` are a comment in a JSON
/// file. An agent that believed a wrong one would run `split 'content("x")'`,
/// see exit 0, and never learn that the rename it left behind was reset.
#[test]
fn the_class_the_schema_publishes_is_the_one_the_binary_enforces() {
    let schema = schema_json();
    let repo = pure_rename_repo("schema-class-split");

    // The rename is in the diff at all -- otherwise everything below is
    // vacuously true.
    let everything = listed_paths(&repo, &["list", "--spec", "all"]);
    assert!(
        everything.contains(&"dst.txt".to_string()),
        "the hunkless change is not in the diff: {everything:?}"
    );

    for name in ["file", "glob", "extension", "status", "type"] {
        let published = published_predicate(&schema, name);
        assert_eq!(published["class"], "file", "{name}() is published wrong");
        assert_eq!(published["reaches_hunkless_changes"], true);
    }
    for name in ["content", "added", "removed", "lines", "id"] {
        let published = published_predicate(&schema, name);
        assert_eq!(published["class"], "content", "{name}() is published wrong");
        assert_eq!(published["reaches_hunkless_changes"], false);
    }

    // A file-level predicate written as widely as it can be: the rename comes
    // with it.
    let by_file = listed_paths(&repo, &["list", "--spec", r#"glob("*.txt")"#]);
    assert!(
        by_file.contains(&"dst.txt".to_string()),
        "glob() is published as class file but did not reach the rename: {by_file:?}"
    );

    // And a content predicate written as widely as it can be -- the empty
    // substring is inside every string -- still cannot.
    let by_content = listed_paths(&repo, &["list", "--spec", r#"content("")"#]);
    assert!(
        !by_content.is_empty(),
        "content(\"\") matched nothing at all, so it proves nothing"
    );
    assert!(
        !by_content.contains(&"dst.txt".to_string()),
        "content() is published as class content but reached the rename: {by_content:?}"
    );

    // The line predicates were the other half of the same bug: line 0 is inside
    // 0..N, and a stand-in reports a zero-length range at line 0.
    let by_lines = listed_paths(&repo, &["list", "--spec", "lines(0..100000)"]);
    assert!(
        !by_lines.contains(&"dst.txt".to_string()),
        "lines() is published as class content but reached the rename: {by_lines:?}"
    );
}

/// `build.semantic` must describe the binary that answered, not the binary the
/// developer happened to build.
///
/// CI builds `--no-default-features`, where the eight tree-sitter predicates
/// are absent. A schema that claimed otherwise would send a caller down a path
/// that always fails, which is exactly the discovery-by-failure this replaces.
/// So the claim is checked by asking the same binary to run one.
#[test]
fn the_schema_agrees_with_this_build_about_the_semantic_predicates() {
    let schema = schema_json();
    let available = schema["build"]["semantic"]
        .as_bool()
        .expect("build.semantic must be a boolean");

    for name in [
        "function",
        "scope",
        "annotation",
        "decorator",
        "doc",
        "import",
        "toplevel",
        "depth",
    ] {
        let published = published_predicate(&schema, name);
        assert_eq!(published["class"], "semantic", "{name}() is published wrong");
        assert_eq!(published["available"], available);
        assert_eq!(published["requires_feature"], "semantic");
    }

    let repo = pure_rename_repo("schema-semantic-availability");
    let out = repo.hunk(&["--error-format", "json", "list", "--spec", "doc()"]);

    if available {
        assert!(
            out.status.success(),
            "the schema says semantic predicates work here, but doc() failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    } else {
        let json: serde_json::Value = serde_json::from_slice(&out.stderr)
            .expect("a refusal must still be structured on stderr");
        assert_eq!(
            json["code"], "SEMANTIC_FEATURE_REQUIRED",
            "the schema says semantic predicates are unavailable here, so doc() must say why"
        );
    }
}

/// Every code the binary actually emits is one the schema listed up front.
///
/// The point of publishing the list is that a caller can write its retry logic
/// before it has ever seen a failure. That only holds if the list is complete:
/// a code met at runtime and absent from the schema falls into the caller's
/// "unknown failure" branch, which is the prose-matching fallback the codes
/// exist to retire. Triggering them is also how a caller would otherwise have
/// to find them, and for the mutating verbs that means doing it to a real
/// revision.
#[test]
fn every_error_code_the_binary_emits_is_one_the_schema_published() {
    let schema = schema_json();
    let published: Vec<&str> = schema["errors"]
        .as_array()
        .expect("errors must be an array")
        .iter()
        .map(|e| e["code"].as_str().expect("a code is a string"))
        .collect();
    assert!(
        published.contains(&"UNKNOWN"),
        "the catch-all code must be published, or a caller has no branch for an uncoded failure"
    );

    let repo = error_repo("schema-error-codes");
    for spec in [
        "nosuchpred()",
        "type(insert",
        r#"status("nope")"#,
        r#"glob(regex:"[")"#,
        r#"id("hunk-deadbeef")"#,
    ] {
        let json = error_json(&repo, &["list", "--spec", spec]);
        let code = json["code"].as_str().expect("every failure carries a code");
        assert!(
            published.contains(&code),
            "{spec:?} failed with {code}, which `schema` does not publish: {published:?}"
        );
        assert_ne!(
            code, "UNKNOWN",
            "{spec:?} is a selection error and should carry a specific code"
        );
    }
}

/// The command list is generated from clap's own view of the parser, so a
/// renamed flag or a new subcommand cannot leave a stale entry. This proves the
/// generation is wired to the real parser rather than to a copy of it.
///
/// It stays deliberately shallow: flags, arity and defaults are in `--help`,
/// which clap builds from the same definitions. What `--help` does not answer
/// in one read is which verbs take a hunkset at all.
#[test]
fn the_published_commands_are_the_ones_the_binary_accepts() {
    let schema = schema_json();
    let commands = schema["commands"]
        .as_array()
        .expect("commands must be an array");

    let help = String::from_utf8_lossy(
        &Command::new(jj_hunk_bin())
            .arg("--help")
            .output()
            .expect("failed to run jj-hunk --help")
            .stdout,
    )
    .to_string();

    for command in commands {
        let name = command["name"].as_str().expect("a name is a string");
        assert!(
            help.contains(name),
            "the schema publishes {name}, which --help does not list"
        );
    }

    let named: Vec<&str> = commands
        .iter()
        .map(|c| c["name"].as_str().unwrap_or_default())
        .collect();
    assert!(named.contains(&"schema"), "schema must describe itself too");

    let accepts_selection = |name: &str| {
        commands
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not published"))["accepts_selection"]
            == serde_json::Value::Bool(true)
    };
    assert!(accepts_selection("split"), "split takes a hunkset");
    assert!(accepts_selection("list"), "list filters with --spec");
    assert!(
        !accepts_selection("select"),
        "select is the merge-tool hop and reads its selection from the environment"
    );
}

/// Argument facts are read off the evaluator rather than restated, and these
/// are the two that silently cost a caller a whole selection when guessed.
///
/// A misspelled `status()` or `type()` value would select nothing at exit 0 if
/// it were not rejected, so the closed value sets are the most useful thing in
/// the output after the class split. The default pattern kind is next:
/// `added(TODO)` is a substring match while `file(src/a.rs)` is an equality
/// one, and getting that backwards is an empty result rather than an error.
#[test]
fn the_schema_publishes_the_argument_facts_that_are_silent_when_guessed_wrong() {
    let schema = schema_json();
    let repo = error_repo("schema-argument-facts");

    for (name, expected) in [
        (
            "status",
            vec!["modified", "added", "removed", "renamed", "copied"],
        ),
        ("type", vec!["insert", "delete", "replace"]),
    ] {
        let values: Vec<&str> = published_predicate(&schema, name)["values"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}() must publish its value set"))
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        assert_eq!(values, expected);

        // Every published value is one the binary really takes, and one that is
        // not published is refused rather than quietly matching nothing.
        for value in &values {
            repo.hunk_ok(&["list", "--spec", &format!(r#"{name}("{value}")"#)]);
        }
        let json = error_json(&repo, &["list", "--spec", &format!(r#"{name}("nope")"#)]);
        assert_eq!(json["code"], "INVALID_ARGUMENT");
    }

    assert_eq!(
        published_predicate(&schema, "file")["default_pattern_kind"],
        "exact"
    );
    assert_eq!(
        published_predicate(&schema, "glob")["default_pattern_kind"],
        "glob"
    );
    assert_eq!(
        published_predicate(&schema, "content")["default_pattern_kind"],
        "substring"
    );

    // `id()` resolves its argument instead of matching with it, so it is the
    // one predicate that does not take the whole prefix set.
    let prefixes: Vec<&str> = published_predicate(&schema, "id")["pattern_prefixes"]
        .as_array()
        .expect("id() publishes its prefixes")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    assert_eq!(prefixes, vec!["exact"]);
    let json = error_json(&repo, &["list", "--spec", r#"id(substring:"abcd")"#]);
    assert_eq!(json["code"], "INVALID_ARGUMENT");
}
// ---------------------------------------------------------------------------
// list --fields: masking the output
// ---------------------------------------------------------------------------

/// A diff whose text dwarfs its structure, which is the case `--fields` is
/// for: every hunk carries its own `removed`/`added` and three lines of
/// context on each side, and none of it is read by an agent that lists, picks
/// an id, and acts.
fn mask_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    let before: String = (1..=200).map(|i| format!("line {i:03} of the file\n")).collect();
    repo.write_file("wide.txt", &before);
    repo.jj_ok(&["commit", "-m", "base"]);

    // Twenty edits spread far enough apart to stay twenty separate hunks.
    let after: String = (1..=200)
        .map(|i| {
            if i % 10 == 0 {
                format!("LINE {i:03} was rewritten by an agent\n")
            } else {
                format!("line {i:03} of the file\n")
            }
        })
        .collect();
    repo.write_file("wide.txt", &after);
    repo
}

/// A repo holding one of every shape whose change no hunk can express, plus a
/// rename, so a mask can be watched around all of them at once.
fn hunkless_repo(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.write_file("old.txt", "keep\nchange me\nkeep\n");
    repo.write_file("bin.dat", "plain\n");
    repo.write_file("target.txt", "a target\n");
    repo.jj_ok(&["commit", "-m", "base"]);

    std::fs::rename(repo.path().join("old.txt"), repo.path().join("new.txt")).unwrap();
    repo.write_file("new.txt", "keep\nchanged\nkeep\n");
    std::fs::write(repo.path().join("bin.dat"), [0x00u8, 0x01, 0xff, 0xfe]).unwrap();
    std::fs::remove_file(repo.path().join("target.txt")).unwrap();
    std::os::unix::fs::symlink("new.txt", repo.path().join("target.txt")).unwrap();
    repo
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not json ({e}): {text}"))
}

/// **The regression that would break every existing caller.**
///
/// `--fields` re-emits a listing through a second serialiser, so the risk is
/// not that the masked form is wrong -- it is that the *unmasked* one quietly
/// changes shape along with it. A caller that never passes the flag must get
/// byte-for-byte what it got before this existed, in every format and
/// grouping.
///
/// This is a preservation guard: it passes before the change as well as after,
/// which is the whole point of it.
#[test]
fn a_listing_without_fields_is_exactly_what_it_always_was() {
    let repo = hunkless_repo("fields-untouched");

    for args in [
        vec!["list"],
        vec!["list", "--format", "yaml"],
        vec!["list", "--format", "text"],
        vec!["list", "--format", "diff"],
        vec!["list", "--group", "status"],
        vec!["list", "--files"],
        vec!["list", "--spec-template"],
    ] {
        let out = repo.hunk(&args);
        assert!(out.status.success(), "{args:?} must still succeed");
        assert!(
            !String::from_utf8_lossy(&out.stdout).is_empty(),
            "{args:?} must still print something"
        );
    }

    // The shape a caller actually parses, spelled out rather than merely
    // non-empty: the mask must not have moved a key or dropped an optional.
    let json = parse(&repo.hunk_ok(&["list"]));
    let files = json["files"].as_array().unwrap();
    let renamed = files.iter().find(|f| f["path"] == "new.txt").unwrap();
    assert_eq!(renamed["rename"]["from"], "old.txt");
    assert!(renamed["hunks"][0]["removed"].is_string(), "{renamed}");
    assert!(renamed["hunks"][0]["context"].is_object(), "{renamed}");
}

/// The point of the feature, measured rather than asserted. The mask an agent
/// running list -> preview -> act by id actually wants keeps four keys and
/// drops the diff text, and the response has to get dramatically smaller --
/// this is a token-cost feature, so a mask that saved a little would not be
/// worth the flag.
#[test]
fn a_mask_drops_the_diff_text_and_keeps_what_a_selection_needs() {
    let repo = mask_repo("fields-size");

    let full = repo.hunk_ok(&["list"]);
    let masked = repo.hunk_ok(&["list", "--fields", "path,hunks.id,hunks.type"]);

    assert!(
        masked.len() * 4 < full.len(),
        "a mask that keeps only paths, ids and types must be far smaller: \
         {} bytes masked vs {} bytes full",
        masked.len(),
        full.len()
    );

    let json = parse(&masked);
    let file = &json["files"][0];
    assert_eq!(file["path"], "wide.txt");
    assert!(file["status"].is_null(), "status was not asked for: {file}");

    let hunks = file["hunks"].as_array().unwrap();
    assert_eq!(hunks.len(), 20, "the hunks themselves must all still be there");
    for hunk in hunks {
        assert!(hunk["id"].as_str().unwrap().starts_with("hunk-"), "{hunk}");
        assert!(hunk["type"].is_string(), "{hunk}");
        assert!(hunk["removed"].is_null(), "the diff text must be gone: {hunk}");
        assert!(hunk["added"].is_null(), "the diff text must be gone: {hunk}");
        assert!(hunk["context"].is_null(), "the context must be gone: {hunk}");
    }

    // The line ranges are maskable too, and are the one part of a hunk an
    // agent may still want -- they are what says *where* to look before it
    // opens the file.
    let with_ranges = parse(&repo.hunk_ok(&["list", "--fields", "path,hunks.id,hunks.before"]));
    let hunk = &with_ranges["files"][0]["hunks"][0];
    assert!(hunk["before"]["start"].is_number(), "{hunk}");
    assert!(hunk["after"].is_null(), "only `before` was asked for: {hunk}");
}

/// An id read out of a masked listing has to be the same id. Nothing in the
/// mask touches how ids are computed, but that is exactly the sort of thing a
/// re-serialisation quietly breaks -- and an id that no longer resolves turns
/// a cheaper listing into a useless one.
#[test]
fn ids_from_a_masked_listing_still_select() {
    let repo = mask_repo("fields-roundtrip");
    let json = parse(&repo.hunk_ok(&["list", "--fields", "path,hunks.id"]));
    let id = json["files"][0]["hunks"][0]["id"].as_str().unwrap().to_string();

    repo.hunk_ok(&["commit", &format!(r#"id("{id}")"#), "one hunk, chosen blind"]);

    let summary = repo.changed_files("@-");
    assert!(
        summary.iter().any(|line| line.contains("wide.txt")),
        "the hunk named by a masked id did not land: {summary:?}"
    );
    // And only that one: 19 of the 20 hunks are still in the working copy.
    let left = parse(&repo.hunk_ok(&["list"]));
    assert_eq!(left["files"][0]["hunks"].as_array().unwrap().len(), 19);
}

/// The two spellings are one feature. `hunks.id` says where the field lives
/// and is what the error message hands back; `id` is what an agent types. If
/// they ever disagreed, the shorthand would be a second, subtly different
/// flag.
#[test]
fn the_bare_and_dotted_spellings_produce_the_same_listing() {
    let repo = mask_repo("fields-spelling");
    assert_eq!(
        repo.hunk_ok(&["list", "--fields", "path,id,type"]),
        repo.hunk_ok(&["list", "--fields", "path,hunks.id,hunks.type"])
    );
    // And `hunks` alone is every hunk field, not a hunk field called "hunks".
    assert_eq!(
        repo.hunk_ok(&["list", "--fields", "path,status,hunks"]),
        repo.hunk_ok(&["list"])
    );
}

/// A typo in a mask is indistinguishable from a deliberate omission: ask for
/// `paht` and every entry comes back without a `path`, which reads as a diff
/// of files that have no path. So it is refused, and `details.valid_fields`
/// carries the whole list so the caller can correct itself rather than ask a
/// human what the fields are called.
#[test]
fn an_unknown_field_name_is_refused_with_the_list_to_correct_it_from() {
    let repo = mask_repo("fields-typo");
    let json = error_json(&repo, &["list", "--fields", "paht,id"]);

    assert_eq!(json["error"], "usage");
    assert_eq!(json["code"], "INVALID_FIELDS");
    assert_eq!(json["details"]["fields"], serde_json::json!(["paht"]));

    let valid = json["details"]["valid_fields"].as_array().unwrap();
    assert!(valid.iter().any(|name| name == "path"), "{valid:?}");
    assert!(valid.iter().any(|name| name == "hunks.removed"), "{valid:?}");
    assert_eq!(
        json["details"]["always_included"],
        serde_json::json!(["rename", "binary", "mode", "symlink", "truncated"])
    );

    // A mask that names nothing at all is the same refusal: an empty listing
    // is not what "--fields ''" was reaching for either.
    assert_eq!(
        error_json(&repo, &["list", "--fields", ""])["code"],
        "INVALID_FIELDS"
    );
}

/// The five file-level flags survive a mask that names none of them, because
/// each is serialised only when it is true -- so a mask able to drop one could
/// forge "this file is not a rename", "this listing is not truncated". The two
/// that carry more than information are `rename`, whose `from` is what makes a
/// renamed file selectable on the raw `jj --tool=jj-hunk` path, and
/// `truncated`, which is the only sign that the hunks listed are a prefix of
/// the real diff.
#[test]
fn a_mask_cannot_hide_that_something_happened() {
    let repo = hunkless_repo("fields-flags");
    let json = parse(&repo.hunk_ok(&["list", "--fields", "hunks.id"]));
    let files = json["files"].as_array().unwrap();

    let by = |path: &str| {
        // `path` itself was not asked for, so entries are found by the flag
        // that survived -- which is the property under test.
        files
            .iter()
            .find(|f| f["rename"]["from"] == path || f["rename"]["to"] == path)
            .cloned()
    };

    let renamed = by("old.txt").expect("the rename must still be visible");
    assert_eq!(renamed["rename"]["from"], "old.txt");
    assert!(renamed["path"].is_null(), "path was not asked for: {renamed}");

    assert!(
        files.iter().any(|f| f["binary"] == true),
        "the binary marker did not survive: {json}"
    );
    assert!(
        files.iter().any(|f| f["symlink"] == true),
        "the symlink marker did not survive: {json}"
    );

    // Truncation is the one whose absence is a correctness claim.
    let truncated = parse(&repo.hunk_ok(&[
        "list",
        "--max-lines",
        "1",
        "--fields",
        "hunks.id",
    ]));
    assert!(
        truncated["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["truncated"] == true),
        "a masked listing must still say it was cut short: {truncated}"
    );
}

/// A mask names fields of a listing, and three of `list`'s output modes are
/// not listings. Serving them unmasked at exit 0 is the one answer that cannot
/// be right: a caller passes `--fields` to make a response smaller, and the
/// whole thing coming back looks exactly like a mask with nothing to drop.
#[test]
fn fields_is_refused_by_the_output_modes_it_cannot_shape() {
    let repo = mask_repo("fields-modes");

    for (args, conflict) in [
        (vec!["list", "--fields", "path", "--files"], "--files"),
        (
            vec!["list", "--fields", "path", "--spec-template"],
            "--spec-template",
        ),
        (
            vec!["list", "--fields", "path", "--format", "text"],
            "--format text",
        ),
        (
            vec!["list", "--fields", "path", "--format", "diff"],
            "--format diff",
        ),
    ] {
        let json = error_json(&repo, &args);
        assert_eq!(json["code"], "INCOMPATIBLE_OPTIONS", "{args:?}");
        assert_eq!(json["details"]["option"], "--fields", "{args:?}");
        assert_eq!(json["details"]["incompatible_with"], conflict, "{args:?}");
    }

    // The refusal that set the precedent, which had been falling through to
    // UNKNOWN. Leaving it there would have made the code useless to branch on:
    // a caller would still need a prose fallback for this very class.
    let json = error_json(&repo, &["list", "--spec-template", "--format", "text"]);
    assert_eq!(json["code"], "INCOMPATIBLE_OPTIONS");
    assert_eq!(json["details"]["option"], "--spec-template");
}

/// Grouping and masking are orthogonal, and the group's own `name` is not a
/// field of anything -- masking it away would leave the files in an
/// unlabelled bag, which is worse than not grouping at all.
#[test]
fn a_mask_applies_inside_groups_without_unlabelling_them() {
    let repo = hunkless_repo("fields-groups");
    let json = parse(&repo.hunk_ok(&[
        "list",
        "--group",
        "status",
        "--fields",
        "path,hunks.id",
    ]));

    let groups = json["groups"].as_array().unwrap();
    assert!(!groups.is_empty(), "{json}");
    for group in groups {
        assert!(group["name"].is_string(), "a group lost its name: {group}");
        for file in group["files"].as_array().unwrap() {
            assert!(file["path"].is_string(), "{file}");
            assert!(file["status"].is_null(), "status was not asked for: {file}");
        }
    }
}
