use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};

use crate::config::{CompareMode, HashAlgorithm, LogLevel, OutputMode, PreserveMode, VerifyMode};
use crate::i18n::{Language, set_language, tr};

/// fastsync 命令行参数。
///
/// 参数按照同步语义、比较策略、性能控制和输出行为分组，默认值优先保证安全。
#[derive(Debug, Clone)]
pub struct Cli {
    /// 源目录。
    pub source: PathBuf,

    /// 目标目录。
    pub target: PathBuf,

    /// 只生成计划与摘要，不实际修改目标目录。
    pub dry_run: bool,

    /// 删除目标端源目录中不存在的多余项。默认关闭，避免误删。
    pub delete: bool,

    /// 遍历时跟随符号链接。默认关闭。
    pub follow_symlinks: bool,

    /// 文件比较策略。
    pub compare: CompareMode,

    /// strict 比较模式的快捷方式：大小一致时始终使用 BLAKE3 确认内容。
    pub strict: bool,

    /// 内容校验哈希算法。当前支持 BLAKE3。
    pub hash: HashAlgorithm,

    /// 复制后的校验强度。
    pub verify: VerifyMode,

    /// 禁用同名且内容相同文件的独立元数据同步。
    pub sync_metadata: bool,

    /// 是否保留修改时间。
    pub preserve_times: PreserveMode,

    /// 是否保留基础权限位。
    pub preserve_permissions: PreserveMode,

    /// 禁用临时文件 + 重命名写入目标文件。
    pub atomic_write: bool,

    /// worker 线程数，可传数字或 auto。
    pub threads: Option<String>,

    /// 有界任务队列长度，默认 threads * 4。
    pub queue_size: Option<usize>,

    /// 最大允许错误数，达到阈值后中止。
    pub max_errors: usize,

    /// 首个错误后立即停止。
    pub stop_on_error: bool,

    /// 日志级别。
    pub log_level: LogLevel,

    /// 摘要输出格式。
    pub output: OutputMode,

    /// 用户界面语言。
    pub language: Language,
}

impl Cli {
    /// 从进程参数解析 CLI，解析失败或请求帮助/版本时由 clap 负责退出。
    pub fn parse() -> Self {
        let args: Vec<_> = std::env::args_os().collect();
        Self::parse_from(args)
    }

    /// 从给定参数解析 CLI，主要用于主入口和测试。
    pub fn parse_from<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        let language = Self::detect_language(&args);
        let matches = Self::command(language).get_matches_from(args);
        Self::from_matches(matches, language)
    }

    /// 构造指定语言的 clap 命令定义。
    pub fn command(language: Language) -> Command {
        set_language(language);

        let arguments = static_text(tr(language, "cli.arguments"));
        let options = static_text(tr(language, "cli.options"));
        let usage = tr(language, "cli.usage");
        let help_template = format!("{{about-with-newline}}\n{usage}: {{usage}}\n\n{{all-args}}");

        Command::new("fastsync")
            .version(env!("CARGO_PKG_VERSION"))
            .about(tr(language, "cli.about"))
            .disable_help_flag(true)
            .disable_version_flag(true)
            .help_template(help_template)
            .arg(
                Arg::new("source")
                    .value_name("SOURCE")
                    .value_parser(value_parser!(PathBuf))
                    .required(true)
                    .help(tr(language, "cli.source"))
                    .help_heading(arguments),
            )
            .arg(
                Arg::new("target")
                    .value_name("TARGET")
                    .value_parser(value_parser!(PathBuf))
                    .required(true)
                    .help(tr(language, "cli.target"))
                    .help_heading(arguments),
            )
            .arg(flag(
                "dry_run",
                'n',
                "dry-run",
                tr(language, "cli.dry_run"),
                options,
            ))
            .arg(flag(
                "delete",
                'd',
                "delete",
                tr(language, "cli.delete"),
                options,
            ))
            .arg(long_flag(
                "follow_symlinks",
                "follow-symlinks",
                tr(language, "cli.follow_symlinks"),
                options,
            ))
            .arg(
                Arg::new("compare")
                    .short('c')
                    .long("compare")
                    .value_name("MODE")
                    .value_parser(compare_mode_parser(language))
                    .default_value("fast")
                    .help(tr(language, "cli.compare"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("strict")
                    .long("strict")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("compare")
                    .help(tr(language, "cli.strict"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("hash")
                    .long("hash")
                    .value_name("ALGORITHM")
                    .value_parser(hash_algorithm_parser(language))
                    .default_value("blake3")
                    .help(tr(language, "cli.hash"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("verify")
                    .long("verify")
                    .value_name("MODE")
                    .value_parser(verify_mode_parser(language))
                    .default_value("changed")
                    .help(tr(language, "cli.verify"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("sync_metadata")
                    .long("no-sync-metadata")
                    .action(ArgAction::SetFalse)
                    .help(tr(language, "cli.no_sync_metadata"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("preserve_times")
                    .long("preserve-times")
                    .value_name("MODE")
                    .value_parser(preserve_mode_parser(language))
                    .default_value("auto")
                    .help(tr(language, "cli.preserve_times"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("preserve_permissions")
                    .long("preserve-permissions")
                    .value_name("MODE")
                    .value_parser(preserve_mode_parser(language))
                    .default_value("auto")
                    .help(tr(language, "cli.preserve_permissions"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("atomic_write")
                    .long("no-atomic-write")
                    .action(ArgAction::SetFalse)
                    .help(tr(language, "cli.no_atomic_write"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("threads")
                    .short('t')
                    .long("threads")
                    .value_name("N|auto")
                    .default_value("auto")
                    .help(tr(language, "cli.threads"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("queue_size")
                    .short('q')
                    .long("queue-size")
                    .value_name("N")
                    .value_parser(value_parser!(usize))
                    .help(tr(language, "cli.queue_size"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("max_errors")
                    .long("max-errors")
                    .value_name("N")
                    .value_parser(value_parser!(usize))
                    .default_value("100")
                    .help(tr(language, "cli.max_errors"))
                    .help_heading(options),
            )
            .arg(long_flag(
                "stop_on_error",
                "stop-on-error",
                tr(language, "cli.stop_on_error"),
                options,
            ))
            .arg(
                Arg::new("log_level")
                    .short('l')
                    .long("log-level")
                    .value_name("LEVEL")
                    .value_parser(log_level_parser())
                    .default_value("info")
                    .help(tr(language, "cli.log_level"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("output")
                    .short('o')
                    .long("output")
                    .value_name("FORMAT")
                    .value_parser(output_mode_parser())
                    .default_value("text")
                    .help(tr(language, "cli.output"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("language")
                    .long("lang")
                    .value_name("LOCALE")
                    .value_parser(language_parser())
                    .default_value(language.as_locale())
                    .help(tr(language, "cli.lang"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("help")
                    .short('h')
                    .long("help")
                    .action(ArgAction::Help)
                    .help(tr(language, "cli.help"))
                    .help_heading(options),
            )
            .arg(
                Arg::new("version")
                    .short('V')
                    .long("version")
                    .action(ArgAction::Version)
                    .help(tr(language, "cli.version"))
                    .help_heading(options),
            )
    }

    /// 根据参数中的 `--lang` 或 `FASTSYNC_LANG` 选择帮助/错误语言。
    pub fn detect_language(args: &[OsString]) -> Language {
        let mut iter = args.iter().skip(1);
        while let Some(arg) = iter.next() {
            if let Some(language) = parse_inline_language(arg) {
                return language;
            }

            if arg == "--lang" {
                if let Some(value) = iter.next().and_then(|value| value.to_str()) {
                    if let Some(language) = Language::parse(value) {
                        return language;
                    }
                }
            }
        }

        Language::from_env().unwrap_or(Language::DEFAULT)
    }

    fn from_matches(matches: ArgMatches, fallback_language: Language) -> Self {
        let language = matches
            .get_one::<Language>("language")
            .copied()
            .unwrap_or(fallback_language);

        Self {
            source: matches
                .get_one::<PathBuf>("source")
                .expect("required by clap")
                .clone(),
            target: matches
                .get_one::<PathBuf>("target")
                .expect("required by clap")
                .clone(),
            dry_run: matches.get_flag("dry_run"),
            delete: matches.get_flag("delete"),
            follow_symlinks: matches.get_flag("follow_symlinks"),
            compare: *matches
                .get_one::<CompareMode>("compare")
                .expect("defaulted by clap"),
            strict: matches.get_flag("strict"),
            hash: *matches
                .get_one::<HashAlgorithm>("hash")
                .expect("defaulted by clap"),
            verify: *matches
                .get_one::<VerifyMode>("verify")
                .expect("defaulted by clap"),
            sync_metadata: *matches.get_one::<bool>("sync_metadata").unwrap_or(&true),
            preserve_times: *matches
                .get_one::<PreserveMode>("preserve_times")
                .expect("defaulted by clap"),
            preserve_permissions: *matches
                .get_one::<PreserveMode>("preserve_permissions")
                .expect("defaulted by clap"),
            atomic_write: *matches.get_one::<bool>("atomic_write").unwrap_or(&true),
            threads: matches.get_one::<String>("threads").cloned(),
            queue_size: matches.get_one::<usize>("queue_size").copied(),
            max_errors: *matches
                .get_one::<usize>("max_errors")
                .expect("defaulted by clap"),
            stop_on_error: matches.get_flag("stop_on_error"),
            log_level: *matches
                .get_one::<LogLevel>("log_level")
                .expect("defaulted by clap"),
            output: *matches
                .get_one::<OutputMode>("output")
                .expect("defaulted by clap"),
            language,
        }
    }

    /// 测试辅助构造器，避免单元测试依赖命令行字符串解析。
    #[cfg(test)]
    pub fn for_test(source: &std::path::Path, target: &std::path::Path) -> Self {
        Self {
            source: source.to_path_buf(),
            target: target.to_path_buf(),
            dry_run: false,
            delete: false,
            follow_symlinks: false,
            compare: CompareMode::Fast,
            strict: false,
            hash: HashAlgorithm::Blake3,
            verify: VerifyMode::Changed,
            sync_metadata: true,
            preserve_times: PreserveMode::Auto,
            preserve_permissions: PreserveMode::Auto,
            atomic_write: true,
            threads: Some("auto".to_string()),
            queue_size: None,
            max_errors: 100,
            stop_on_error: false,
            log_level: LogLevel::Info,
            output: OutputMode::Text,
            language: Language::DEFAULT,
        }
    }
}

fn flag(
    id: &'static str,
    short: char,
    long: &'static str,
    help: String,
    heading: &'static str,
) -> Arg {
    Arg::new(id)
        .short(short)
        .long(long)
        .action(ArgAction::SetTrue)
        .help(help)
        .help_heading(heading)
}

fn long_flag(id: &'static str, long: &'static str, help: String, heading: &'static str) -> Arg {
    Arg::new(id)
        .long(long)
        .action(ArgAction::SetTrue)
        .help(help)
        .help_heading(heading)
}

fn parse_inline_language(arg: &OsStr) -> Option<Language> {
    arg.to_str()
        .and_then(|raw| raw.strip_prefix("--lang="))
        .and_then(Language::parse)
}

fn static_text(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn compare_mode_parser(language: Language) -> impl TypedValueParser<Value = CompareMode> + 'static {
    PossibleValuesParser::new([
        possible_value("fast", language, "value.compare.fast"),
        possible_value("strict", language, "value.compare.strict"),
    ])
    .map(|value| match value.as_str() {
        "fast" => CompareMode::Fast,
        "strict" => CompareMode::Strict,
        _ => unreachable!("validated by clap possible values"),
    })
}

fn verify_mode_parser(language: Language) -> impl TypedValueParser<Value = VerifyMode> + 'static {
    PossibleValuesParser::new([
        possible_value("none", language, "value.verify.none"),
        possible_value("changed", language, "value.verify.changed"),
        possible_value("all", language, "value.verify.all"),
    ])
    .map(|value| match value.as_str() {
        "none" => VerifyMode::None,
        "changed" => VerifyMode::Changed,
        "all" => VerifyMode::All,
        _ => unreachable!("validated by clap possible values"),
    })
}

fn preserve_mode_parser(
    language: Language,
) -> impl TypedValueParser<Value = PreserveMode> + 'static {
    PossibleValuesParser::new([
        possible_value("auto", language, "value.preserve.auto"),
        possible_value("true", language, "value.preserve.true"),
        possible_value("false", language, "value.preserve.false"),
    ])
    .map(|value| match value.as_str() {
        "auto" => PreserveMode::Auto,
        "true" => PreserveMode::True,
        "false" => PreserveMode::False,
        _ => unreachable!("validated by clap possible values"),
    })
}

fn hash_algorithm_parser(
    language: Language,
) -> impl TypedValueParser<Value = HashAlgorithm> + 'static {
    PossibleValuesParser::new([possible_value("blake3", language, "value.hash.blake3")]).map(
        |value| match value.as_str() {
            "blake3" => HashAlgorithm::Blake3,
            _ => unreachable!("validated by clap possible values"),
        },
    )
}

fn log_level_parser() -> impl TypedValueParser<Value = LogLevel> + 'static {
    PossibleValuesParser::new(["error", "warn", "info", "debug", "trace"]).map(|value| match value
        .as_str()
    {
        "error" => LogLevel::Error,
        "warn" => LogLevel::Warn,
        "info" => LogLevel::Info,
        "debug" => LogLevel::Debug,
        "trace" => LogLevel::Trace,
        _ => unreachable!("validated by clap possible values"),
    })
}

fn output_mode_parser() -> impl TypedValueParser<Value = OutputMode> + 'static {
    PossibleValuesParser::new(["text", "json"]).map(|value| match value.as_str() {
        "text" => OutputMode::Text,
        "json" => OutputMode::Json,
        _ => unreachable!("validated by clap possible values"),
    })
}

fn language_parser() -> impl TypedValueParser<Value = Language> + 'static {
    PossibleValuesParser::new([
        PossibleValue::new("en").aliases([
            "en-US",
            "en_US",
            "en_US.UTF-8",
            "english",
            "C",
            "POSIX",
        ]),
        PossibleValue::new("zh-CN").aliases([
            "zh",
            "zh-cn",
            "zh_CN",
            "zh_CN.UTF-8",
            "zh-Hans",
            "zh_Hans",
            "zh-Hans-CN",
            "zh_Hans_CN",
            "cn",
            "chinese",
            "中文",
        ]),
    ])
    .map(|value| Language::parse(&value).expect("validated by clap possible values"))
}

fn possible_value(name: &'static str, language: Language, help_key: &str) -> PossibleValue {
    PossibleValue::new(name).help(tr(language, help_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_help_is_english() {
        let help = Cli::command(Language::En).render_long_help().to_string();

        assert!(help.contains("A fast, reliable one-way directory synchronization tool"));
        assert!(help.contains("Source directory."));
        assert!(help.contains("Trust metadata when it matches"));
        assert!(!help.contains("快速、可靠的单向目录同步工具"));
    }

    #[test]
    fn zh_cn_help_is_chinese() {
        let help = Cli::command(Language::ZhCn).render_long_help().to_string();

        assert!(help.contains("快速、可靠的单向目录同步工具"));
        assert!(help.contains("源目录。"));
        assert!(help.contains("元数据一致时信任元数据"));
    }

    #[test]
    fn detects_inline_language_before_full_parse() {
        let args = vec![
            OsString::from("fastsync"),
            OsString::from("--lang=zh-CN"),
            OsString::from("src"),
            OsString::from("dst"),
        ];

        assert_eq!(Cli::detect_language(&args), Language::ZhCn);
    }

    #[test]
    fn parses_locale_aliases_from_language_flag() {
        let cli = Cli::parse_from(["fastsync", "--lang", "zh_CN.UTF-8", "src", "dst"]);

        assert_eq!(cli.language, Language::ZhCn);
    }
}
