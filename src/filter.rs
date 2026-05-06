use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{FastSyncError, Result, io_context};
use crate::i18n::{tr_path, tr_value};

/// 过滤规则的工作模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// 黑名单模式：匹配到的路径不参与同步。
    Exclude,
    /// 白名单模式：只有匹配到的路径参与同步。
    Include,
}

/// 用户通过 CLI 声明的过滤规则文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterConfig {
    pub mode: FilterMode,
    pub path: PathBuf,
}

/// 同步路径过滤器。
///
/// 过滤器只匹配同步根目录下的相对路径。黑名单模式保护匹配路径不被复制、
/// 覆盖、校验或删除；白名单模式把同步作用域限制为匹配路径。
#[derive(Debug, Clone, Default)]
pub struct PathFilter {
    mode: Option<FilterMode>,
    rules: Vec<FilterRule>,
}

impl PathFilter {
    /// 构造无过滤器，保持原有同步行为。
    pub fn disabled() -> Self {
        Self::default()
    }

    /// 从可选配置读取并编译规则文件。
    pub fn from_config(config: Option<&FilterConfig>) -> Result<Self> {
        let Some(config) = config else {
            return Ok(Self::disabled());
        };

        let content = io_context(
            tr_path("io.read_filter_file", config.path.display()),
            fs::read_to_string(&config.path),
        )?;
        Self::from_rules(config.mode, &content)
    }

    /// 从文本内容编译过滤器，主要供测试和上层配置转换使用。
    pub fn from_rules(mode: FilterMode, content: &str) -> Result<Self> {
        let mut rules = Vec::new();
        for (index, line) in content.lines().enumerate() {
            if let Some(rule) = FilterRule::parse(line, index + 1)? {
                rules.push(rule);
            }
        }

        Ok(Self {
            mode: Some(mode),
            rules,
        })
    }

    /// 判断路径是否进入同步语义。
    ///
    /// `is_dir` 用于让目录规则匹配目录本身及其子树。过滤外路径在比较、
    /// 复制、元数据同步、校验和删除阶段都应视为不可见。
    pub fn allows_entry(&self, path: &Path, is_dir: bool) -> bool {
        match self.mode {
            None => true,
            Some(FilterMode::Exclude) => !self.rule_matches(path, is_dir),
            Some(FilterMode::Include) => self.rule_matches(path, is_dir),
        }
    }

    /// 判断扫描器是否需要进入该目录。
    ///
    /// 白名单模式下，目录自身可以不参与同步，但只要可能存在匹配后代就必须
    /// 继续下探；黑名单模式下，命中的目录整棵子树都会被跳过。
    pub fn should_descend(&self, path: &Path) -> bool {
        match self.mode {
            None => true,
            Some(FilterMode::Exclude) => !self.rule_matches(path, true),
            Some(FilterMode::Include) => {
                self.rule_matches(path, true)
                    || self
                        .rules
                        .iter()
                        .any(|rule| rule.could_match_descendant(path))
            }
        }
    }

    fn rule_matches(&self, path: &Path, is_dir: bool) -> bool {
        self.rules.iter().any(|rule| rule.matches(path, is_dir))
    }
}

#[derive(Debug, Clone)]
struct FilterRule {
    components: Vec<String>,
    basename_only: bool,
    directory_only: bool,
}

impl FilterRule {
    fn parse(line: &str, line_number: usize) -> Result<Option<Self>> {
        let pattern = line.trim();
        if pattern.is_empty() || pattern.starts_with('#') {
            return Ok(None);
        }
        if pattern.starts_with('!') {
            return Err(invalid_rule(
                line_number,
                "negation rules are not supported",
            ));
        }

        let directory_only = pattern.ends_with('/');
        let trimmed = pattern.trim_matches('/');
        if trimmed.is_empty() {
            return Err(invalid_rule(line_number, "filter pattern cannot be empty"));
        }

        let mut components = Vec::new();
        for component in trimmed.split('/') {
            if component.is_empty() {
                continue;
            }
            if matches!(component, "." | "..") {
                return Err(invalid_rule(
                    line_number,
                    "filter pattern must stay inside the sync root",
                ));
            }
            components.push(component.to_string());
        }
        if components.is_empty() {
            return Err(invalid_rule(line_number, "filter pattern cannot be empty"));
        }

        Ok(Some(Self {
            basename_only: components.len() == 1 && !pattern.starts_with('/'),
            components,
            directory_only,
        }))
    }

    fn matches(&self, path: &Path, is_dir: bool) -> bool {
        let Some(path_components) = path_components(path) else {
            return false;
        };
        if path_components.is_empty() {
            return false;
        }

        if self.basename_only {
            return self.matches_basename_path(&path_components, is_dir);
        }

        self.matches_root_path_or_subtree(&path_components, is_dir)
    }

    fn matches_basename_path(&self, path_components: &[String], is_dir: bool) -> bool {
        let pattern = &self.components[0];
        for (index, component) in path_components.iter().enumerate() {
            let component_is_dir = index + 1 < path_components.len() || is_dir;
            if (!self.directory_only || component_is_dir)
                && glob_component_matches(pattern, component)
            {
                return true;
            }
        }
        false
    }

    fn matches_root_path_or_subtree(&self, path_components: &[String], is_dir: bool) -> bool {
        let exact_match = path_components_match(&self.components, path_components);
        if exact_match && (!self.directory_only || is_dir) {
            return true;
        }

        if path_components.len() <= 1 {
            return false;
        }

        for prefix_len in 1..path_components.len() {
            if path_components_match(&self.components, &path_components[..prefix_len]) {
                return true;
            }
        }
        false
    }

    fn could_match_descendant(&self, dir: &Path) -> bool {
        if self.basename_only {
            return true;
        }

        let Some(dir_components) = path_components(dir) else {
            return false;
        };
        if dir_components.is_empty() {
            return true;
        }

        path_is_possible_rule_prefix(&dir_components, &self.components)
    }
}

fn invalid_rule(line_number: usize, message: &'static str) -> FastSyncError {
    FastSyncError::Io {
        context: tr_value("io.parse_filter_rule", line_number),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, message),
    }
}

fn path_components(path: &Path) -> Option<Vec<String>> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_str()?.to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(components)
}

fn path_components_match(pattern: &[String], path: &[String]) -> bool {
    fn matches_from(pattern: &[String], path: &[String]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((head, tail)) if head == "**" => {
                matches_from(tail, path) || (!path.is_empty() && matches_from(pattern, &path[1..]))
            }
            Some((head, tail)) => {
                !path.is_empty()
                    && glob_component_matches(head, &path[0])
                    && matches_from(tail, &path[1..])
            }
        }
    }

    matches_from(pattern, path)
}

fn path_is_possible_rule_prefix(path: &[String], pattern: &[String]) -> bool {
    fn possible(path: &[String], pattern: &[String]) -> bool {
        if path.is_empty() {
            return true;
        }
        match pattern.split_first() {
            None => false,
            Some((head, tail)) if head == "**" => {
                possible(path, tail) || possible(&path[1..], pattern)
            }
            Some((head, tail)) => {
                glob_component_matches(head, &path[0]) && possible(&path[1..], tail)
            }
        }
    }

    possible(path, pattern)
}

fn glob_component_matches(pattern: &str, value: &str) -> bool {
    fn matches_from(pattern: &[char], value: &[char]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some(('*', tail)) => {
                matches_from(tail, value)
                    || (!value.is_empty() && matches_from(pattern, &value[1..]))
            }
            Some(('?', tail)) => !value.is_empty() && matches_from(tail, &value[1..]),
            Some((head, tail)) => value.split_first().is_some_and(|(value_head, value_tail)| {
                head == value_head && matches_from(tail, value_tail)
            }),
        }
    }

    let pattern: Vec<_> = pattern.chars().collect();
    let value: Vec<_> = value.chars().collect();
    matches_from(&pattern, &value)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn exclude_rules_protect_matching_files_and_directory_subtrees() -> Result<()> {
        let filter = PathFilter::from_rules(FilterMode::Exclude, "*.tmp\ncache/\n")?;

        assert!(!filter.allows_entry(Path::new("a.tmp"), false));
        assert!(!filter.allows_entry(Path::new("nested/a.tmp"), false));
        assert!(!filter.allows_entry(Path::new("cache"), true));
        assert!(!filter.allows_entry(Path::new("cache/data.bin"), false));
        assert!(filter.allows_entry(Path::new("src/main.rs"), false));
        Ok(())
    }

    #[test]
    fn include_rules_limit_sync_scope_but_keep_needed_traversal() -> Result<()> {
        let filter = PathFilter::from_rules(FilterMode::Include, "/src/**/*.rs\nassets/\n")?;

        assert!(!filter.allows_entry(Path::new("src"), true));
        assert!(filter.should_descend(Path::new("src")));
        assert!(filter.allows_entry(Path::new("src/bin/main.rs"), false));
        assert!(!filter.allows_entry(Path::new("src/bin/main.txt"), false));
        assert!(filter.allows_entry(Path::new("assets"), true));
        assert!(filter.allows_entry(Path::new("assets/logo.png"), false));
        Ok(())
    }

    #[test]
    fn ignores_blank_lines_and_comments() -> Result<()> {
        let filter = PathFilter::from_rules(
            FilterMode::Exclude,
            "\n# generated files\n\n*.tmp\n   # indented comment\n",
        )?;

        assert!(!filter.allows_entry(Path::new("build.tmp"), false));
        assert!(filter.allows_entry(Path::new("main.rs"), false));
        Ok(())
    }

    #[test]
    fn anchored_rules_match_only_from_sync_root() -> Result<()> {
        let filter = PathFilter::from_rules(FilterMode::Exclude, "/target/\n")?;

        assert!(!filter.allows_entry(Path::new("target"), true));
        assert!(!filter.allows_entry(Path::new("target/debug/app"), false));
        assert!(filter.allows_entry(Path::new("nested/target"), true));
        assert!(filter.allows_entry(Path::new("nested/target/debug/app"), false));
        Ok(())
    }

    #[test]
    fn question_mark_and_double_star_match_common_globs() -> Result<()> {
        let filter =
            PathFilter::from_rules(FilterMode::Include, "docs/v?/guide-*.md\nsrc/**/*.rs\n")?;

        assert!(filter.allows_entry(Path::new("docs/v1/guide-install.md"), false));
        assert!(!filter.allows_entry(Path::new("docs/v10/guide-install.md"), false));
        assert!(filter.allows_entry(Path::new("src/lib.rs"), false));
        assert!(filter.allows_entry(Path::new("src/bin/fastsync.rs"), false));
        assert!(!filter.allows_entry(Path::new("src/bin/fastsync.txt"), false));
        Ok(())
    }

    #[test]
    fn invalid_root_escape_patterns_fail_loudly() {
        let error = PathFilter::from_rules(FilterMode::Exclude, "../secret\n")
            .expect_err("escape patterns should be rejected");

        assert!(error.to_string().contains("sync root"));
    }

    #[test]
    fn unsupported_negation_rules_fail_loudly() {
        let error = PathFilter::from_rules(FilterMode::Exclude, "!keep.txt\n")
            .expect_err("negation is intentionally unsupported in the initial subset");

        assert!(
            error
                .to_string()
                .contains("negation rules are not supported")
        );
    }
}
