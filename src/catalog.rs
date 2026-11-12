//! Downloadable models, shared by the pill (Model library menu) and the helper (`--download`).
//! Every file has its published SHA-256 where the source provides one.

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Voice,
    Text,
    Vision,
    Engine, // runtimes and helper models the others need
}

pub struct File {
    pub url: &'static str,
    pub sha256: Option<&'static str>,
    pub unpack: bool, // .tar.bz2 / .zip, unpacked into the model folder
}

pub struct Model {
    pub id: &'static str, // folder under %LOCALAPPDATA%\ResourcePill\models
    pub kind: Kind,
    pub name: &'static str,
    pub about: &'static str,
    pub size_mb: u32,
    pub marker: &'static str, // file whose presence means "installed"
    pub files: &'static [File],
}


pub const MODELS: &[Model] = &[
    Model {
        id: "parakeet-v3",
        kind: Kind::Voice,
        name: "Parakeet TDT 0.6B v3",
        about: "fast, accurate · English + 24 European languages",
        size_mb: 465,
        marker: "tokens.txt",
        files: &[File { url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2", sha256: Some("1495e91b59d7fc53ddca91f3431758ec99f8fa253fdb0e1e851be0f72b14b0f9"), unpack: true }],
    },
    Model {
        id: "cohere-transcribe",
        kind: Kind::Voice,
        name: "Cohere Transcribe 2B",
        about: "most accurate · 14 languages · heavier on the CPU",
        size_mb: 1621,
        marker: "tokens.txt",
        files: &[File { url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01.tar.bz2", sha256: Some("bd582588d50685a795dcd2807ab77e11361b8312d96c53884682def45ab4206d"), unpack: true }],
    },
    Model {
        id: "qwen3.5-4b",
        kind: Kind::Text,
        name: "Qwen 3.5 4B",
        about: "meeting summaries + explain · quick, fits most GPUs",
        size_mb: 2614,
        marker: "Qwen3.5-4B-Q4_K_M.gguf",
        files: &[File { url: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/main/Qwen3.5-4B-Q4_K_M.gguf", sha256: Some("00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4"), unpack: false }],
    },
    Model {
        id: "qwen3.5-9b",
        kind: Kind::Text,
        name: "Qwen 3.5 9B",
        about: "best local summaries · needs ~7 GB of GPU memory",
        size_mb: 5417,
        marker: "Qwen3.5-9B-Q4_K_M.gguf",
        files: &[File { url: "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/resolve/main/Qwen3.5-9B-Q4_K_M.gguf", sha256: Some("03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8"), unpack: false }],
    },
    Model {
        id: "vad",
        kind: Kind::Engine,
        name: "Voice detection (Silero)",
        about: "required for recording",
        size_mb: 1,
        marker: "silero_vad.onnx",
        files: &[File { url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx", sha256: Some("9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6"), unpack: false }],
    },
    Model {
        id: "speaker",
        kind: Kind::Engine,
        name: "Speaker voiceprints (CAM++)",
        about: "required to tell speakers apart",
        size_mb: 28,
        marker: "3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx",
        files: &[File { url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx", sha256: None, unpack: false }],
    },
    Model {
        id: "voiceprint",
        kind: Kind::Engine,
        name: "Voiceprints (NVIDIA TitaNet-Large)",
        about: "sharper at telling similar voices apart",
        size_mb: 96,
        marker: "nemo_en_titanet_large.onnx",
        // GitHub publishes no digest for this file; pinned to the hash of the release as first downloaded
        files: &[File { url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_large.onnx", sha256: Some("d51abcf31717ef28162f26acb9d44dd4127c3d44c9b8624f699f3425daca8e77"), unpack: false }],
    },
    Model {
        id: "segmentation",
        kind: Kind::Engine,
        name: "Speaker turns (pyannote segmentation 3.0)",
        about: "tells voices apart even when people talk back-to-back",
        size_mb: 7,
        marker: "model.onnx",
        // GitHub publishes no digest for this file; pinned to the hash of the release as first downloaded
        files: &[File { url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2", sha256: Some("24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488"), unpack: true }],
    },
    Model {
        id: "llama.cpp",
        kind: Kind::Engine,
        name: "llama.cpp (NVIDIA CUDA)",
        about: "runs the text models, only while needed",
        size_mb: 170,
        marker: "llama-server.exe",
        files: &[File { url: "https://github.com/ggml-org/llama.cpp/releases/download/b12317/llama-b12317-bin-win-cuda-13.5-x64.zip", sha256: Some("8c9542f737e3cb2e6592069cb62ff5158cf39de747e225cdd4bb7e7528561969"), unpack: true }],
    },
    Model {
        id: "cudart",
        kind: Kind::Engine,
        name: "CUDA runtime for llama.cpp",
        about: "required by llama.cpp on NVIDIA GPUs",
        size_mb: 407,
        marker: "cudart64_13.dll",
        files: &[File { url: "https://github.com/ggml-org/llama.cpp/releases/download/b12317/cudart-llama-bin-win-cuda-13.5-x64.zip", sha256: Some("c2bf4cc202bfbaaa7a0b75f5082457c3811326947bb216059b72839d844e9cba"), unpack: true }],
    },
];

pub fn find(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

