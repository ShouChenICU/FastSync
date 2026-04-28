use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::error::{Result, io_context};
use crate::i18n::tr_path;

const HASH_BUFFER_SIZE: usize = 1024 * 1024;

/// BLAKE3 摘要的固定长度字节表示。
pub type Blake3Digest = [u8; 32];

/// 以流式方式计算文件 BLAKE3 哈希。
///
/// 该函数不会一次性读取整个文件，适合大文件校验和比较。
pub fn blake3_file(path: &Path) -> Result<Blake3Digest> {
    let file = io_context(
        tr_path("io.open_file_for_hash", path.display()),
        File::open(path),
    )?;
    blake3_reader(path, file)
}

/// 从任意流式 reader 计算 BLAKE3 哈希。
///
/// `path` 只用于错误上下文；调用方仍需保证 reader 对应的内容来源正确。
pub fn blake3_reader(path: &Path, reader: impl Read) -> Result<Blake3Digest> {
    let mut reader = BufReader::with_capacity(HASH_BUFFER_SIZE, reader);
    let mut hasher = blake3::Hasher::new();
    // 哈希缓冲区必须位于堆上；Windows 主线程默认栈较小，网络服务端构建清单时会走到这里。
    let mut buffer = vec![0_u8; HASH_BUFFER_SIZE];

    loop {
        let read = io_context(
            tr_path("io.read_file_for_hash", path.display()),
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn blake3_file_works_on_small_stack_thread()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        let path = root.path().join("sample.bin");
        fs::write(&path, vec![42_u8; 64 * 1024])?;

        let digest = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn({
                let path = path.clone();
                move || blake3_file(&path)
            })?
            .join()
            .expect("small-stack hashing thread should not panic")?;

        assert_eq!(digest, blake3_file(&path)?);
        Ok(())
    }
}
