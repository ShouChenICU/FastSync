use std::path::PathBuf;

/// 文件复制原因，用于摘要和后续诊断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyReason {
    Missing,
    MetadataChanged,
    ContentChanged,
}

/// 一个明确的同步操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOperation {
    CreateDirectory {
        relative_path: PathBuf,
    },
    CopyFile {
        relative_path: PathBuf,
        bytes: u64,
        reason: CopyReason,
    },
    SetMetadata {
        relative_path: PathBuf,
    },
    DeleteFile {
        relative_path: PathBuf,
    },
    DeleteDirectory {
        relative_path: PathBuf,
    },
    DeleteSymlink {
        relative_path: PathBuf,
    },
}

/// 扫描和比较阶段生成的同步计划。
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    pub operations: Vec<PlanOperation>,
    pub bytes_to_copy: u64,
    pub blake3_compared_files: usize,
}

impl SyncPlan {
    /// 追加操作，并同步维护用于摘要展示的字节数。
    pub fn push(&mut self, operation: PlanOperation) {
        if let PlanOperation::CopyFile { bytes, .. } = operation {
            self.bytes_to_copy = self.bytes_to_copy.saturating_add(bytes);
        }
        self.operations.push(operation);
    }

    /// 记录比较阶段执行过一次 BLAKE3 内容比较。
    pub fn record_blake3_comparison(&mut self) {
        self.blake3_compared_files = self.blake3_compared_files.saturating_add(1);
    }
}
