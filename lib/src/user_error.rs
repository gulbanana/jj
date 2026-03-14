// Copyright 2022-2024 The Jujutsu Authors
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

use std::error;
use std::io;
use std::iter;
use std::sync::Arc;

use itertools::Itertools as _;
use jj_lib::absorb::AbsorbError;
use jj_lib::backend::BackendError;
use jj_lib::backend::CommitId;
use jj_lib::bisect::BisectionError;
use jj_lib::config::ConfigFileSaveError;
use jj_lib::config::ConfigGetError;
use jj_lib::config::ConfigLoadError;
use jj_lib::config::ConfigMigrateError;
use jj_lib::evolution::WalkPredecessorsError;
use jj_lib::fileset::FilePatternParseError;
use jj_lib::fileset::FilesetParseError;
use jj_lib::fileset::FilesetParseErrorKind;
use jj_lib::fix::FixError;
use jj_lib::formatter::FormatRecorder;
use jj_lib::formatter::Formatter;
use jj_lib::gitignore::GitIgnoreError;
use jj_lib::index::IndexError;
use jj_lib::op_heads_store::OpHeadResolutionError;
use jj_lib::op_heads_store::OpHeadsStoreError;
use jj_lib::op_store::OpStoreError;
use jj_lib::op_store::OperationId;
use jj_lib::op_walk::OpsetEvaluationError;
use jj_lib::op_walk::OpsetResolutionError;
use jj_lib::repo::CheckOutCommitError;
use jj_lib::repo::EditCommitError;
use jj_lib::repo::RepoLoaderError;
use jj_lib::repo::RewriteRootCommit;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::repo_path::UiPathParseError;
use jj_lib::revset::RevsetEvaluationError;
use jj_lib::revset::RevsetParseError;
use jj_lib::revset::RevsetParseErrorKind;
use jj_lib::revset::RevsetResolutionError;
use jj_lib::revset_util::UserRevsetEvaluationError;
use jj_lib::secure_config::SecureConfigError;
use jj_lib::str_util::StringPatternParseError;
use jj_lib::trailer::TrailerParseError;
use jj_lib::transaction::TransactionCommitError;
use jj_lib::view::RenameWorkspaceError;
use jj_lib::working_copy::RecoverWorkspaceError;
use jj_lib::working_copy::ResetError;
use jj_lib::working_copy::SnapshotError;
use jj_lib::working_copy::WorkingCopyStateError;
use jj_lib::workspace::WorkspaceInitError;
use jj_lib::workspace_store::WorkspaceStoreError;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserErrorKind {
    User,
    Config,
    /// Invalid command line. The inner error type may be `clap::Error`.
    Cli,
    BrokenPipe,
    Internal,
}

#[derive(Clone, Debug)]
pub struct UserError {
    pub kind: UserErrorKind,
    pub error: Arc<dyn error::Error + Send + Sync>,
    pub hints: Vec<ErrorHint>,
}

impl UserError {
    pub fn new(kind: UserErrorKind, err: impl Into<Box<dyn error::Error + Send + Sync>>) -> Self {
        Self {
            kind,
            error: Arc::from(err.into()),
            hints: vec![],
        }
    }

    pub fn with_message(
        kind: UserErrorKind,
        message: impl Into<String>,
        source: impl Into<Box<dyn error::Error + Send + Sync>>,
    ) -> Self {
        Self::new(kind, ErrorWithMessage::new(message, source))
    }

    /// Returns error with the given plain-text `hint` attached.
    pub fn hinted(mut self, hint: impl Into<String>) -> Self {
        self.add_hint(hint);
        self
    }

    /// Appends plain-text `hint` to the error.
    pub fn add_hint(&mut self, hint: impl Into<String>) {
        self.hints.push(ErrorHint::PlainText(hint.into()));
    }

    /// Appends formatted `hint` to the error.
    pub fn add_formatted_hint(&mut self, hint: FormatRecorder) {
        self.hints.push(ErrorHint::Formatted(hint));
    }

    /// Constructs formatted hint and appends it to the error.
    pub fn add_formatted_hint_with(
        &mut self,
        write: impl FnOnce(&mut dyn Formatter) -> io::Result<()>,
    ) {
        let mut formatter = FormatRecorder::new(true);
        write(&mut formatter).expect("write() to FormatRecorder should never fail");
        self.add_formatted_hint(formatter);
    }

    /// Appends 0 or more plain-text `hints` to the error.
    pub fn extend_hints(&mut self, hints: impl IntoIterator<Item = String>) {
        self.hints
            .extend(hints.into_iter().map(ErrorHint::PlainText));
    }
}

#[derive(Clone, Debug)]
pub enum ErrorHint {
    PlainText(String),
    Formatted(FormatRecorder),
}

/// Wraps error with user-visible message.
#[derive(Debug, Error)]
#[error("{message}")]
struct ErrorWithMessage {
    message: String,
    source: Box<dyn error::Error + Send + Sync>,
}

impl ErrorWithMessage {
    fn new(
        message: impl Into<String>,
        source: impl Into<Box<dyn error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            message: message.into(),
            source: source.into(),
        }
    }
}

pub fn user_error(err: impl Into<Box<dyn error::Error + Send + Sync>>) -> UserError {
    UserError::new(UserErrorKind::User, err)
}

pub fn user_error_with_message(
    message: impl Into<String>,
    source: impl Into<Box<dyn error::Error + Send + Sync>>,
) -> UserError {
    UserError::with_message(UserErrorKind::User, message, source)
}

pub fn config_error(err: impl Into<Box<dyn error::Error + Send + Sync>>) -> UserError {
    UserError::new(UserErrorKind::Config, err)
}

pub fn config_error_with_message(
    message: impl Into<String>,
    source: impl Into<Box<dyn error::Error + Send + Sync>>,
) -> UserError {
    UserError::with_message(UserErrorKind::Config, message, source)
}

pub fn internal_error(err: impl Into<Box<dyn error::Error + Send + Sync>>) -> UserError {
    UserError::new(UserErrorKind::Internal, err)
}

pub fn internal_error_with_message(
    message: impl Into<String>,
    source: impl Into<Box<dyn error::Error + Send + Sync>>,
) -> UserError {
    UserError::with_message(UserErrorKind::Internal, message, source)
}

pub fn format_similarity_hint<S: AsRef<str>>(candidates: &[S]) -> Option<String> {
    match candidates {
        [] => None,
        names => {
            let quoted_names = names.iter().map(|s| format!("`{}`", s.as_ref())).join(", ");
            Some(format!("Did you mean {quoted_names}?"))
        }
    }
}

impl From<io::Error> for UserError {
    fn from(err: io::Error) -> Self {
        let kind = match err.kind() {
            io::ErrorKind::BrokenPipe => UserErrorKind::BrokenPipe,
            _ => UserErrorKind::User,
        };
        Self::new(kind, err)
    }
}

impl From<jj_lib::file_util::PathError> for UserError {
    fn from(err: jj_lib::file_util::PathError) -> Self {
        user_error(err)
    }
}

impl From<ConfigFileSaveError> for UserError {
    fn from(err: ConfigFileSaveError) -> Self {
        user_error(err)
    }
}

impl From<ConfigGetError> for UserError {
    fn from(err: ConfigGetError) -> Self {
        let hint = config_get_error_hint(&err);
        let mut cmd_err = config_error(err);
        cmd_err.extend_hints(hint);
        cmd_err
    }
}

impl From<ConfigLoadError> for UserError {
    fn from(err: ConfigLoadError) -> Self {
        let hint = match &err {
            ConfigLoadError::Read(_) => None,
            ConfigLoadError::Parse { source_path, .. } => source_path
                .as_ref()
                .map(|path| format!("Check the config file: {}", path.display())),
        };
        let mut cmd_err = config_error(err);
        cmd_err.extend_hints(hint);
        cmd_err
    }
}

impl From<ConfigMigrateError> for UserError {
    fn from(err: ConfigMigrateError) -> Self {
        let hint = err
            .source_path
            .as_ref()
            .map(|path| format!("Check the config file: {}", path.display()));
        let mut cmd_err = config_error(err);
        cmd_err.extend_hints(hint);
        cmd_err
    }
}

impl From<RewriteRootCommit> for UserError {
    fn from(err: RewriteRootCommit) -> Self {
        internal_error_with_message("Attempted to rewrite the root commit", err)
    }
}

impl From<EditCommitError> for UserError {
    fn from(err: EditCommitError) -> Self {
        internal_error_with_message("Failed to edit a commit", err)
    }
}

impl From<CheckOutCommitError> for UserError {
    fn from(err: CheckOutCommitError) -> Self {
        internal_error_with_message("Failed to check out a commit", err)
    }
}

impl From<RenameWorkspaceError> for UserError {
    fn from(err: RenameWorkspaceError) -> Self {
        user_error_with_message("Failed to rename a workspace", err)
    }
}

impl From<BackendError> for UserError {
    fn from(err: BackendError) -> Self {
        match &err {
            BackendError::Unsupported(_) => user_error(err),
            _ => internal_error_with_message("Unexpected error from backend", err),
        }
    }
}

impl From<IndexError> for UserError {
    fn from(err: IndexError) -> Self {
        internal_error_with_message("Unexpected error from index", err)
    }
}

impl From<OpHeadsStoreError> for UserError {
    fn from(err: OpHeadsStoreError) -> Self {
        internal_error_with_message("Unexpected error from operation heads store", err)
    }
}

impl From<WorkspaceStoreError> for UserError {
    fn from(err: WorkspaceStoreError) -> Self {
        internal_error_with_message("Unexpected error from workspace store", err)
    }
}

impl From<WorkspaceInitError> for UserError {
    fn from(err: WorkspaceInitError) -> Self {
        match err {
            WorkspaceInitError::DestinationExists(_) => {
                user_error("The target repo already exists")
            }
            WorkspaceInitError::EncodeRepoPath(_) => user_error(err),
            WorkspaceInitError::CheckOutCommit(err) => {
                internal_error_with_message("Failed to check out the initial commit", err)
            }
            WorkspaceInitError::Path(err) => {
                internal_error_with_message("Failed to access the repository", err)
            }
            WorkspaceInitError::OpHeadsStore(err) => {
                user_error_with_message("Failed to record initial operation", err)
            }
            WorkspaceInitError::WorkspaceStore(err) => {
                internal_error_with_message("Failed to record workspace path", err)
            }
            WorkspaceInitError::Backend(err) => {
                user_error_with_message("Failed to access the repository", err)
            }
            WorkspaceInitError::WorkingCopyState(err) => {
                internal_error_with_message("Failed to access the repository", err)
            }
            WorkspaceInitError::SignInit(err) => user_error(err),
            WorkspaceInitError::TransactionCommit(err) => err.into(),
        }
    }
}

impl From<OpHeadResolutionError> for UserError {
    fn from(err: OpHeadResolutionError) -> Self {
        match err {
            OpHeadResolutionError::NoHeads => {
                internal_error_with_message("Corrupt repository", err)
            }
        }
    }
}

impl From<OpsetEvaluationError> for UserError {
    fn from(err: OpsetEvaluationError) -> Self {
        match err {
            OpsetEvaluationError::OpsetResolution(err) => {
                let hint = opset_resolution_error_hint(&err);
                let mut cmd_err = user_error(err);
                cmd_err.extend_hints(hint);
                cmd_err
            }
            OpsetEvaluationError::OpHeadResolution(err) => err.into(),
            OpsetEvaluationError::OpHeadsStore(err) => err.into(),
            OpsetEvaluationError::OpStore(err) => err.into(),
        }
    }
}

impl From<SnapshotError> for UserError {
    fn from(err: SnapshotError) -> Self {
        internal_error_with_message("Failed to snapshot the working copy", err)
    }
}

impl From<OpStoreError> for UserError {
    fn from(err: OpStoreError) -> Self {
        internal_error_with_message("Failed to load an operation", err)
    }
}

impl From<RepoLoaderError> for UserError {
    fn from(err: RepoLoaderError) -> Self {
        internal_error_with_message("Failed to load the repo", err)
    }
}

impl From<ResetError> for UserError {
    fn from(err: ResetError) -> Self {
        internal_error_with_message("Failed to reset the working copy", err)
    }
}

impl From<TransactionCommitError> for UserError {
    fn from(err: TransactionCommitError) -> Self {
        internal_error(err)
    }
}

impl From<WalkPredecessorsError> for UserError {
    fn from(err: WalkPredecessorsError) -> Self {
        match err {
            WalkPredecessorsError::Backend(err) => err.into(),
            WalkPredecessorsError::Index(err) => err.into(),
            WalkPredecessorsError::OpStore(err) => err.into(),
            WalkPredecessorsError::CycleDetected(_) => internal_error(err),
        }
    }
}

impl From<TrailerParseError> for UserError {
    fn from(err: TrailerParseError) -> Self {
        user_error(err)
    }
}

impl From<UserRevsetEvaluationError> for UserError {
    fn from(err: UserRevsetEvaluationError) -> Self {
        match err {
            UserRevsetEvaluationError::Resolution(err) => err.into(),
            UserRevsetEvaluationError::Evaluation(err) => err.into(),
        }
    }
}

#[cfg(feature = "git")]
mod git {
    use jj_lib::git::GitDefaultRefspecError;
    use jj_lib::git::GitExportError;
    use jj_lib::git::GitFetchError;
    use jj_lib::git::GitImportError;
    use jj_lib::git::GitPushError;
    use jj_lib::git::GitRefExpansionError;
    use jj_lib::git::GitRemoteManagementError;
    use jj_lib::git::GitResetHeadError;
    use jj_lib::git::UnexpectedGitBackendError;

    use super::*;

    impl From<GitImportError> for UserError {
        fn from(err: GitImportError) -> Self {
            let hint = match &err {
                GitImportError::MissingHeadTarget { .. }
                | GitImportError::MissingRefAncestor { .. } => Some(
                    "\
Is this Git repository a partial clone (cloned with the --filter argument)?
jj currently does not support partial clones. To use jj with this repository, try re-cloning with \
                     the full repository contents."
                        .to_string(),
                ),
                GitImportError::Backend(_) => None,
                GitImportError::Index(_) => None,
                GitImportError::Git(_) => None,
                GitImportError::UnexpectedBackend(_) => None,
            };
            let mut cmd_err =
                user_error_with_message("Failed to import refs from underlying Git repo", err);
            cmd_err.extend_hints(hint);
            cmd_err
        }
    }

    impl From<GitExportError> for UserError {
        fn from(err: GitExportError) -> Self {
            user_error_with_message("Failed to export refs to underlying Git repo", err)
        }
    }

    impl From<GitFetchError> for UserError {
        fn from(err: GitFetchError) -> Self {
            match err {
                GitFetchError::NoSuchRemote(_) => user_error(err),
                GitFetchError::RemoteName(_) => {
                    user_error(err).hinted("Run `jj git remote rename` to give a different name.")
                }
                GitFetchError::RejectedUpdates(_) | GitFetchError::Subprocess(_) => user_error(err),
            }
        }
    }

    impl From<GitDefaultRefspecError> for UserError {
        fn from(err: GitDefaultRefspecError) -> Self {
            match err {
                GitDefaultRefspecError::NoSuchRemote(_) => user_error(err),
                GitDefaultRefspecError::InvalidRemoteConfiguration(_, _) => user_error(err),
            }
        }
    }

    impl From<GitRefExpansionError> for UserError {
        fn from(err: GitRefExpansionError) -> Self {
            match &err {
                GitRefExpansionError::Expression(_) => user_error(err)
                    .hinted("Specify patterns in `(positive | ...) & ~(negative | ...)` form."),
                GitRefExpansionError::InvalidBranchPattern(_) => user_error(err),
            }
        }
    }

    impl From<GitPushError> for UserError {
        fn from(err: GitPushError) -> Self {
            match err {
                GitPushError::NoSuchRemote(_) => user_error(err),
                GitPushError::RemoteName(_) => {
                    user_error(err).hinted("Run `jj git remote rename` to give a different name.")
                }
                GitPushError::Subprocess(_) => user_error(err),
                GitPushError::UnexpectedBackend(_) => user_error(err),
            }
        }
    }

    impl From<GitRemoteManagementError> for UserError {
        fn from(err: GitRemoteManagementError) -> Self {
            user_error(err)
        }
    }

    impl From<GitResetHeadError> for UserError {
        fn from(err: GitResetHeadError) -> Self {
            user_error_with_message("Failed to reset Git HEAD state", err)
        }
    }

    impl From<UnexpectedGitBackendError> for UserError {
        fn from(err: UnexpectedGitBackendError) -> Self {
            user_error(err)
        }
    }
}

impl From<RevsetEvaluationError> for UserError {
    fn from(err: RevsetEvaluationError) -> Self {
        user_error(err)
    }
}

impl From<FilesetParseError> for UserError {
    fn from(err: FilesetParseError) -> Self {
        let hint = fileset_parse_error_hint(&err);
        let mut cmd_err =
            user_error_with_message(format!("Failed to parse fileset: {}", err.kind()), err);
        cmd_err.extend_hints(hint);
        cmd_err
    }
}

impl From<RecoverWorkspaceError> for UserError {
    fn from(err: RecoverWorkspaceError) -> Self {
        match err {
            RecoverWorkspaceError::Backend(err) => err.into(),
            RecoverWorkspaceError::Reset(err) => err.into(),
            RecoverWorkspaceError::RewriteRootCommit(err) => err.into(),
            RecoverWorkspaceError::TransactionCommit(err) => err.into(),
            err @ RecoverWorkspaceError::WorkspaceMissingWorkingCopy(_) => user_error(err),
        }
    }
}

impl From<RevsetParseError> for UserError {
    fn from(err: RevsetParseError) -> Self {
        let hint = revset_parse_error_hint(&err);
        let mut cmd_err =
            user_error_with_message(format!("Failed to parse revset: {}", err.kind()), err);
        cmd_err.extend_hints(hint);
        cmd_err
    }
}

impl From<RevsetResolutionError> for UserError {
    fn from(err: RevsetResolutionError) -> Self {
        let hints = revset_resolution_error_hints(&err);
        let mut cmd_err = user_error(err);
        cmd_err.extend_hints(hints);
        cmd_err
    }
}

impl From<UiPathParseError> for UserError {
    fn from(err: UiPathParseError) -> Self {
        user_error(err)
    }
}

impl From<WorkingCopyStateError> for UserError {
    fn from(err: WorkingCopyStateError) -> Self {
        internal_error_with_message("Failed to access working copy state", err)
    }
}

impl From<GitIgnoreError> for UserError {
    fn from(err: GitIgnoreError) -> Self {
        user_error_with_message("Failed to process .gitignore.", err)
    }
}

impl From<AbsorbError> for UserError {
    fn from(err: AbsorbError) -> Self {
        match err {
            AbsorbError::Backend(err) => err.into(),
            AbsorbError::RevsetEvaluation(err) => err.into(),
        }
    }
}

impl From<FixError> for UserError {
    fn from(err: FixError) -> Self {
        match err {
            FixError::Backend(err) => err.into(),
            FixError::RevsetEvaluation(err) => err.into(),
            FixError::Io(err) => err.into(),
            FixError::FixContent(err) => internal_error_with_message(
                "An error occurred while attempting to fix file content",
                err,
            ),
        }
    }
}

impl From<BisectionError> for UserError {
    fn from(err: BisectionError) -> Self {
        match err {
            BisectionError::RevsetEvaluationError(_) => user_error(err),
        }
    }
}

impl From<SecureConfigError> for UserError {
    fn from(err: SecureConfigError) -> Self {
        internal_error_with_message("Failed to determine the secure config for a repo", err)
    }
}

/// handles lib-level error types, which never contain cli-level types
pub fn find_source_parse_error_hint(err: &dyn error::Error) -> Option<String> {
    let source = err.source()?;
    if let Some(source) = source.downcast_ref() {
        config_get_error_hint(source)
    } else if let Some(source) = source.downcast_ref() {
        file_pattern_parse_error_hint(source)
    } else if let Some(source) = source.downcast_ref() {
        fileset_parse_error_hint(source)
    } else if let Some(source) = source.downcast_ref() {
        revset_parse_error_hint(source)
    } else if let Some(source) = source.downcast_ref() {
        // TODO: propagate all hints?
        revset_resolution_error_hints(source).into_iter().next()
    } else if let Some(source) = source.downcast_ref() {
        string_pattern_parse_error_hint(source)
    } else {
        None
    }
}

fn config_get_error_hint(err: &ConfigGetError) -> Option<String> {
    match &err {
        ConfigGetError::NotFound { .. } => None,
        ConfigGetError::Type { source_path, .. } => source_path
            .as_ref()
            .map(|path| format!("Check the config file: {}", path.display())),
    }
}

fn file_pattern_parse_error_hint(err: &FilePatternParseError) -> Option<String> {
    match err {
        FilePatternParseError::InvalidKind(_) => Some(String::from(
            "See https://docs.jj-vcs.dev/latest/filesets/#file-patterns or `jj help -k filesets` \
             for valid prefixes.",
        )),
        // Suggest root:"<path>" if input can be parsed as repo-relative path
        FilePatternParseError::UiPath(UiPathParseError::Fs(e)) => {
            RepoPathBuf::from_relative_path(&e.input).ok().map(|path| {
                format!(r#"Consider using root:{path:?} to specify repo-relative path"#)
            })
        }
        FilePatternParseError::RelativePath(_) => None,
        FilePatternParseError::GlobPattern(_) => None,
    }
}

fn fileset_parse_error_hint(err: &FilesetParseError) -> Option<String> {
    match err.kind() {
        FilesetParseErrorKind::SyntaxError => Some(String::from(
            "See https://docs.jj-vcs.dev/latest/filesets/ or use `jj help -k filesets` for \
             filesets syntax and how to match file paths.",
        )),
        FilesetParseErrorKind::NoSuchFunction {
            name: _,
            candidates,
        } => format_similarity_hint(candidates),
        FilesetParseErrorKind::InvalidArguments { .. } => find_source_parse_error_hint(&err),
        FilesetParseErrorKind::RedefinedFunctionParameter => None,
        FilesetParseErrorKind::Expression(_) => find_source_parse_error_hint(&err),
        FilesetParseErrorKind::InAliasExpansion(_)
        | FilesetParseErrorKind::InParameterExpansion(_)
        | FilesetParseErrorKind::RecursiveAlias(_) => None,
    }
}

fn opset_resolution_error_hint(err: &OpsetResolutionError) -> Option<String> {
    match err {
        OpsetResolutionError::MultipleOperations {
            expr: _,
            candidates,
        } => Some(format!(
            "Try specifying one of the operations by ID: {}",
            candidates.iter().map(short_operation_hash).join(", ")
        )),
        OpsetResolutionError::EmptyOperations(_)
        | OpsetResolutionError::InvalidIdPrefix(_)
        | OpsetResolutionError::NoSuchOperation(_)
        | OpsetResolutionError::AmbiguousIdPrefix(_) => None,
    }
}

// TODO: find another way for jj_cli::revset_util to reuse this code
pub fn revset_parse_error_hint(err: &RevsetParseError) -> Option<String> {
    // Only for the bottom error, which is usually the root cause
    let bottom_err = iter::successors(Some(err), |e| e.origin()).last().unwrap();
    match bottom_err.kind() {
        RevsetParseErrorKind::SyntaxError => Some(
            "See https://docs.jj-vcs.dev/latest/revsets/ or use `jj help -k revsets` for revsets \
             syntax and how to quote symbols."
                .into(),
        ),
        RevsetParseErrorKind::NotPrefixOperator {
            op: _,
            similar_op,
            description,
        }
        | RevsetParseErrorKind::NotPostfixOperator {
            op: _,
            similar_op,
            description,
        }
        | RevsetParseErrorKind::NotInfixOperator {
            op: _,
            similar_op,
            description,
        } => Some(format!("Did you mean `{similar_op}` for {description}?")),
        RevsetParseErrorKind::NoSuchFunction {
            name: _,
            candidates,
        } => format_similarity_hint(candidates),
        RevsetParseErrorKind::InvalidFunctionArguments { .. }
        | RevsetParseErrorKind::Expression(_) => find_source_parse_error_hint(bottom_err),
        _ => None,
    }
}

pub fn revset_resolution_error_hints(err: &RevsetResolutionError) -> Vec<String> {
    let multiple_targets_hint = |targets: &[CommitId]| {
        format!(
            "Use commit ID to select single revision from: {}",
            targets.iter().map(|id| format!("{id:.12}")).join(", ")
        )
    };
    match err {
        RevsetResolutionError::NoSuchRevision {
            name: _,
            candidates,
        } => format_similarity_hint(candidates).into_iter().collect(),
        RevsetResolutionError::DivergentChangeId {
            symbol,
            visible_targets,
        } => vec![
            format!(
                "Use change offset to select single revision: {}",
                visible_targets
                    .iter()
                    .map(|(offset, _)| format!("{symbol}/{offset}"))
                    .join(", ")
            ),
            format!("Use `change_id({symbol})` to select all revisions"),
            "To abandon unneeded revisions, run `jj abandon <commit_id>`".to_owned(),
        ],
        RevsetResolutionError::ConflictedRef {
            kind: "bookmark",
            symbol,
            targets,
        } => vec![
            multiple_targets_hint(targets),
            format!("Use `bookmarks({symbol})` to select all revisions"),
            format!(
                "To set which revision the bookmark points to, run `jj bookmark set {symbol} -r \
                 <REVISION>`"
            ),
        ],
        RevsetResolutionError::ConflictedRef {
            kind: _,
            symbol: _,
            targets,
        } => vec![multiple_targets_hint(targets)],
        RevsetResolutionError::EmptyString
        | RevsetResolutionError::WorkspaceMissingWorkingCopy { .. }
        | RevsetResolutionError::AmbiguousCommitIdPrefix(_)
        | RevsetResolutionError::AmbiguousChangeIdPrefix(_)
        | RevsetResolutionError::Backend(_)
        | RevsetResolutionError::Other(_) => vec![],
    }
}

fn string_pattern_parse_error_hint(err: &StringPatternParseError) -> Option<String> {
    match err {
        StringPatternParseError::InvalidKind(_) => Some(
            "Try prefixing with one of `exact:`, `glob:`, `regex:`, `substring:`, or one of these \
             with `-i` suffix added (e.g. `glob-i:`) for case-insensitive matching"
                .into(),
        ),
        StringPatternParseError::GlobPattern(_) | StringPatternParseError::Regex(_) => None,
    }
}

// TODO: remove these duplicate functions

pub fn short_operation_hash(operation_id: &OperationId) -> String {
    format!("{operation_id:.12}")
}
