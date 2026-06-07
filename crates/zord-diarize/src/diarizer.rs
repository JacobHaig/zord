//! sherpa-onnx-backed diarization: model management + offline diarizer + an
//! optional online (provisional) labeler.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig, SpeakerEmbeddingManager,
};

/// sherpa-onnx GitHub release tags hosting the pre-exported ONNX models.
const SEG_TAG: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models";
// NOTE: the release tag is misspelled "recongition" in the upstream repo — this
// is the real, working tag, not a typo on our end.
const EMB_TAG: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models";

/// Default cosine threshold for clustering / online matching. Lower = more
/// likely to merge speakers; higher = more likely to split.
const DEFAULT_THRESHOLD: f32 = 0.5;

/// Selectable speaker-segmentation model. All of these load through sherpa's
/// pyannote config — the Reverb models are pyannote-architecture variants
/// fine-tuned by Rev on ~26k hours of expertly-labeled real meetings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentationModel {
    /// pyannote segmentation-3.0 — the long-standing default (MIT license).
    Pyannote30,
    /// Rev Reverb v1 — pyannote-3.0 fine-tune, ~16% better word-diarization
    /// error than stock pyannote. Non-commercial license (Rev).
    ReverbV1,
    /// Rev Reverb v2 — WavLM-based, Rev's most accurate (~22% better than
    /// stock pyannote). Large download. Non-commercial license (Rev).
    ReverbV2,
}

impl SegmentationModel {
    pub const ALL: &'static [SegmentationModel] = &[
        SegmentationModel::Pyannote30,
        SegmentationModel::ReverbV1,
        SegmentationModel::ReverbV2,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SegmentationModel::Pyannote30 => "pyannote-3.0",
            SegmentationModel::ReverbV1 => "reverb-v1",
            SegmentationModel::ReverbV2 => "reverb-v2",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SegmentationModel::Pyannote30 => "pyannote 3.0 — default (MIT license)",
            SegmentationModel::ReverbV1 => {
                "Reverb v1 — more accurate, ~11 MB (non-commercial license)"
            }
            SegmentationModel::ReverbV2 => {
                "Reverb v2 — most accurate, ~254 MB (non-commercial license)"
            }
        }
    }

    /// Archive stem on the sherpa-onnx release == the extracted directory name
    /// (each contains `model.onnx`).
    fn stem(self) -> &'static str {
        match self {
            SegmentationModel::Pyannote30 => "sherpa-onnx-pyannote-segmentation-3-0",
            SegmentationModel::ReverbV1 => "sherpa-onnx-reverb-diarization-v1",
            SegmentationModel::ReverbV2 => "sherpa-onnx-reverb-diarization-v2",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.name() == s)
    }

    pub fn parse_or_default(s: &str) -> Self {
        Self::parse(s).unwrap_or(SegmentationModel::Pyannote30)
    }
}

/// Selectable speaker-embedding model. Segmentation (pyannote) is shared by all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingModel {
    /// NVIDIA NeMo TitaNet small — balanced default (English).
    TitanetSmall,
    /// WeSpeaker CAM++ (VoxCeleb) — lightest.
    CamPlusPlus,
    /// NVIDIA NeMo TitaNet large — best quality, heavier.
    TitanetLarge,
    /// 3D-Speaker CAM++ — robust general-purpose embedding.
    ThreeDSpeakerCampPlus,
    /// WeSpeaker ResNet34 (VoxCeleb, English).
    WespeakerResnet34,
}

impl EmbeddingModel {
    pub const ALL: &'static [EmbeddingModel] = &[
        EmbeddingModel::TitanetSmall,
        EmbeddingModel::CamPlusPlus,
        EmbeddingModel::TitanetLarge,
        EmbeddingModel::ThreeDSpeakerCampPlus,
        EmbeddingModel::WespeakerResnet34,
    ];

    pub fn name(self) -> &'static str {
        match self {
            EmbeddingModel::TitanetSmall => "nemo-titanet-small",
            EmbeddingModel::CamPlusPlus => "wespeaker-cam++",
            EmbeddingModel::TitanetLarge => "nemo-titanet-large",
            EmbeddingModel::ThreeDSpeakerCampPlus => "3dspeaker-campplus",
            EmbeddingModel::WespeakerResnet34 => "wespeaker-resnet34",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EmbeddingModel::TitanetSmall => "TitaNet small — balanced (default)",
            EmbeddingModel::CamPlusPlus => "WeSpeaker CAM++ — lightest, fastest",
            EmbeddingModel::TitanetLarge => "TitaNet large — best quality, slower",
            EmbeddingModel::ThreeDSpeakerCampPlus => "3D-Speaker CAM++ — robust, general-purpose",
            EmbeddingModel::WespeakerResnet34 => "WeSpeaker ResNet34 — solid mid-range (English)",
        }
    }

    pub fn size_label(self) -> &'static str {
        match self {
            EmbeddingModel::TitanetSmall => "~38 MB",
            EmbeddingModel::CamPlusPlus => "~28 MB",
            EmbeddingModel::TitanetLarge => "~97 MB",
            EmbeddingModel::ThreeDSpeakerCampPlus => "~27 MB",
            EmbeddingModel::WespeakerResnet34 => "~25 MB",
        }
    }

    /// The .onnx asset file name on the sherpa-onnx release.
    fn filename(self) -> &'static str {
        match self {
            EmbeddingModel::TitanetSmall => "nemo_en_titanet_small.onnx",
            EmbeddingModel::CamPlusPlus => "wespeaker_en_voxceleb_CAM++.onnx",
            EmbeddingModel::TitanetLarge => "nemo_en_titanet_large.onnx",
            EmbeddingModel::ThreeDSpeakerCampPlus => {
                "3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx"
            }
            EmbeddingModel::WespeakerResnet34 => "wespeaker_en_voxceleb_resnet34.onnx",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.name() == s)
    }

    pub fn parse_or_default(s: &str) -> Self {
        Self::parse(s).unwrap_or(EmbeddingModel::TitanetSmall)
    }

    /// Direct download URLs for manual fetch when the in-app download fails.
    /// Diarization needs two files: the chosen segmentation archive
    /// (`.tar.bz2`, extract into the models folder) and this embedding `.onnx`.
    pub fn download_urls(self, seg: SegmentationModel) -> Vec<String> {
        vec![
            format!("{SEG_TAG}/{}.tar.bz2", seg.stem()),
            format!("{EMB_TAG}/{}", self.filename()),
        ]
    }
}

fn models_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "Zord")
        .ok_or_else(|| anyhow!("could not resolve a data directory"))?;
    let dir = dirs.data_dir().join("models");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn segmentation_path(seg: SegmentationModel) -> Result<PathBuf> {
    Ok(models_dir()?.join(seg.stem()).join("model.onnx"))
}

fn segmentation_present(seg: SegmentationModel) -> bool {
    // Require a non-empty file: a truncated/interrupted download leaves a 0-byte
    // (or partial) model.onnx that "exists" but makes sherpa produce garbage or
    // no segments. Treat empty as absent so it gets re-fetched.
    segmentation_path(seg)
        .map(|p| p.exists() && std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false))
        .unwrap_or(false)
}

fn embedding_path(model: EmbeddingModel) -> Result<PathBuf> {
    Ok(models_dir()?.join(model.filename()))
}

fn embedding_present(model: EmbeddingModel) -> bool {
    embedding_path(model)
        .map(|p| p.exists() && std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false))
        .unwrap_or(false)
}

/// Whether *both* required models (the chosen segmentation + embedding) are
/// present locally, i.e. diarization can run without a download.
pub fn diar_models_present(seg: SegmentationModel, model: EmbeddingModel) -> bool {
    segmentation_present(seg) && embedding_present(model)
}

/// Catalog entry for the model-management UI: (id, label, size, present).
pub fn list_embedding_models(
    seg: SegmentationModel,
) -> Vec<(&'static str, &'static str, &'static str, bool)> {
    EmbeddingModel::ALL
        .iter()
        .map(|m| (m.name(), m.label(), m.size_label(), diar_models_present(seg, *m)))
        .collect()
}

/// Ensure both the chosen segmentation model and the chosen embedding model are
/// downloaded; returns their paths. `progress` → (downloaded, total) bytes.
pub fn ensure_diar_models(
    seg: SegmentationModel,
    model: EmbeddingModel,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(PathBuf, PathBuf)> {
    let seg = ensure_segmentation(seg, progress)?;
    let emb = ensure_embedding(model, progress)?;
    Ok((seg, emb))
}

fn ensure_segmentation(
    seg: SegmentationModel,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let path = segmentation_path(seg)?;
    if path.exists() {
        return Ok(path);
    }
    let dir = models_dir()?;
    let archive_url = format!("{SEG_TAG}/{}.tar.bz2", seg.stem());
    tracing::info!(%archive_url, "downloading diarization segmentation model (first run)");
    let tarball = dir.join(format!("{}.tar.bz2", seg.stem()));
    zord_net::download_to_file(&archive_url, &tarball, progress)?;

    unpack_segmentation_tarball(&tarball, &dir)?;

    if !path.exists() {
        anyhow::bail!("segmentation archive did not produce {path:?}");
    }
    Ok(path)
}

fn unpack_segmentation_tarball(tarball: &std::path::Path, dir: &std::path::Path) -> Result<()> {
    let file = std::fs::File::open(&tarball)?;
    // Cap total decompressed bytes so a malicious/compromised mirror can't ship
    // a high-ratio bzip2 bomb that fills the disk (segmentation models are ≤~50 MB).
    const MAX_UNPACK_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB
    let bz = std::io::Read::take(bzip2::read::BzDecoder::new(file), MAX_UNPACK_BYTES);
    tar::Archive::new(bz)
        .unpack(&dir)
        .context("unpacking segmentation archive")?;
    let _ = std::fs::remove_file(&tarball);
    Ok(())
}

fn ensure_embedding(
    model: EmbeddingModel,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let path = embedding_path(model)?;
    if path.exists() && std::fs::metadata(&path)?.len() > 0 {
        return Ok(path);
    }
    let url = format!("{EMB_TAG}/{}", model.filename());
    tracing::info!(%url, "downloading speaker-embedding model (first run)");
    zord_net::download_to_file(&url, &path, progress)?;
    Ok(path)
}

/// Delete a downloaded embedding model (the shared segmentation model is small
/// and left in place). No-op if absent.
pub fn delete_embedding(model: EmbeddingModel) -> Result<()> {
    let path = embedding_path(model)?;
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("deleting {path:?}"))?;
    }
    Ok(())
}

fn to_cfg_path(p: &std::path::Path) -> Option<String> {
    Some(p.to_string_lossy().into_owned())
}

/// Assemble the offline diarization config from the model paths and clustering knobs.
fn build_diar_config(
    seg: &std::path::Path,
    emb: &std::path::Path,
    num_speakers: Option<i32>,
    threshold: f32,
) -> OfflineSpeakerDiarizationConfig {
    OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: to_cfg_path(seg),
            },
            ..Default::default()
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: to_cfg_path(emb),
            ..Default::default()
        },
        clustering: FastClusteringConfig {
            num_clusters: num_speakers.unwrap_or(-1),
            threshold,
        },
        ..Default::default()
    }
}

/// One diarized span: a time range (session-relative ms) labeled with a 0-based
/// speaker index.
#[derive(Debug, Clone, Copy)]
pub struct DiarSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: i32,
}

/// Offline (accurate) diarizer. Segments → embeds → clusters a whole waveform.
pub struct Diarizer {
    inner: OfflineSpeakerDiarization,
}

impl Diarizer {
    /// Load the diarizer for the chosen segmentation + embedding models.
    /// `num_speakers` forces a fixed count (`None` = auto-detect). `threshold`
    /// controls clustering granularity.
    pub fn load(
        seg_model: SegmentationModel,
        model: EmbeddingModel,
        num_speakers: Option<i32>,
        threshold: f32,
    ) -> Result<Self> {
        let seg = segmentation_path(seg_model)?;
        let emb = embedding_path(model)?;
        if !seg.exists() || !emb.exists() {
            anyhow::bail!("diarization models are not downloaded yet");
        }
        let config = build_diar_config(&seg, &emb, num_speakers, threshold);
        let inner = OfflineSpeakerDiarization::create(&config)
            .ok_or_else(|| anyhow!("failed to create the diarizer (bad/missing models?)"))?;
        Ok(Self { inner })
    }

    /// Sample rate the segmentation model expects (typically 16 kHz).
    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate().max(0) as u32
    }

    /// Diarize a full mono waveform (at [`sample_rate`]). Returns speaker-labeled
    /// spans sorted by start time.
    pub fn diarize(&self, samples: &[f32]) -> Result<Vec<DiarSegment>> {
        let result = self
            .inner
            .process(samples)
            .ok_or_else(|| anyhow!("diarization produced no result"))?;
        Ok(result
            .sort_by_start_time()
            .into_iter()
            .map(|s| DiarSegment {
                start_ms: (s.start.max(0.0) * 1000.0) as u64,
                end_ms: (s.end.max(0.0) * 1000.0) as u64,
                speaker: s.speaker,
            })
            .collect())
    }
}

/// Online, *provisional* speaker labeler used during recording. Each speech
/// chunk is embedded and matched against previously-seen speakers by cosine
/// similarity; unmatched chunks mint a new speaker index. These labels are
/// rough and always superseded by the offline [`Diarizer`] pass at stop.
pub struct LiveLabeler {
    extractor: SpeakerEmbeddingExtractor,
    manager: SpeakerEmbeddingManager,
    threshold: f32,
    next: i32,
}

impl LiveLabeler {
    pub fn new(model: EmbeddingModel, threshold: f32) -> Result<Self> {
        let emb = embedding_path(model)?;
        if !emb.exists() {
            anyhow::bail!("speaker-embedding model is not downloaded yet");
        }
        let extractor = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: to_cfg_path(&emb),
            ..Default::default()
        })
        .ok_or_else(|| anyhow!("failed to create the embedding extractor"))?;
        let manager = SpeakerEmbeddingManager::create(extractor.dim())
            .ok_or_else(|| anyhow!("failed to create the speaker manager"))?;
        Ok(Self {
            extractor,
            manager,
            threshold,
            next: 0,
        })
    }

    pub fn new_default(model: EmbeddingModel) -> Result<Self> {
        Self::new(model, DEFAULT_THRESHOLD)
    }

    /// Assign a provisional 0-based speaker index to a mono chunk at
    /// `sample_rate`. Returns `None` if the chunk is too short to embed.
    pub fn label(&mut self, samples: &[f32], sample_rate: u32) -> Option<i32> {
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(sample_rate as i32, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        let embedding = self.extractor.compute(&stream)?;
        if let Some(name) = self.manager.search(&embedding, self.threshold) {
            return name.parse::<i32>().ok();
        }
        let idx = self.next;
        self.next += 1;
        self.manager.add(&idx.to_string(), &embedding);
        Some(idx)
    }
}
