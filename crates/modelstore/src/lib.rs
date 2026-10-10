//! The store of FilmCraft's optional neural models (layer L2).
//!
//! Like the Whisper models of `filmcraft-speech`, these models are never bundled and never in the
//! repository: each is downloaded on demand, after the user has seen its size, source and licence,
//! into `<data dir>/models/<id>/`, and every file is checked against a pinned SHA-256 before use.
//! A pack (`.tar.gz`) is verified, then only the members named in the catalogue are unpacked to
//! files of their own, each verified again; nothing else in the archive is ever written.
//!
//! | id | task | file(s) | licence |
//! |---|---|---|---|
//! | `deepfilternet3` | voice clean-up (`audio.denoiseVoice`) | 3 ONNX graphs, 8 MB | MIT OR Apache-2.0 |
//! | `yolox-nano` / `yolox-tiny` / `yolox-s` | person detection (`sequence.autoReframe`) | 1 ONNX graph, 3.7 / 20 / 36 MB | Apache-2.0 |
//! | `yunet` | face detection (`sequence.autoReframe`) | 1 ONNX graph, 0.2 MB | MIT |

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::path::{Path, PathBuf};

use serde::Serialize;

/// A file taken out of a downloaded pack.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Member {
    /// Path of the member inside the archive.
    pub member: &'static str,
    /// File name it is stored under in the model folder.
    pub name: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// One downloadable file. When `members` is not empty the file is a `.tar.gz` pack and only the
/// members are kept; otherwise the file itself is the model.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub members: &'static [Member],
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// What it is used for.
    pub task: &'static str,
    pub description: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub author: &'static str,
    pub source: &'static str,
    pub files: &'static [ModelFile],
}

impl ModelInfo {
    /// Bytes to download (packs count as downloaded).
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// The names the model is read from once installed.
    fn stored(&self) -> Vec<(&'static str, u64)> {
        let mut v = Vec::new();
        for f in self.files {
            if f.members.is_empty() {
                v.push((f.name, f.size));
            } else {
                v.extend(f.members.iter().map(|m| (m.name, m.size)));
            }
        }
        v
    }
}

#[derive(Debug)]
pub enum StoreError {
    Unknown(String),
    Io(String),
    Download(String),
    Cancelled,
    Missing(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Unknown(m) => write!(f, "unknown model {m}"),
            StoreError::Io(m) | StoreError::Download(m) => f.write_str(m),
            StoreError::Cancelled => f.write_str("download cancelled"),
            StoreError::Missing(m) => write!(f, "model {m} is not installed"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

const NO_MEMBERS: &[Member] = &[];

const YOLOX_LICENSE: &str = "https://github.com/Megvii-BaseDetection/YOLOX/blob/main/LICENSE";

macro_rules! yolox {
    ($id:literal, $name:literal, $file:literal, $sha:literal, $size:expr, $desc:literal) => {
        ModelInfo {
            id: $id,
            name: $name,
            task: "Subject detection for auto-reframe (people)",
            description: $desc,
            license: "Apache-2.0 (YOLOX code and weights trained on COCO)",
            license_url: YOLOX_LICENSE,
            author: "Megvii",
            source: "https://github.com/Megvii-BaseDetection/YOLOX",
            files: &[ModelFile {
                name: $file,
                url: concat!("https://github.com/Megvii-BaseDetection/YOLOX/releases/download/0.1.1rc0/", $file),
                sha256: $sha,
                size: $size,
                members: NO_MEMBERS,
            }],
        }
    };
}

static CATALOGUE: &[ModelInfo] = &[
    ModelInfo {
        id: "deepfilternet3",
        name: "DeepFilterNet3",
        task: "Voice clean-up (noise removal on speech)",
        description: "Speech enhancement at 48 kHz: a gain per ERB band plus deep filtering of the low frequencies. Three ONNX graphs (about 1 million weights).",
        license: "MIT OR Apache-2.0 (DeepFilterNet code and models)",
        license_url: "https://github.com/Rikorose/DeepFilterNet/blob/main/LICENSE-MIT",
        author: "Hendrik Schröter",
        source: "https://github.com/Rikorose/DeepFilterNet",
        files: &[ModelFile {
            name: "DeepFilterNet3_onnx.tar.gz",
            url: "https://raw.githubusercontent.com/Rikorose/DeepFilterNet/v0.5.6/models/DeepFilterNet3_onnx.tar.gz",
            sha256: "c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616",
            size: 7_983_136,
            members: &[
                Member {
                    member: "tmp/export/enc.onnx",
                    name: "enc.onnx",
                    sha256: "7c5399d3da8a50ebef1c1a0ae421b33376aa5e45d0e92df16da7e83c9c131916",
                    size: 1_954_042,
                },
                Member {
                    member: "tmp/export/erb_dec.onnx",
                    name: "erb_dec.onnx",
                    sha256: "ab669a1d10afe20911728b33053a452071042317a90581092b325da7b2f9d895",
                    size: 3_292_397,
                },
                Member {
                    member: "tmp/export/df_dec.onnx",
                    name: "df_dec.onnx",
                    sha256: "23114ce3b0f6464b763ee62f7bb8aab6b2a129a21eabd5bcfe59413db05f278a",
                    size: 3_340_803,
                },
            ],
        }],
    },
    yolox!(
        "yolox-nano",
        "YOLOX-Nano",
        "yolox_nano.onnx",
        "c789161ed43c8269fcd4e67c67eeeb4e80c622da2eb296a20bc6007bd18a0b7d",
        3_659_407,
        "Object detector, 0.9 M weights, 416 px input: the default, fast on a CPU."
    ),
    yolox!(
        "yolox-tiny",
        "YOLOX-Tiny",
        "yolox_tiny.onnx",
        "427cc366d34e27ff7a03e2899b5e3671425c262ea2291f88bb942bc1cc70b0f7",
        20_219_662,
        "Object detector, 5 M weights, 416 px input: more accurate on small or crowded subjects."
    ),
    yolox!(
        "yolox-s",
        "YOLOX-S",
        "yolox_s.onnx",
        "c5c2d13e59ae883e6af3b45daea64af4833a4951c92d116ec270d9ddbe998063",
        35_858_002,
        "Object detector, 9 M weights, 640 px input: the most accurate of the three, 4x slower than Nano."
    ),
    ModelInfo {
        id: "yunet",
        name: "YuNet",
        task: "Face detection for auto-reframe",
        description: "Face detector (53 k weights) from OpenCV Zoo, 2023 March version, 640 px input.",
        license: "MIT (OpenCV Zoo / libfacedetection)",
        license_url: "https://github.com/opencv/opencv_zoo/blob/main/models/face_detection_yunet/LICENSE",
        author: "Shiqi Yu and contributors",
        source: "https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet",
        files: &[ModelFile {
            name: "face_detection_yunet_2023mar.onnx",
            url: "https://media.githubusercontent.com/media/opencv/opencv_zoo/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx",
            sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
            size: 232_589,
            members: NO_MEMBERS,
        }],
    },
];

pub fn catalogue() -> &'static [ModelInfo] {
    CATALOGUE
}

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    CATALOGUE.iter().find(|m| m.id == id)
}

pub fn model_dir(models_dir: &Path, m: &ModelInfo) -> PathBuf {
    models_dir.join(m.id)
}

/// Every stored file is present with its catalogue size (contents were verified when unpacked).
pub fn installed(models_dir: &Path, m: &ModelInfo) -> bool {
    let d = model_dir(models_dir, m);
    m.stored().iter().all(|(n, s)| std::fs::metadata(d.join(n)).is_ok_and(|md| md.len() == *s))
}

/// Bytes still to download for `m`.
pub fn missing_bytes(models_dir: &Path, m: &ModelInfo) -> u64 {
    if installed(models_dir, m) { 0 } else { m.size() }
}

/// Reads one stored file of an installed model.
pub fn read(models_dir: &Path, m: &ModelInfo, name: &str) -> Result<Vec<u8>, StoreError> {
    if !installed(models_dir, m) {
        return Err(StoreError::Missing(m.id.to_string()));
    }
    if !m.stored().iter().any(|(n, _)| *n == name) {
        return Err(StoreError::Unknown(format!("{}/{name}", m.id)));
    }
    Ok(std::fs::read(model_dir(models_dir, m).join(name))?)
}

/// Download progress: `(bytes done, bytes total, file)`; return false to cancel.
#[cfg(feature = "download")]
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64, &str) -> bool;

#[cfg(feature = "download")]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(feature = "download")]
fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(data))
}

/// Downloads `m` into `models_dir/<id>/`: each file streams into `<name>.part`, its SHA-256 is
/// checked, a pack is unpacked (members only, each checked again) and the part file is removed or
/// renamed into place. A wrong checksum deletes what was written and fails.
#[cfg(feature = "download")]
pub fn download(models_dir: &Path, m: &ModelInfo, progress: Progress) -> Result<(), StoreError> {
    use sha2::Digest;
    use std::io::{Read, Write};
    if installed(models_dir, m) {
        return Ok(());
    }
    let dir = model_dir(models_dir, m);
    std::fs::create_dir_all(&dir)?;
    let total = m.size();
    let agent = agent();
    let mut done = 0u64;
    for f in m.files {
        let part = dir.join(format!("{}.part", f.name));
        let resp = agent.get(f.url).call().map_err(|e| StoreError::Download(format!("{}: {e}", f.url)))?;
        let mut body = resp.into_body();
        let mut reader = body.as_reader();
        let mut out = std::io::BufWriter::new(std::fs::File::create(&part)?);
        let mut hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut got = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| StoreError::Download(format!("{}: {e}", f.name)))?;
            if n == 0 {
                break;
            }
            let chunk = buf.get(..n).unwrap_or(&[]);
            hasher.update(chunk);
            out.write_all(chunk)?;
            got += n as u64;
            if !progress(done + got, total, f.name) {
                drop(out);
                let _ = std::fs::remove_file(&part);
                return Err(StoreError::Cancelled);
            }
        }
        out.flush()?;
        drop(out);
        let sha = hex(&hasher.finalize());
        if sha != f.sha256 || got != f.size {
            let _ = std::fs::remove_file(&part);
            return Err(StoreError::Download(format!("{}: checksum mismatch (got {sha}, {got} bytes; expected {}, {} bytes)", f.name, f.sha256, f.size)));
        }
        if f.members.is_empty() {
            std::fs::rename(&part, dir.join(f.name))?;
        } else {
            let pack = std::fs::read(&part)?;
            let _ = std::fs::remove_file(&part);
            for mem in f.members {
                let data = tar_gz_member(&pack, mem.member).ok_or_else(|| StoreError::Download(format!("{}: {} is not in the archive", f.name, mem.member)))?;
                if data.len() as u64 != mem.size || sha256_hex(&data) != mem.sha256 {
                    return Err(StoreError::Download(format!("{}: {} does not match its checksum", f.name, mem.member)));
                }
                let tmp = dir.join(format!("{}.part", mem.name));
                std::fs::write(&tmp, &data)?;
                std::fs::rename(&tmp, dir.join(mem.name))?;
            }
        }
        done += f.size;
    }
    Ok(())
}

/// The bytes of regular file `wanted` in a `.tar.gz`, if present. Entry names are only compared,
/// never used as paths. Archives larger than 256 MiB once unpacked are refused.
#[cfg(feature = "download")]
pub fn tar_gz_member(pack: &[u8], wanted: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(pack).take(256 << 20).read_to_end(&mut raw).ok()?;
    tar_member(&raw, wanted)
}

/// The bytes of regular file `wanted` in an uncompressed tar.
pub fn tar_member(tar: &[u8], wanted: &str) -> Option<Vec<u8>> {
    let mut pos = 0usize;
    let mut long_name: Option<String> = None;
    while let Some(h) = tar.get(pos..pos + 512) {
        if h.iter().all(|b| *b == 0) {
            return None;
        }
        let field = |a: usize, b: usize| h.get(a..b).unwrap_or(&[]);
        let text = |s: &[u8]| String::from_utf8_lossy(s.split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
        let size = usize::from_str_radix(text(field(124, 136)).trim(), 8).ok()?;
        let kind = h.get(156).copied().unwrap_or(b'0');
        let prefix = text(field(345, 500));
        let mut name = text(field(0, 100));
        if !prefix.is_empty() {
            name = format!("{prefix}/{name}");
        }
        let data = tar.get(pos + 512..pos + 512 + size)?;
        pos += 512 + size.div_ceil(512) * 512;
        match kind {
            b'L' => long_name = Some(text(data)),
            b'x' | b'g' => {}
            b'0' | 0 => {
                let n = long_name.take().unwrap_or(name);
                if n.trim_start_matches("./") == wanted {
                    return Some(data.to_vec());
                }
            }
            _ => long_name = None,
        }
    }
    None
}

#[cfg(feature = "download")]
fn agent() -> ureq::Agent {
    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_with(name: &str, data: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{:011o}", data.len());
        h[124..135].copy_from_slice(size.as_bytes());
        h[156] = b'0';
        let mut t = h;
        t.extend_from_slice(data);
        t.resize(t.len().div_ceil(512) * 512 + 1024, 0);
        t
    }

    #[test]
    fn finds_a_member_and_ignores_others() {
        let t = tar_with("./tmp/export/enc.onnx", b"hello");
        assert_eq!(tar_member(&t, "tmp/export/enc.onnx").as_deref(), Some(&b"hello"[..]));
        assert!(tar_member(&t, "tmp/export/other.onnx").is_none());
        assert!(tar_member(&t[..100], "x").is_none());
    }

    #[test]
    fn catalogue_is_well_formed() {
        for m in catalogue() {
            assert!(!m.files.is_empty());
            for f in m.files {
                assert!(f.url.starts_with("https://"), "{}", m.id);
                assert_eq!(f.sha256.len(), 64);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                for mem in f.members {
                    assert_eq!(mem.sha256.len(), 64);
                    assert!(!mem.name.contains('/') && !mem.name.contains(".."));
                }
            }
        }
        assert!(find("deepfilternet3").is_some() && find("nope").is_none());
    }
}
