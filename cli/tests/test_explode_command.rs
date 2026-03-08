// Copyright 2026 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::common::TestEnvironment;

#[test]
fn test_explode_no_history() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    // Create a fresh commit via `jj new` — it should have a single evolog entry
    work_dir.run_jj(["new"]).success();
    let output = work_dir.run_jj(["explode"]);
    insta::assert_snapshot!(output, @r"
    ------- stderr -------
    Nothing to explode.
    [EOF]
    ");
}

#[test]
fn test_explode_with_history() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    // Start from a fresh commit to get predictable evolog
    work_dir.run_jj(["new"]).success();
    work_dir.write_file("file", "v1\n");
    work_dir.run_jj(["describe", "-m", "first version"]).success();
    work_dir.write_file("file", "v2\n");
    work_dir.run_jj(["describe", "-m", "second version"]).success();

    let output = work_dir.run_jj(["explode"]);
    insta::assert_snapshot!(output, @r"
    ------- stderr -------
    Created zxsnswpr 74f7d6cf (empty) (no description set)
    Created vuyypyzk 23e2283c (no description set)
    Created vnkrswwx 6baf1f5f (empty) first version
    Created rnprkorl 1fcb2fe0 first version
    Created qrrxplmp 186904b5 (empty) second version
    [EOF]
    ");

    // Verify the chain was created in the log
    let output = work_dir.run_jj(["log", "-r", "all() ~ root()"]);
    insta::assert_snapshot!(output, @r"
    @  rlvkpnrz test.user@example.com 2001-02-03 08:05:10 d08e4bca
    │  second version
    │ ○  qrrxplmp test.user@example.com 2001-02-03 08:05:11 186904b5
    │ │  (empty) second version
    │ ○  rnprkorl test.user@example.com 2001-02-03 08:05:11 1fcb2fe0
    │ │  first version
    │ ○  vnkrswwx test.user@example.com 2001-02-03 08:05:11 6baf1f5f
    │ │  (empty) first version
    │ ○  vuyypyzk test.user@example.com 2001-02-03 08:05:11 23e2283c
    │ │  (no description set)
    │ ○  zxsnswpr test.user@example.com 2001-02-03 08:05:11 74f7d6cf
    ├─╯  (empty) (no description set)
    ○  qpvuntsm test.user@example.com 2001-02-03 08:05:07 e8849ae1
    │  (empty) (no description set)
    ~
    [EOF]
    ");
}

#[test]
fn test_explode_specific_revision() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    // Create a commit with history
    work_dir.run_jj(["new"]).success();
    work_dir.write_file("file", "v1\n");
    work_dir.run_jj(["describe", "-m", "first"]).success();
    work_dir.write_file("file", "v2\n");
    work_dir.run_jj(["describe", "-m", "second"]).success();
    // Save the change id
    let change_id = work_dir
        .run_jj(["log", "--no-graph", "-r", "@", "-T", "change_id"])
        .success()
        .stdout
        .into_raw();
    let change_id = change_id.trim();
    // Move to a new commit
    work_dir.run_jj(["new", "-m", "child commit"]).success();

    // Explode the previous commit by change id
    let output = work_dir.run_jj(["explode", "-r", change_id]);
    insta::assert_snapshot!(output, @r"
    ------- stderr -------
    Created spxsnpux 3a85e9f4 (empty) (no description set)
    Created uyxvvpyv 8e0c7ee3 (no description set)
    Created ulkllqzl 4579258e (empty) first
    Created woxlrlun b04ccdb2 first
    Created xxroyzqx 959d369e (empty) second
    [EOF]
    ");
}

#[test]
fn test_explode_root_commit() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    let output = work_dir.run_jj(["explode", "-r", "root()"]);
    insta::assert_snapshot!(output, @r"
    ------- stderr -------
    Error: Cannot explode the root commit
    [EOF]
    [exit status: 1]
    ");
}

#[test]
fn test_explode_original_untouched() {
    let test_env = TestEnvironment::default();
    test_env.run_jj_in(".", ["git", "init", "repo"]).success();
    let work_dir = test_env.work_dir("repo");

    work_dir.run_jj(["new"]).success();
    work_dir.write_file("file", "v1\n");
    work_dir.run_jj(["describe", "-m", "first"]).success();
    work_dir.write_file("file", "v2\n");
    work_dir.run_jj(["describe", "-m", "second"]).success();

    // Record the commit id before explode
    let before_id = work_dir
        .run_jj(["log", "--no-graph", "-r", "@", "-T", "commit_id"])
        .success()
        .stdout
        .into_raw();
    let before_id = before_id.trim().to_owned();

    work_dir.run_jj(["explode"]).success();

    // The original @ should still be the same commit
    let after_id = work_dir
        .run_jj(["log", "--no-graph", "-r", "@", "-T", "commit_id"])
        .success()
        .stdout
        .into_raw();
    let after_id = after_id.trim();

    assert_eq!(before_id, after_id);
}
