//! The optional models: where they come from, their exact sizes and SHA-256, and under which terms.
//!
//! **None of them is part of FilmCraft.** A model is downloaded only when the user asks (`ml.install`),
//! after the licence has been shown and accepted (`acceptLicense:true`, or the dialog of the window); every
//! file is pinned to a revision, its size and its SHA-256, and a file that fails either check is deleted.
//! A model folder is `<models dir>/<id>/`, the models dir being `FILMCRAFT_ML_MODELS_DIR` or
//! `<data dir>/models/ml`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub use filmcraft_mlfetch::{DownloadError, FileSpec, Options, Progress};

/// One downloadable model.
#[derive(Clone, Debug)]
pub struct Model {
    pub id: &'static str,
    pub title: &'static str,
    /// What FilmCraft uses it for.
    pub use_for: &'static str,
    pub files: &'static [FileSpec],
    /// Base URLs: a file is at `<mirror>/<name>`; tried in order, each file is checked against its SHA-256.
    pub mirrors: &'static [&'static str],
    /// The licence of the weights as their publisher states it.
    pub licence: &'static str,
    pub licence_url: &'static str,
    pub source_url: &'static str,
    /// Whether the licence limits use (research / non-commercial / GPL): shown in the dialog.
    pub personal_only: bool,
}

impl Model {
    pub fn size(&self) -> u64 {
        self.files.iter().filter_map(|f| f.size).sum()
    }
    pub fn dir(&self, models: &Path) -> PathBuf {
        models.join(self.id)
    }
    /// All files present with their pinned size (contents were verified when downloaded).
    pub fn installed(&self, models: &Path) -> bool {
        let d = self.dir(models);
        self.files.iter().all(|f| std::fs::metadata(d.join(f.name)).is_ok_and(|m| Some(m.len()) == f.size))
    }
}

const fn spec(name: &'static str, size: u64, sha256: &'static str) -> FileSpec {
    FileSpec { name, size: Some(size), sha256: Some(sha256), max: size }
}

pub static RIFE: Model = Model {
    id: "rife-v4",
    title: "RIFE v4 (frame interpolation)",
    use_for: "clip.interpolate: slow motion and frame-rate conversion",
    files: &[spec("flownet.safetensors", 12_166_924, "4b953bee2017bea5e71aae69c36a9caf316071788f9956b7099c015c75ee6af7")],
    mirrors: &["https://huggingface.co/TensorForger/RIFE-safetensors/resolve/78a62b7c2dd910536432d6c2c3a25e76f14fbf78"],
    licence: "MIT, (c) Megvii Inc. (ECCV2022-RIFE); safetensors re-host by TensorForger on Hugging Face. Training data (Vimeo90K) not re-read.",
    licence_url: "https://huggingface.co/TensorForger/RIFE-safetensors/blob/main/LICENSE",
    source_url: "https://huggingface.co/TensorForger/RIFE-safetensors",
    personal_only: false,
};

pub static BEAT_THIS: Model = Model {
    id: "beat-this-final0",
    title: "Beat This! (beat and downbeat tracking)",
    use_for: "sequence.markBeats / cutOnBeats with model:\"beat-this\"",
    files: &[
        spec("model.safetensors", 81_029_488, "f758c77ba5d862cad3718a95705964e9af55298b1fb3544c62c9d79184ea0f2e"),
        spec("mel-filterbank.bin", 262_656, "1ee975d96f44ccf2c3bfe37825c1c1f0b089f5703c7a12a84b1f0a3bce004533"),
    ],
    mirrors: &[
        "https://huggingface.co/safe-models/beat-this-final0/resolve/39ec64821baf38aa16fafa500a91379c16db0062",
        "https://huggingface.co/musetric/beat-this-onnx/resolve/25308706dda545c0d5950065bb0604ad71bcf1f5",
    ],
    licence: "MIT (CPJKU/beat_this, checkpoint final0, Foscarin, Schlüter, Widmer 2024). The authors warn that some of the training material is under limited licences; re-hosted on Hugging Face by third parties (safe-models, musetric).",
    licence_url: "https://github.com/CPJKU/beat_this/blob/main/LICENSE",
    source_url: "https://github.com/CPJKU/beat_this",
    personal_only: false,
};

pub static SPAN2X: Model = Model {
    id: "span-2x",
    title: "SPAN 2x NomosUni (super-resolution)",
    use_for: "clip.upscale: 2x enlargement of video frames",
    files: &[spec("2xNomosUni_span_multijpg.safetensors", 4_461_056, "bee2a9c082f2b8f6e7f5db504b36593c24a1a959511f587114c399ca58b9c92c")],
    mirrors: &["https://huggingface.co/Phips/2xNomosUni_span_multijpg/resolve/main"],
    licence: "CC BY 4.0 (Philip Hofmann, declared on the model page); network SPAN, Apache-2.0 (hongyuanyu/SPAN); trained on the author's NomosUni dataset",
    licence_url: "https://creativecommons.org/licenses/by/4.0/",
    source_url: "https://huggingface.co/Phips/2xNomosUni_span_multijpg",
    personal_only: false,
};

pub static SIGLIP2: Model = Model {
    id: "siglip2-base-224",
    title: "SigLIP 2 base 224 (image and text embeddings)",
    use_for: "media.rushIndex / media.rushSearch: find a shot in hours of rushes by describing it",
    files: &[
        spec("config.json", 253, "fe8b5fe6d5734360678fd71c11c21e1ea3364bd8598d34295d9206335973ffd7"),
        spec("preprocessor_config.json", 394, "9b36b57ebaf20f09bf4c22100ccc21877ea6bfe5aead0c00c59f8af8ccefacfc"),
        spec("tokenizer.json", 34_363_039, "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322"),
        spec("model.safetensors", 1_500_800_904, "612923381c76ec5a9bed335d1c48827e3f2e506ac31b044b63b2031fadee6a0b"),
    ],
    mirrors: &["https://huggingface.co/google/siglip2-base-patch16-224/resolve/75de2d55ec2d0b4efc50b3e9ad70dba96a7b2fa2"],
    licence: "Apache-2.0 (Google, model card read). The tokenizer is Gemma's: check its terms before redistributing the model; training data not re-read.",
    licence_url: "https://huggingface.co/google/siglip2-base-patch16-224",
    source_url: "https://huggingface.co/google/siglip2-base-patch16-224",
    personal_only: false,
};

pub static HTDEMUCS: Model = Model {
    id: "htdemucs",
    title: "HT Demucs (voice / music separation)",
    use_for: "audio.separate: isolate the voice, remove the music, split drums / bass / other / vocals",
    files: &[spec("955717e8-8726e21a.th", 84_141_911, "8726e21a993978c7ba086d3872e7608d7d5bfca646ca4aca459ffda844faa8b4")],
    mirrors: &["https://dl.fbaipublicfiles.com/demucs/hybrid_transformer"],
    licence: "MIT (Meta Platforms, facebookresearch/demucs: code and the released weights). Trained on MUSDB18-HQ and 800 extra songs: the data licences were not re-read.",
    licence_url: "https://github.com/facebookresearch/demucs/blob/main/LICENSE",
    source_url: "https://github.com/facebookresearch/demucs",
    personal_only: false,
};

pub static RVM: Model = Model {
    id: "rvm-mobilenetv3",
    title: "Robust Video Matting (person matte)",
    use_for: "clip.matte: the alpha matte of the person in a video, stable from frame to frame",
    files: &[spec("rvm_mobilenetv3.pth", 15_217_721, "3c7c1d92033f7c38d6577c481d13a195d7d80a159b960f4f3119ac7b534cf4f8")],
    mirrors: &["https://huggingface.co/SJTU-TES/RobustVideoMatting/resolve/3c88b194e7e4efe55e6fe0f81088419df264e8b4"],
    licence: "GPL-3.0 (PeterL1n/RobustVideoMatting, code and weights; Hugging Face re-host by SJTU-TES). PERSONAL USE: the weights are not covered by FilmCraft's own licence and must not be redistributed with it.",
    licence_url: "https://github.com/PeterL1n/RobustVideoMatting/blob/master/LICENSE",
    source_url: "https://github.com/PeterL1n/RobustVideoMatting",
    personal_only: true,
};

pub fn all() -> Vec<&'static Model> {
    vec![&RIFE, &BEAT_THIS, &SPAN2X, &SIGLIP2, &HTDEMUCS, &RVM]
}

pub fn find(id: &str) -> Option<&'static Model> {
    all().into_iter().find(|m| m.id == id)
}

/// Download the missing files of `m` into `<models>/<id>/`.
pub fn download(m: &Model, models: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(&Progress)) -> Result<(), DownloadError> {
    let mirrors: Vec<String> = filmcraft_mlfetch::mirrors(None, None, m.mirrors);
    filmcraft_mlfetch::download(m.files, &mirrors, &m.dir(models), &Options::default(), cancel, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_pinned() {
        let all = all();
        let mut ids: Vec<&str> = all.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "unique ids");
        for m in all {
            assert!(!m.mirrors.is_empty() && m.mirrors.iter().all(|u| u.starts_with("https://")), "{}: https mirrors", m.id);
            assert!(m.licence.len() > 20 && m.licence_url.starts_with("https://") && m.source_url.starts_with("https://"), "{}: licence stated", m.id);
            for f in m.files {
                let (sha, size) = (f.sha256.unwrap_or_default(), f.size.unwrap_or(0));
                assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()), "{}/{}: SHA-256 pinned", m.id, f.name);
                assert!(size > 0 && f.max == size, "{}/{}: exact size pinned", m.id, f.name);
                assert!(!f.name.contains('/') && !f.name.contains(".."), "{}: file name stays in the model folder", m.id);
            }
        }
        // a model that restricts use says so
        assert!(find("rvm-mobilenetv3").is_some_and(|m| m.personal_only && m.licence.contains("GPL")));
        assert!(find("nope").is_none());
    }

    #[test]
    fn installed_needs_every_file_at_its_size() {
        let dir = std::env::temp_dir().join(format!("fc-ml-cat-{}", std::process::id()));
        let m = &BEAT_THIS;
        assert!(!m.installed(&dir), "nothing there");
        std::fs::create_dir_all(m.dir(&dir)).unwrap();
        for f in m.files {
            std::fs::write(m.dir(&dir).join(f.name), b"short").unwrap();
        }
        assert!(!m.installed(&dir), "a file of the wrong size is not an installed file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
