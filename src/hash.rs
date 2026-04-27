use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::error::{Result, io_context};

/// BLAKE3 摘要的固定长度字节表示。
pub type Blake3Digest = [u8; 32];

/// 以流式方式计算文件 BLAKE3 哈希。
///
/// 该函数不会一次性读取整个文件，适合大文件校验和比较。
pub fn blake3_file(path: &Path) -> Result<Blake3Digest> {
    let file = io_context(
        format!("打开文件用于哈希: {}", path.display()),
        File::open(path),
    )?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 1024 * 1024];

    loop {
        let read = io_context(
            format!("读取文件用于哈希: {}", path.display()),
            reader.read(&mut buffer),
        )?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(*hasher.finalize().as_bytes())
}

/// 比较两个文件的 BLAKE3 摘要是否一致。
pub fn same_blake3(left: &Path, right: &Path) -> Result<bool> {
    Ok(blake3_file(left)? == blake3_file(right)?)
}
