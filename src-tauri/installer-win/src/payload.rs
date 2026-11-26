//! Self-extracting (SFX) payload packer and verified streaming extractor.
//!
//! Binary layout appended after the Windows PE stub executable:
//!   `[stub_pe_bytes][gzip_payload][meta_json][trailer: 24 bytes]`
//!
//! Trailer (24 bytes, little-endian):
//!   - `0..8`:   magic `b"HCASTSFX"`
//!   - `8..16`:  `payload_len: u64`
//!   - `16..20`: `meta_len: u32`
//!   - `20..24`: `version_tag: u32` (`1`)

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"HCASTSFX";
const TRAILER_LEN: u64 = 24;
const FORMAT_VERSION: u32 = 1;
const MAX_META_LEN: u32 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadMeta {
    pub version: String,
    pub exe_name: String,
    pub raw_size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedPayload {
    pub stub_len: u64,
    pub payload_offset: u64,
    pub payload_len: u64,
    pub meta: PayloadMeta,
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn sha256_file(path: &Path) -> io::Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((total, hex_encode(&hasher.finalize())))
}

/// Pack `stub_exe` + gzip-compressed `app_exe` + JSON metadata + SFX trailer into `out_setup`.
pub fn pack_installer(
    stub_exe: &Path,
    app_exe: &Path,
    version: &str,
    exe_name: &str,
    out_setup: &Path,
) -> io::Result<PayloadMeta> {
    let (raw_size, sha256) = sha256_file(app_exe)?;
    if raw_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "application binary is empty",
        ));
    }

    if let Some(parent) = out_setup.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let tmp_path = out_setup.with_extension("exe.packing");
    let mut out = File::create(&tmp_path)?;

    // 1. Copy stub PE executable.
    {
        let mut stub = File::open(stub_exe)?;
        io::copy(&mut stub, &mut out)?;
    }

    // 2. Stream gzip-compressed application executable.
    let payload_start = out.stream_position()?;
    {
        let mut src = File::open(app_exe)?;
        let mut encoder = GzEncoder::new(&mut out, Compression::best());
        io::copy(&mut src, &mut encoder)?;
        encoder.finish()?;
    }
    let payload_end = out.stream_position()?;
    let payload_len = payload_end - payload_start;

    // 3. Append metadata JSON.
    let meta = PayloadMeta {
        version: version.to_string(),
        exe_name: exe_name.to_string(),
        raw_size,
        sha256,
    };
    let meta_bytes =
        serde_json::to_vec(&meta).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let meta_len = u32::try_from(meta_bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "metadata too large"))?;
    out.write_all(&meta_bytes)?;

    // 4. Append 24-byte trailer.
    let mut trailer = [0u8; 24];
    trailer[0..8].copy_from_slice(MAGIC);
    trailer[8..16].copy_from_slice(&payload_len.to_le_bytes());
    trailer[16..20].copy_from_slice(&meta_len.to_le_bytes());
    trailer[20..24].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.write_all(&trailer)?;
    out.sync_all()?;
    drop(out);

    fs::rename(&tmp_path, out_setup)?;
    Ok(meta)
}

/// Inspect the trailing SFX header of an installer binary. Returns `Ok(None)` for a bare stub.
pub fn locate_payload(exe_path: &Path) -> io::Result<Option<LocatedPayload>> {
    let mut file = File::open(exe_path)?;
    let total_len = file.metadata()?.len();
    if total_len < TRAILER_LEN {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-(TRAILER_LEN as i64)))?;
    let mut trailer = [0u8; 24];
    file.read_exact(&mut trailer)?;
    if &trailer[0..8] != MAGIC {
        return Ok(None);
    }
    let payload_len = u64::from_le_bytes(trailer[8..16].try_into().unwrap());
    let meta_len = u32::from_le_bytes(trailer[16..20].try_into().unwrap());
    let format_ver = u32::from_le_bytes(trailer[20..24].try_into().unwrap());
    if format_ver != FORMAT_VERSION || meta_len == 0 || meta_len > MAX_META_LEN || payload_len == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "安装包尾部元信息损坏",
        ));
    }

    let tail_total = payload_len
        .checked_add(meta_len as u64)
        .and_then(|n| n.checked_add(TRAILER_LEN))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "安装包长度溢出"))?;
    if tail_total >= total_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "安装包结构不完整",
        ));
    }

    let stub_len = total_len - tail_total;
    let payload_offset = stub_len;
    let meta_offset = payload_offset + payload_len;
    file.seek(SeekFrom::Start(meta_offset))?;
    let mut meta_buf = vec![0u8; meta_len as usize];
    file.read_exact(&mut meta_buf)?;
    let meta: PayloadMeta = serde_json::from_slice(&meta_buf).map_err(|err| {
        io::Error::new(io::ErrorKind::InvalidData, format!("元数据解析失败：{err}"))
    })?;

    Ok(Some(LocatedPayload {
        stub_len,
        payload_offset,
        payload_len,
        meta,
    }))
}

/// Extract the embedded gzip payload to `dest_exe`, verifying size & SHA-256 before committing.
pub fn extract_verified<F>(
    exe_path: &Path,
    located: &LocatedPayload,
    dest_exe: &Path,
    mut on_progress: F,
) -> io::Result<()>
where
    F: FnMut(u8),
{
    if let Some(parent) = dest_exe.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = File::open(exe_path)?;
    file.seek(SeekFrom::Start(located.payload_offset))?;
    let limited = file.take(located.payload_len);
    let mut decoder = GzDecoder::new(limited);

    let tmp_path = dest_exe.with_extension("exe.new");
    let mut out = File::create(&tmp_path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut written = 0u64;
    let mut last_pct = 0u8;

    let result: io::Result<()> = (|| {
        loop {
            let n = decoder.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            written += n as u64;
            if let Some(raw_pct) = written
                .saturating_mul(100)
                .checked_div(located.meta.raw_size)
            {
                let pct = raw_pct.min(100) as u8;
                if pct > last_pct {
                    last_pct = pct;
                    on_progress(pct);
                }
            }
        }
        out.sync_all()?;
        Ok(())
    })();

    drop(out);

    if let Err(err) = result {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    let digest = hex_encode(&hasher.finalize());
    if written != located.meta.raw_size || digest != located.meta.sha256 {
        let _ = fs::remove_file(&tmp_path);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "安装包完整性校验失败：预期 {} ({})，实际 {} ({})",
                located.meta.raw_size, located.meta.sha256, written, digest
            ),
        ));
    }

    // On Windows, fs::rename fails if destination exists; remove old file first after staging succeeds.
    if dest_exe.exists() {
        fs::remove_file(dest_exe)?;
    }
    fs::rename(&tmp_path, dest_exe)?;
    on_progress(100);
    Ok(())
}

/// Verify embedded payload integrity without writing any file to disk (used by `--self-test`).
pub fn verify_embedded_payload(exe_path: &Path) -> io::Result<PayloadMeta> {
    let located = locate_payload(exe_path)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "当前程序不包含内嵌安装载荷"))?;
    let mut file = File::open(exe_path)?;
    file.seek(SeekFrom::Start(located.payload_offset))?;
    let limited = file.take(located.payload_len);
    let mut decoder = GzDecoder::new(limited);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = decoder.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let digest = hex_encode(&hasher.finalize());
    if total != located.meta.raw_size || digest != located.meta.sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "内嵌载荷 SHA-256 校验失败",
        ));
    }
    Ok(located.meta)
}

/// Copy only the lightweight stub executable (excluding the appended payload) as `uninstall.exe`.
pub fn copy_clean_uninstaller(
    exe_path: &Path,
    located: &LocatedPayload,
    dest_uninstall: &Path,
) -> io::Result<()> {
    let tmp = dest_uninstall.with_extension("exe.new");
    let src = File::open(exe_path)?;
    let mut limited = src.take(located.stub_len);
    let mut out = File::create(&tmp)?;
    let copied = io::copy(&mut limited, &mut out)?;
    out.sync_all()?;
    drop(out);
    if copied != located.stub_len {
        let _ = fs::remove_file(&tmp);
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "写入卸载程序不完整",
        ));
    }
    if dest_uninstall.exists() {
        fs::remove_file(dest_uninstall)?;
    }
    fs::rename(&tmp, dest_uninstall)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_locate_extract_and_copy_uninstaller_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub.exe");
        let app = dir.path().join("HIVECAST.exe");
        let setup = dir.path().join("HIVECAST-setup.exe");

        let stub_bytes = b"MZ_FAKE_STUB_HEADER_BYTES_1234567890".repeat(32);
        let app_bytes = b"HIVECAST_APP_BINARY_CONTENT_ABCDE_0123456789".repeat(2048);
        fs::write(&stub, &stub_bytes).unwrap();
        fs::write(&app, &app_bytes).unwrap();

        let meta = pack_installer(&stub, &app, "0.1.0", "HIVECAST.exe", &setup).unwrap();
        assert_eq!(meta.version, "0.1.0");
        assert_eq!(meta.raw_size, app_bytes.len() as u64);

        let located = locate_payload(&setup).unwrap().unwrap();
        assert_eq!(located.stub_len, stub_bytes.len() as u64);
        assert_eq!(located.meta, meta);

        // Bare stub returns None.
        assert!(locate_payload(&stub).unwrap().is_none());

        // Self-test succeeds.
        assert_eq!(verify_embedded_payload(&setup).unwrap(), meta);

        // Extract and verify bytes + progress monotonically hits 100.
        let out_exe = dir.path().join("installed/HIVECAST.exe");
        let mut last = 0u8;
        extract_verified(&setup, &located, &out_exe, |pct| {
            assert!(pct >= last);
            last = pct;
        })
        .unwrap();
        assert_eq!(last, 100);
        assert_eq!(fs::read(&out_exe).unwrap(), app_bytes);

        // Clean uninstaller excludes the payload.
        let uninst = dir.path().join("installed/uninstall.exe");
        copy_clean_uninstaller(&setup, &located, &uninst).unwrap();
        assert_eq!(fs::read(&uninst).unwrap(), stub_bytes);
        assert!(locate_payload(&uninst).unwrap().is_none());
    }

    #[test]
    fn extract_rejects_tampered_payload_and_cleans_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub.exe");
        let app = dir.path().join("HIVECAST.exe");
        let setup = dir.path().join("HIVECAST-setup.exe");

        fs::write(&stub, b"STUB_BYTES_0123456789").unwrap();
        fs::write(&app, b"PAYLOAD_CONTENT_0123456789".repeat(512)).unwrap();
        pack_installer(&stub, &app, "0.1.0", "HIVECAST.exe", &setup).unwrap();

        let mut located = locate_payload(&setup).unwrap().unwrap();
        located.meta.sha256 = "0".repeat(64);
        let dest = dir.path().join("out/HIVECAST.exe");
        assert!(extract_verified(&setup, &located, &dest, |_| {}).is_err());
        assert!(!dest.exists());
        assert!(!dest.with_extension("exe.new").exists());
    }
}
