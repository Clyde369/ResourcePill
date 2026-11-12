#!/usr/bin/env bash
# Downloads ResourcePill's transcription + summary models into %LOCALAPPDATA%\ResourcePill\models.
# Every file is checked against its published SHA-256 (where one exists). Safe to re-run: finished
# files are skipped and partial downloads resume.
set -euo pipefail

ROOT="$(cygpath -u "$LOCALAPPDATA")/ResourcePill/models"
mkdir -p "$ROOT"

fetch() { # dir url sha256|- [extract]
    local dir="$ROOT/$1" url="$2" sha="$3" extract="${4:-}"
    local file="$dir/$(basename "$url")"
    mkdir -p "$dir"
    if [ -f "$dir/.ok" ]; then echo "ok    $1"; return; fi
    echo "get   $1 <- $url"
    curl -fL --retry 5 --retry-delay 3 -C - -o "$file" "$url"
    if [ "$sha" != "-" ]; then
        echo "$sha  $file" | sha256sum -c --quiet - || { echo "CHECKSUM MISMATCH: $file"; rm -f "$file"; exit 1; }
    fi
    case "$extract" in
        tar) tar -xjf "$file" -C "$dir" --strip-components=1 && rm -f "$file" ;;
        zip) unzip -oq "$file" -d "$dir" && rm -f "$file" ;;
    esac
    touch "$dir/.ok"
    echo "done  $1"
}

GH=https://github.com
HF=https://huggingface.co

# speech detection + speaker voiceprints (small, first)
fetch vad "$GH/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx" 9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6
fetch speaker "$GH/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx" -
# speech-to-text
fetch parakeet-v3 "$GH/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2" 1495e91b59d7fc53ddca91f3431758ec99f8fa253fdb0e1e851be0f72b14b0f9 tar
# summary runtime (llama.cpp, CUDA 13.5 build; started only while summarising)
fetch llama.cpp "$GH/ggml-org/llama.cpp/releases/download/b12317/llama-b12317-bin-win-cuda-13.5-x64.zip" 8c9542f737e3cb2e6592069cb62ff5158cf39de747e225cdd4bb7e7528561969 zip
fetch cudart "$GH/ggml-org/llama.cpp/releases/download/b12317/cudart-llama-bin-win-cuda-13.5-x64.zip" c2bf4cc202bfbaaa7a0b75f5082457c3811326947bb216059b72839d844e9cba zip
# summary models
fetch qwen3.5-4b "$HF/unsloth/Qwen3.5-4B-GGUF/resolve/main/Qwen3.5-4B-Q4_K_M.gguf" 00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4
fetch qwen3.5-9b "$HF/unsloth/Qwen3.5-9B-GGUF/resolve/main/Qwen3.5-9B-Q4_K_M.gguf" 03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8
# most accurate speech-to-text (large)
fetch cohere-transcribe "$GH/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01.tar.bz2" bd582588d50685a795dcd2807ab77e11361b8312d96c53884682def45ab4206d tar

echo "ALL MODELS READY in $ROOT"
