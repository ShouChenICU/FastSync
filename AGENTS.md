# fastsync Agent Instructions

## Role

You are a Rust programming agent maintaining fastsync. Your goal is to improve directory sync performance and stability while preserving synchronization correctness, recoverability, and maintainability.

## Project Overview

fastsync is a fast directory synchronization tool written in Rust for large directories and many-file workloads. Core concerns include directory scanning, difference detection, concurrent copying, result verification, error recovery, and observable logging.

## Working Principles

- Confirm synchronization semantics before changing code: clarify one-way or two-way behavior, overwrite strategy, delete strategy, timestamp handling, and permission handling.
- Fix root causes instead of applying surface patches. Prefer minimal, verifiable changes by default.
- Implement and verify in small steps before broader optimization or refactoring, especially around the core sync pipeline.
- Before adding dependencies, evaluate necessity, maintenance cost, and performance benefit. Prefer the standard library and mature, lightweight Rust crates.
- Do not change CLI behavior, synchronization semantics, or default safety policies unless the requirement is clear.

## Coding Standards

- Keep code logic clear, structurally complete, and maintainable for later iteration.
- Split modules by responsibility. Keep scanning, comparison, scheduling, execution, verification, configuration, and errors clearly separated.
- Prefer directory-backed modules for large features. When a module grows toward 2000 lines, split it by responsibility before adding more behavior; avoid single source files over 2000 lines unless there is a documented reason.
- Public structs, important functions, concurrency boundaries, and error branches should have concise Chinese comments that explain inputs, outputs, constraints, and failure semantics.
- Avoid unexplained `unwrap` or `expect`; propagate errors through `Result` and add useful context.
- Encapsulate I/O, concurrency, hashing, and path handling logic into testable units instead of piling complex behavior into entry points.
- Base performance optimization on measurement. For large files, prefer streaming reads and avoid loading whole files into memory.

## Internationalization

- fastsync supports English and Simplified Chinese. Keep user-facing CLI help, text summaries, errors, I/O contexts, and log messages internationalized through `src/i18n.rs` and `locales/app.yml`.
- Do not hard-code new user-facing English or Chinese strings in core modules. Add translation keys to `locales/app.yml` and access them through the existing i18n helpers.
- JSON output field names and other machine-readable schema keys must stay stable and untranslated.
- Language selection priority is `--lang`, then `FASTSYNC_LANG`, then system locale variables (`LC_ALL`, `LC_MESSAGES`, `LANGUAGE`, `LANG`), then English fallback. Preserve common locale alias compatibility such as `zh_CN.UTF-8` and `zh-Hans-CN`.
- When adding CLI options or value enums, localize both the option help and possible-value help. Keep `--lang <en|zh-CN>` as the public canonical form while accepting compatible aliases.
- When changing translated output, update or add focused tests for default English, Simplified Chinese, and locale alias behavior where relevant.

## Rust Conventions

- `cargo fmt`, `cargo clippy`, `cargo test`, and `cargo check` should pass by default.
- Prefer clear task partitioning, message passing, or controlled worker pools for concurrency. Be cautious with shared mutable state.
- When touching filesystem semantics, explicitly handle symlinks, empty directories, permissions, timestamps, file overwrite behavior, and partial-failure recovery.

## Delivery Requirements

- Explain the motivation, impact, risks, and verification method for each change.
- For changes involving sync correctness, delete behavior, concurrent execution, or verification logic, prefer adding tests or at least documenting the key boundary cases.

## Final Checks

- Before finishing any code-change task, run `cargo clippy --all-targets --all-features -- -D warnings` and fix all reported issues.
- Also run the relevant tests for changed code behavior. For broad Rust changes, prefer `cargo test`.
- Documentation-only or instruction-only changes do not require `cargo clippy` unless they also modify code.
