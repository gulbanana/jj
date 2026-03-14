// Copyright 2025 The Jujutsu Authors
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

//! Temporary location for code extracted from cli_util

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;

use chrono::TimeZone as _;
use jj_lib::backend::CommitId;
use jj_lib::config::ConfigGetResultExt as _;
use jj_lib::config::ConfigNamePathBuf;
use jj_lib::config::StackedConfig;
use jj_lib::conflicts::ConflictMarkerStyle;
use jj_lib::dsl_util::load_aliases_map;
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::fileset::FilesetParseContext;
use jj_lib::id_prefix::IdPrefixContext;
use jj_lib::ref_name::RemoteName;
use jj_lib::ref_name::WorkspaceName;
use jj_lib::ref_name::WorkspaceNameBuf;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPathUiConverter;
use jj_lib::revset;
use jj_lib::revset::ResolvedRevsetExpression;
use jj_lib::revset::RevsetAliasesMap;
use jj_lib::revset::RevsetDiagnostics;
use jj_lib::revset::RevsetExpression;
use jj_lib::revset::RevsetExtensions;
use jj_lib::revset::RevsetParseContext;
use jj_lib::revset::RevsetWorkspaceContext;
use jj_lib::revset::UserRevsetExpression;
use jj_lib::revset_util;
use jj_lib::revset_util::RevsetExpressionEvaluator;
use jj_lib::settings::UserSettings;
use jj_lib::store::Store;
use jj_lib::user_error::UserError;
use jj_lib::user_error::config_error_with_message;
use jj_lib::workspace::Workspace;
use tracing::instrument;

// TODO: Some fields are pub due to the vestigial WorkspaceCommandEnvironment.

/// Metadata and configuration loaded for a specific workspace.
pub struct WorkspaceEnvironment {
    settings: UserSettings,
    fileset_aliases_map: FilesetAliasesMap,
    revset_aliases_map: RevsetAliasesMap,
    revset_extensions: Arc<RevsetExtensions>,
    default_ignored_remote: Option<&'static RemoteName>,
    revsets_use_glob_by_default: bool,
    pub path_converter: RepoPathUiConverter,
    pub workspace_name: WorkspaceNameBuf,
    immutable_heads_expression: Arc<UserRevsetExpression>,
    short_prefixes_expression: Option<Arc<UserRevsetExpression>>,
    pub conflict_marker_style: ConflictMarkerStyle,
}

impl WorkspaceEnvironment {
    #[instrument(skip_all)]
    pub fn new(
        workspace: &Workspace,
        cwd: PathBuf,
        revset_extensions: Arc<RevsetExtensions>,
        mut warn: impl FnMut(fmt::Arguments<'_>) -> io::Result<()>,
    ) -> Result<Self, UserError> {
        let settings = workspace.settings();
        let fileset_aliases_map = load_fileset_aliases(settings.config(), &mut warn)?;
        let revset_aliases_map = load_revset_aliases(settings.config(), &mut warn)?;
        let default_ignored_remote = default_ignored_remote_name(workspace.repo_loader().store());
        let path_converter = RepoPathUiConverter::Fs {
            cwd: cwd,
            base: workspace.workspace_root().to_owned(),
        };
        let env = Self {
            settings: settings.clone(),
            fileset_aliases_map,
            revset_aliases_map,
            revset_extensions,
            default_ignored_remote,
            revsets_use_glob_by_default: settings.get("ui.revsets-use-glob-by-default")?,
            path_converter,
            workspace_name: workspace.workspace_name().to_owned(),
            immutable_heads_expression: RevsetExpression::root(),
            short_prefixes_expression: None,
            conflict_marker_style: settings.get("ui.conflict-marker-style")?,
        };
        Ok(env)
    }

    pub fn path_converter(&self) -> &RepoPathUiConverter {
        &self.path_converter
    }

    pub fn workspace_name(&self) -> &WorkspaceName {
        &self.workspace_name
    }

    pub fn revset_aliases_map(&mut self) -> &mut RevsetAliasesMap {
        &mut self.revset_aliases_map
    }

    pub fn revset_extensions(&self) -> &Arc<RevsetExtensions> {
        &self.revset_extensions
    }

    /// Parsing context for fileset expressions specified by command arguments.
    pub fn fileset_parse_context(&self) -> FilesetParseContext<'_> {
        FilesetParseContext {
            aliases_map: &self.fileset_aliases_map,
            path_converter: &self.path_converter,
        }
    }

    /// Parsing context for fileset expressions loaded from config files.
    pub fn fileset_parse_context_for_config(&self) -> FilesetParseContext<'_> {
        // TODO: bump MSRV to 1.91.0 to leverage const PathBuf::new()
        static ROOT_PATH_CONVERTER: LazyLock<RepoPathUiConverter> =
            LazyLock::new(|| RepoPathUiConverter::Fs {
                cwd: PathBuf::new(),
                base: PathBuf::new(),
            });
        FilesetParseContext {
            aliases_map: &self.fileset_aliases_map,
            path_converter: &ROOT_PATH_CONVERTER,
        }
    }

    pub fn revset_parse_context(&self) -> RevsetParseContext<'_> {
        let workspace_context = RevsetWorkspaceContext {
            path_converter: &self.path_converter,
            workspace_name: &self.workspace_name,
        };
        let now = if let Some(timestamp) = self.settings.commit_timestamp() {
            chrono::Local
                .timestamp_millis_opt(timestamp.timestamp.0)
                .unwrap()
        } else {
            chrono::Local::now()
        };
        RevsetParseContext {
            aliases_map: &self.revset_aliases_map,
            local_variables: HashMap::new(),
            user_email: self.settings.user_email(),
            date_pattern_context: now.into(),
            default_ignored_remote: self.default_ignored_remote,
            fileset_aliases_map: &self.fileset_aliases_map,
            use_glob_by_default: self.revsets_use_glob_by_default,
            extensions: self.revset_extensions(),
            workspace: Some(workspace_context),
        }
    }

    /// Creates fresh new context which manages cache of short commit/change ID
    /// prefixes. New context should be created per repo view (or operation.)
    pub fn new_id_prefix_context(&self) -> IdPrefixContext {
        let context = IdPrefixContext::new(self.revset_extensions().clone());
        match &self.short_prefixes_expression {
            None => context,
            Some(expression) => context.disambiguate_within(expression.clone()),
        }
    }

    /// Updates parsed revset expressions.
    pub fn reload_revset_expressions(
        &mut self,
        immutable_heads_diagnostics: &mut RevsetDiagnostics,
        short_prefixes_diagnostics: &mut RevsetDiagnostics,
    ) -> Result<(), UserError> {
        self.immutable_heads_expression =
            self.load_immutable_heads_expression(immutable_heads_diagnostics)?;
        self.short_prefixes_expression =
            self.load_short_prefixes_expression(short_prefixes_diagnostics)?;
        Ok(())
    }

    /// User-configured expression defining the immutable set.
    pub fn immutable_expression(&self) -> Arc<UserRevsetExpression> {
        // Negated ancestors expression `~::(<heads> | root())` is slightly
        // easier to optimize than negated union `~(::<heads> | root())`.
        self.immutable_heads_expression.ancestors()
    }

    /// User-configured expression defining the heads of the immutable set.
    pub fn immutable_heads_expression(&self) -> &Arc<UserRevsetExpression> {
        &self.immutable_heads_expression
    }

    /// User-configured conflict marker style for materializing conflicts
    pub fn conflict_marker_style(&self) -> ConflictMarkerStyle {
        self.conflict_marker_style
    }

    fn load_immutable_heads_expression(
        &self,
        diagnostics: &mut RevsetDiagnostics,
    ) -> Result<Arc<UserRevsetExpression>, UserError> {
        let expression = jj_lib::revset_util::parse_immutable_heads_expression(
            diagnostics,
            &self.revset_parse_context(),
        )
        .map_err(|e| config_error_with_message("Invalid `revset-aliases.immutable_heads()`", e))?;
        Ok(expression)
    }

    fn load_short_prefixes_expression(
        &self,
        diagnostics: &mut RevsetDiagnostics,
    ) -> Result<Option<Arc<UserRevsetExpression>>, UserError> {
        let revset_string = self
            .settings
            .get_string("revsets.short-prefixes")
            .optional()?
            .map_or_else(|| self.settings.get_string("revsets.log"), Ok)?;
        if revset_string.is_empty() {
            Ok(None)
        } else {
            let expression =
                revset::parse(diagnostics, &revset_string, &self.revset_parse_context()).map_err(
                    |err| config_error_with_message("Invalid `revsets.short-prefixes`", err),
                )?;
            Ok(Some(expression))
        }
    }

    /// Returns first immutable commit.
    pub fn find_immutable_commit(
        &self,
        repo: &dyn Repo,
        to_rewrite_expr: &Arc<ResolvedRevsetExpression>,
        ignore_immutable: bool,
    ) -> Result<Option<CommitId>, UserError> {
        let immutable_expression = if ignore_immutable {
            UserRevsetExpression::root()
        } else {
            self.immutable_expression()
        };

        // Not using self.id_prefix_context() because the disambiguation data
        // must not be calculated and cached against arbitrary repo. It's also
        // unlikely that the immutable expression contains short hashes.
        let id_prefix_context = IdPrefixContext::new(self.revset_extensions().clone());
        let immutable_expr = RevsetExpressionEvaluator::new(
            repo,
            self.revset_extensions().clone(),
            &id_prefix_context,
            immutable_expression,
        )
        .resolve()
        .map_err(|e| config_error_with_message("Invalid `revset-aliases.immutable_heads()`", e))?;

        let mut commit_id_iter = immutable_expr
            .intersection(to_rewrite_expr)
            .evaluate(repo)?
            .iter();
        Ok(commit_id_iter.next().transpose()?)
    }
}

/// Returns the special remote name that should be ignored by default.
#[cfg_attr(not(feature = "git"), expect(unused_variables))]
pub fn default_ignored_remote_name(store: &Store) -> Option<&'static RemoteName> {
    #[cfg(feature = "git")]
    {
        use jj_lib::git;
        if git::get_git_backend(store).is_ok() {
            return Some(git::REMOTE_NAME_FOR_LOCAL_GIT_REPO);
        }
    }
    None
}

pub fn load_fileset_aliases(
    config: &StackedConfig,
    warn: &mut impl FnMut(fmt::Arguments<'_>) -> io::Result<()>,
) -> Result<FilesetAliasesMap, UserError> {
    let table_name = ConfigNamePathBuf::from_iter(["fileset-aliases"]);
    load_aliases_map(config, &table_name, &mut *warn)
}

pub fn load_revset_aliases(
    config: &StackedConfig,
    warn: &mut impl FnMut(fmt::Arguments<'_>) -> io::Result<()>,
) -> Result<RevsetAliasesMap, UserError> {
    let table_name = ConfigNamePathBuf::from_iter(["revset-aliases"]);
    let aliases_map = load_aliases_map(config, &table_name, &mut *warn)?;
    revset_util::warn_user_redefined_builtin(config, &table_name, &mut *warn)?;
    Ok(aliases_map)
}
