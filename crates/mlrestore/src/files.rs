//! The model files, where they come from and under which terms. **None of them is part of
//! LightCraft**: they are downloaded only when the user asks, after a dialog naming the licence
//! (see `docs/ai-restore.md`), from the places their authors publish them.

pub use filmcraft_mlfetch::FileSpec;

/// A downloadable model.
#[derive(Clone, Debug)]
pub struct Model {
    pub title: &'static str,
    pub file: FileSpec,
    /// Base URL: the file is at `<base>/<file.name>`.
    pub mirror: &'static str,
    /// Licence of the weights as their publisher states it, and where.
    pub licence: &'static str,
    pub licence_url: &'static str,
    pub source_url: &'static str,
}

/// SCUNet "real" (blind, PSNR-oriented) colour denoiser, the authors' own release.
pub const SCUNET: Model = Model {
    title: "SCUNet (blind denoising)",
    file: FileSpec {
        name: "scunet_color_real_psnr.pth",
        size: Some(71_982_841),
        sha256: Some("fa78899ba2caec9d235a900e91d96c689da71c42029230c2028b00f09f809c2e"),
        max: 71_982_841,
    },
    mirror: "https://github.com/cszn/KAIR/releases/download/v1.0",
    licence: "Code Apache-2.0 (cszn/SCUNet); the weights are attached to the authors' MIT-licensed KAIR repository \
              and carry no licence of their own; trained on synthetic noise added to public photo datasets",
    licence_url: "https://github.com/cszn/SCUNet/blob/main/LICENSE",
    source_url: "https://github.com/cszn/SCUNet",
};

/// SPAN 2x "NomosUni" universal upscaler by Philip Hofmann.
pub const SPAN2X: Model = Model {
    title: "SPAN 2x (super-resolution)",
    file: FileSpec {
        name: "2xNomosUni_span_multijpg.safetensors",
        size: Some(4_461_056),
        sha256: Some("bee2a9c082f2b8f6e7f5db504b36593c24a1a959511f587114c399ca58b9c92c"),
        max: 4_461_056,
    },
    mirror: "https://huggingface.co/Phips/2xNomosUni_span_multijpg/resolve/main",
    licence: "CC BY 4.0 (Philip Hofmann, declared on the model page); network SPAN, Apache-2.0 (hongyuanyu/SPAN); \
              trained on the author's NomosUni dataset",
    licence_url: "https://creativecommons.org/licenses/by/4.0/",
    source_url: "https://huggingface.co/Phips/2xNomosUni_span_multijpg",
};
