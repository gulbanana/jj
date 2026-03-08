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

use std::io::Write as _;

use clap_complete::ArgValueCompleter;
use itertools::Itertools as _;
use jj_lib::evolution::walk_predecessors;
use jj_lib::repo::Repo as _;
use tracing::instrument;

use crate::cli_util::CommandHelper;
use crate::cli_util::RevisionArg;
use crate::command_error::CommandError;
use crate::command_error::user_error;
use crate::complete;
use crate::ui::Ui;

/// Create a chain of commits from a change's evolution history
///
/// Takes the evolution log (evolog) of a revision and creates a new chain of
/// commits — one per evolog entry, ordered oldest-to-newest. Each commit in the
/// chain has the tree and description from the corresponding evolog entry. The
/// original commit is left untouched.
///
/// This lets you "explode" a change's edit history into a visible commit chain.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct ExplodeArgs {
    /// The revision to explode
    #[arg(long, short, default_value = "@")]
    #[arg(add = ArgValueCompleter::new(complete::revset_expression_all))]
    revision: RevisionArg,
}

#[instrument(skip_all)]
pub(crate) async fn cmd_explode(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &ExplodeArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper(ui)?;
    let commit = workspace_command.resolve_single_rev(ui, &args.revision)?;

    if commit.id() == workspace_command.repo().store().root_commit_id() {
        return Err(user_error("Cannot explode the root commit"));
    }

    let repo = workspace_command.repo();
    let entries: Vec<_> = walk_predecessors(repo, &[commit.id().clone()])
        .try_collect()?;

    if entries.len() <= 1 {
        writeln!(ui.status(), "Nothing to explode.")?;
        return Ok(());
    }

    // walk_predecessors returns newest-first; reverse to oldest-first
    let entries: Vec<_> = entries.into_iter().rev().collect();

    let mut tx = workspace_command.start_transaction();
    let mut parent_ids = commit.parent_ids().to_vec();

    let num_commits = entries.len();
    for entry in &entries {
        let new_commit = tx
            .repo_mut()
            .new_commit(parent_ids, entry.commit.tree())
            .set_description(entry.commit.description())
            .generate_new_change_id()
            .write()
            .await?;
        parent_ids = vec![new_commit.id().clone()];
        if let Some(mut formatter) = ui.status_formatter() {
            write!(formatter, "Created ")?;
            tx.write_commit_summary(formatter.as_mut(), &new_commit)?;
            writeln!(formatter)?;
        }
    }

    tx.finish(ui, format!("explode {num_commits} commit(s) from evolution history"))?;
    Ok(())
}
