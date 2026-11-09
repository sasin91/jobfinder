//! Which GGUF model this computer runs, and how many of its layers go on the graphics card.
//! Weighed as penny weighs its classifier model by VRAM, but by each file's own size: a model
//! that fits in the card's memory with room for its context runs there whole; one that doesn't
//! is split between the card and the computer's memory; one too big for both isn't run.
//!
//! The card is only used when the llama-server build beside the app can drive it: like penny, an
//! NVIDIA or AMD card without its compute library is said, instead of quietly running on the CPU.

use std::path::Path;

use super::gguf::GgufFile;
use super::hardware::{GIB, GpuVendor, Inventory};

/// Room a model needs beyond its file: the KV cache of CONTEXT_TOKENS and llama.cpp's compute
/// buffers, as an 8B-class model at f16 takes them.
pub const CONTEXT_HEADROOM_BYTES: u64 = 2 * GIB;

/// Graphics memory left for Windows' desktop and the browser.
pub const DESKTOP_VRAM_BYTES: u64 = GIB;

/// The share of the computer's memory a split model may take, in percent.
pub const MEMORY_SHARE_PERCENT: u64 = 50;

/// How a model fits this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Whole on the graphics card.
    Gpu,
    /// gpu_layers of its layers on the card, the rest in memory.
    Split { gpu_layers: u32 },
    /// In memory, on the CPU.
    Memory,
    /// Too big for this computer.
    TooBig,
}

impl Fit {
    /// What the API key page says of it.
    pub fn label(self) -> String {
        match self {
            Fit::Gpu => "runs on the graphics card".to_string(),
            Fit::Split { gpu_layers } => format!("split: {gpu_layers} layers on the graphics card, the rest in memory"),
            Fit::Memory => "runs in memory, on the CPU (slow)".to_string(),
            Fit::TooBig => "too big for this computer".to_string(),
        }
    }

    /// Better fits first: whole on the card, then split, then memory.
    fn rank(self) -> u8 {
        match self {
            Fit::Gpu => 0,
            Fit::Split { .. } => 1,
            Fit::Memory => 2,
            Fit::TooBig => 3,
        }
    }
}

/// The compute libraries a llama.cpp release ships for each kind of card, as the file stems in
/// its zip (ggml-cuda.dll on Windows, libggml-cuda.so elsewhere).
const CUDA: &str = "ggml-cuda";
const VULKAN: &str = "ggml-vulkan";
const HIP: &str = "ggml-hip";
const SYCL: &str = "ggml-sycl";

/// The compute libraries that drive a vendor's cards.
fn libraries_for(vendor: GpuVendor) -> &'static [&'static str] {
    match vendor {
        GpuVendor::Nvidia => &[CUDA, VULKAN],
        GpuVendor::Amd => &[HIP, VULKAN],
        GpuVendor::Intel => &[SYCL, VULKAN],
        GpuVendor::Other => &[VULKAN],
    }
}

/// The compute libraries in llama-server's folder.
pub fn compute_libraries(folder: &Path) -> Vec<&'static str> {
    let present = |stem: &str| {
        let windows = folder.join(format!("{stem}.dll"));
        let unix = folder.join(format!("lib{stem}.so"));
        windows.is_file() || unix.is_file()
    };
    [CUDA, VULKAN, HIP, SYCL]
        .into_iter()
        .filter(|stem| present(stem))
        .collect()
}

/// Whether the card can be used: Ok(true) with a compute library for it, Ok(false) without a card,
/// Err with what to download when an NVIDIA or AMD card has none. An Intel or other card without
/// one is skipped quietly: it is usually built in and slower than the CPU's own.
pub fn card_usable(inventory: &Inventory, libraries: &[&str]) -> Result<bool, String> {
    let Some(gpu) = &inventory.gpu else {
        return Ok(false);
    };
    let wanted = libraries_for(gpu.vendor);
    if wanted.iter().any(|stem| libraries.contains(stem)) {
        return Ok(true);
    }
    match gpu.vendor {
        GpuVendor::Nvidia => Err(format!(
            "{} is an NVIDIA card, but the runtime has no CUDA or Vulkan library. Download the matching runtime in Models, or install a compatible llama-server on PATH.",
            gpu.name
        )),
        GpuVendor::Amd => Err(format!(
            "{} is an AMD card, but the runtime has no HIP or Vulkan library. Download the matching runtime in Models, or install a compatible llama-server on PATH.",
            gpu.name
        )),
        GpuVendor::Intel | GpuVendor::Other => Ok(false),
    }
}

/// The card's memory a model may take.
fn usable_vram(inventory: &Inventory) -> u64 {
    inventory.vram_bytes().saturating_sub(DESKTOP_VRAM_BYTES)
}

/// The computer's memory a model may take.
fn usable_memory(inventory: &Inventory) -> u64 {
    inventory.memory_bytes / 100 * MEMORY_SHARE_PERCENT
}

/// The memory a model takes running: its file and its context.
fn needed(model: &GgufFile) -> u64 {
    model.bytes + CONTEXT_HEADROOM_BYTES
}

/// How model fits, with the card used or not.
pub fn fit(model: &GgufFile, inventory: &Inventory, card_usable: bool) -> Fit {
    let vram = if card_usable { usable_vram(inventory) } else { 0 };
    let memory = usable_memory(inventory);
    let needed = needed(model);
    if vram > 0 && needed <= vram {
        return Fit::Gpu;
    }
    if needed > vram + memory {
        return Fit::TooBig;
    }
    match split_layers(model, vram) {
        Some(gpu_layers) if gpu_layers > 0 => Fit::Split { gpu_layers },
        // No layers on the card: it all has to fit in memory alone.
        _ if needed <= memory => Fit::Memory,
        _ => Fit::TooBig,
    }
}

/// The layers that fit in vram after the context, by each layer's even share of the file.
fn split_layers(model: &GgufFile, vram: u64) -> Option<u32> {
    let layers = u64::from(model.layers?);
    let room = vram.checked_sub(CONTEXT_HEADROOM_BYTES)?;
    let per_layer = model.bytes.checked_div(layers)?;
    let fitting = room.checked_div(per_layer)?.min(layers);
    u32::try_from(fitting).ok()
}

/// The model to run and how: the one named (its file name) when there is a name, else the
/// biggest of the best fit; Err says why none can run.
pub fn choose<'a>(
    models: &'a [GgufFile],
    named: &str,
    inventory: &Inventory,
    card_usable: bool,
) -> Result<(&'a GgufFile, Fit), String> {
    if models.is_empty() {
        return Err("There's no .gguf model in the models folder yet.".to_string());
    }
    if !named.is_empty() {
        let model = models
            .iter()
            .find(|model| model.name.eq_ignore_ascii_case(named))
            .ok_or_else(|| format!("There's no {named} in the models folder."))?;
        let fit = fit(model, inventory, card_usable);
        if fit == Fit::TooBig {
            return Err(format!("{} is too big for this computer ({inventory}).", model.name));
        }
        return Ok((model, fit));
    }
    models
        .iter()
        .map(|model| (model, fit(model, inventory, card_usable)))
        .filter(|(_, fit)| *fit != Fit::TooBig)
        .min_by(|(a, a_fit), (b, b_fit)| a_fit.rank().cmp(&b_fit.rank()).then(b.bytes.cmp(&a.bytes)))
        .ok_or_else(|| format!("Every model in the models folder is too big for this computer ({inventory})."))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::llama::hardware::Gpu;

    fn model(name: &str, gib_tenths: u64, layers: u32) -> GgufFile {
        GgufFile {
            name: name.to_string(),
            path: PathBuf::from(name),
            bytes: gib_tenths * GIB / 10,
            layers: Some(layers),
        }
    }

    fn computer(memory_gib: u64, vendor: GpuVendor, vram_gib: u64) -> Inventory {
        Inventory {
            memory_bytes: memory_gib * GIB,
            gpu: Some(Gpu {
                name: "a card".to_string(),
                vendor,
                vram_bytes: vram_gib * GIB,
            }),
        }
    }

    #[test]
    fn a_model_that_fits_the_card_runs_there() {
        let pc = computer(32, GpuVendor::Nvidia, 12);
        // 4.7 + 2 GB of context in 11 GB.
        assert_eq!(fit(&model("qwen3-8b-q4.gguf", 47, 36), &pc, true), Fit::Gpu);
    }

    #[test]
    fn a_model_too_big_for_the_card_is_split_by_its_layers() {
        let pc = computer(32, GpuVendor::Nvidia, 12);
        // 18 GB over 64 layers is 0.28 GB a layer; 11 - 2 GB leaves room for 32 of them.
        assert_eq!(
            fit(&model("qwen3-32b-q4.gguf", 180, 64), &pc, true),
            Fit::Split { gpu_layers: 32 }
        );
    }

    #[test]
    fn without_a_usable_card_a_model_runs_in_memory_or_not_at_all() {
        let pc = computer(32, GpuVendor::Nvidia, 12);
        assert_eq!(fit(&model("qwen3-8b-q4.gguf", 47, 36), &pc, false), Fit::Memory);
        // 16 GB is half the memory; 18 + 2 GB doesn't fit it.
        assert_eq!(fit(&model("qwen3-32b-q4.gguf", 180, 64), &pc, false), Fit::TooBig);
    }

    #[test]
    fn a_model_whose_layers_are_unknown_runs_in_memory_only_if_it_fits_there() {
        let pc = computer(16, GpuVendor::Nvidia, 24);
        let unknown_layers = |gib_tenths| GgufFile {
            layers: None,
            ..model("no-header.gguf", gib_tenths, 0)
        };
        // 25 + 2 GB fits 23 GB of card and 8 GB of memory together, but can't be split by layers
        // nobody knows, and is too big for the 8 GB alone.
        assert_eq!(fit(&unknown_layers(250), &pc, true), Fit::TooBig);
        assert_eq!(
            fit(&unknown_layers(50), &computer(16, GpuVendor::Nvidia, 0), true),
            Fit::Memory
        );
    }

    #[test]
    fn the_biggest_model_that_runs_on_the_card_is_chosen() {
        let pc = computer(32, GpuVendor::Nvidia, 12);
        let models = [
            model("phi-mini.gguf", 23, 32),
            model("qwen3-8b-q4.gguf", 47, 36),
            model("qwen3-32b-q4.gguf", 180, 64),
            model("llama-70b-q4.gguf", 400, 80),
        ];
        let (chosen, how) = choose(&models, "", &pc, true).unwrap();
        assert_eq!((chosen.name.as_str(), how), ("qwen3-8b-q4.gguf", Fit::Gpu));

        let (chosen, how) = choose(&models, "QWEN3-32B-Q4.gguf", &pc, true).unwrap();
        assert_eq!(
            (chosen.name.as_str(), how),
            ("qwen3-32b-q4.gguf", Fit::Split { gpu_layers: 32 })
        );

        assert_eq!(
            choose(&models, "llama-70b-q4.gguf", &pc, true).unwrap_err(),
            "llama-70b-q4.gguf is too big for this computer (32.0 GB memory, a card with 12.0 GB)."
        );
        assert_eq!(
            choose(&models, "mistral.gguf", &pc, true).unwrap_err(),
            "There's no mistral.gguf in the models folder."
        );
    }

    #[test]
    fn a_card_without_its_compute_library_is_said() {
        let nvidia = computer(32, GpuVendor::Nvidia, 12);
        assert_eq!(card_usable(&nvidia, &[CUDA]), Ok(true));
        assert_eq!(card_usable(&nvidia, &[VULKAN]), Ok(true));
        assert!(card_usable(&nvidia, &[]).unwrap_err().contains("no CUDA or Vulkan"));
        assert!(
            card_usable(&computer(32, GpuVendor::Amd, 16), &[CUDA])
                .unwrap_err()
                .contains("no HIP")
        );
        assert_eq!(card_usable(&computer(16, GpuVendor::Intel, 2), &[]), Ok(false));
        assert_eq!(card_usable(&Inventory::default(), &[CUDA]), Ok(false));
    }
}
