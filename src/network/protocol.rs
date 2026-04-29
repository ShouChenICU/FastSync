use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::SyncDirection;

/// 一次网络同步的行为选项；由客户端声明，服务端按共享权限再次校验。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(super) struct TransferOptions {
    pub(super) delete: bool,
    pub(super) strict: bool,
    pub(super) preserve_times: bool,
    pub(super) preserve_permissions: bool,
    pub(super) file_concurrency: usize,
}

/// 远端目录快照；只包含可通过网络同步的目录和普通文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub(super) dirs: Vec<DirManifest>,
    pub(super) files: Vec<FileManifest>,
}

/// manifest 中的一条目录记录，路径必须是受限相对路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DirManifest {
    #[serde(with = "wire_path")]
    pub(super) path: PathBuf,
    pub(super) metadata: WireMetadata,
}

/// manifest 中的一条文件记录，记录长度和可选元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct FileManifest {
    #[serde(with = "wire_path")]
    pub(super) path: PathBuf,
    pub(super) len: u64,
    pub(super) metadata: WireMetadata,
}

/// 实际文件传输前发送的文件描述，包含用于接收端校验的摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct FileTransfer {
    #[serde(with = "wire_path")]
    pub(super) path: PathBuf,
    pub(super) len: u64,
    pub(super) blake3: String,
    pub(super) metadata: WireMetadata,
}

/// 网络协议中的跨平台文件元数据；不支持的平台字段保持为空。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct WireMetadata {
    pub(super) modified_secs: Option<i64>,
    pub(super) modified_nanos: Option<u32>,
    pub(super) readonly: bool,
    pub(super) unix_mode: Option<u32>,
}

impl WireMetadata {
    /// 从扫描结果提取可安全跨端传输的元数据；时间异常时降级为空。
    pub(super) fn from_entry(entry: &crate::scan::FileEntry) -> Self {
        let (modified_secs, modified_nanos) = entry
            .modified
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| {
                (
                    Some(duration.as_secs() as i64),
                    Some(duration.subsec_nanos()),
                )
            })
            .unwrap_or((None, None));

        Self {
            modified_secs,
            modified_nanos,
            readonly: entry.readonly,
            #[cfg(unix)]
            unix_mode: Some(entry.mode),
            #[cfg(not(unix))]
            unix_mode: None,
        }
    }

    /// 转换为 filetime 可写入的时间戳；缺字段时表示不保留时间。
    pub(super) fn modified_filetime(&self) -> Option<filetime::FileTime> {
        Some(filetime::FileTime::from_unix_time(
            self.modified_secs?,
            self.modified_nanos?,
        ))
    }
}

/// QUIC 控制流中传递的协议消息；大文件内容走独立单向流。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum WireMessage {
    Hello {
        code: String,
        direction: SyncDirection,
        protocol: u16,
        options: TransferOptions,
    },
    Accept {
        mode: String,
        delete_allowed: bool,
    },
    Reject {
        reason: String,
    },
    ManifestStart,
    ManifestDir(DirManifest),
    ManifestFile(FileManifest),
    ManifestEnd,
    HashRequest {
        #[serde(with = "wire_path")]
        path: PathBuf,
    },
    HashRequestEnd,
    Hash {
        #[serde(with = "wire_path")]
        path: PathBuf,
        blake3: String,
    },
    RequestFile {
        #[serde(with = "wire_path")]
        path: PathBuf,
    },
    RequestEnd,
    File(FileTransfer),
    Done,
    Ack {
        files: usize,
        bytes: u64,
        deleted: usize,
    },
}

mod wire_path {
    use super::*;

    /// 将平台相关相对路径编码成 `/` 分隔的网络路径，避免 Windows `\` 泄漏到 Android/Linux。
    pub(super) fn serialize<S>(path: &Path, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let path = path_to_wire_string(path).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&path)
    }

    /// 解码网络相对路径；兼容旧版本 Windows 端发送的 `\` 分隔路径。
    pub(super) fn deserialize<'de, D>(deserializer: D) -> std::result::Result<PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        wire_string_to_path(&value).map_err(serde::de::Error::custom)
    }

    fn path_to_wire_string(path: &Path) -> std::result::Result<String, String> {
        let mut components = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => {
                    let Some(value) = value.to_str() else {
                        return Err("network paths must be valid UTF-8".to_string());
                    };
                    if value.contains(['/', '\\']) {
                        return Err("network path components cannot contain separators".to_string());
                    }
                    components.push(value);
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(
                        "network paths must be relative and stay inside the root".to_string()
                    );
                }
            }
        }

        if components.is_empty() {
            return Err("network path cannot be empty".to_string());
        }
        Ok(components.join("/"))
    }

    fn wire_string_to_path(value: &str) -> std::result::Result<PathBuf, String> {
        if value.is_empty() {
            return Err("network path cannot be empty".to_string());
        }

        let mut path = PathBuf::new();
        for component in value.split(['/', '\\']) {
            if component.is_empty() || component == "." || component == ".." {
                return Err("network paths must be relative and stay inside the root".to_string());
            }
            if Path::new(component)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err("network path components cannot contain separators".to_string());
            }
            path.push(component);
        }
        Ok(path)
    }
}
