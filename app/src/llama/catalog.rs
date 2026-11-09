//! The models the setup wizard offers to download: Qwen's own GGUF builds of Qwen3, from small to
//! large, and two picked by hand for a task (account::task_model): Gemma 4 E4B for reading Danish
//! job posts, and Munin, a Danish-tuned Qwen3.5. The model bake-off (2026-11-02) found Qwen3 4B
//! reads CVs and judges as well as the bigger ones, so it is the one suggested. Each is weighed with model_choice before it is downloaded, by the size
//! Hugging Face lists for it.

use std::path::Path;

use super::gguf::GgufFile;

/// Bytes in a decimal gigabyte, which Hugging Face lists file sizes in.
const GB: u64 = 1_000_000_000;

/// A model to download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogModel {
    /// The Hugging Face repository.
    pub repo: &'static str,
    /// The file in it, which is also its name in the models folder.
    pub file: &'static str,
    /// What the wizard calls it.
    pub label: &'static str,
    /// Its size as Hugging Face lists it, in bytes.
    pub bytes: u64,
    /// Its transformer layers (its config.json num_hidden_layers).
    pub layers: u32,
    /// Whether the wizard may recommend it by how it fits this computer, as the one model every
    /// task runs on; false for a model the user picks for a task by hand.
    pub fits_every_task: bool,
}

impl CatalogModel {
    /// Where it downloads from.
    pub fn url(&self) -> String {
        format!("https://huggingface.co/{}/resolve/main/{}", self.repo, self.file)
    }

    /// As the weighing sees it before it is downloaded.
    pub fn as_gguf(&self, models_folder: &Path) -> GgufFile {
        GgufFile {
            name: self.file.to_string(),
            path: models_folder.join(self.file),
            bytes: self.bytes,
            layers: Some(self.layers),
        }
    }
}

/// Qwen3 4B's file: the smallest good model, and what reads CVs and judges a match by default.
pub const QWEN3_4B: &str = "Qwen3-4B-Q4_K_M.gguf";

/// Gemma 4 E4B's file: what reads job posts by default.
pub const GEMMA_4_E4B: &str = "gemma-4-E4B-it-Q4_K_M.gguf";
/// IBM's official Q4_K_M artifact; form-answer candidate confirmed by user experience.
pub const GRANITE_4_1_3B: &str = "granite-4.1-3b-Q4_K_M.gguf";

/// The models offered, smallest first.
pub const MODELS: [CatalogModel; 7] = [
    CatalogModel {
        repo: "Qwen/Qwen3-1.7B-GGUF",
        file: "Qwen3-1.7B-Q8_0.gguf",
        label: "Qwen3 1.7B: small and quick, for a computer without a graphics card",
        // 1.83 GB.
        bytes: 183 * GB / 100,
        layers: 28,
        fits_every_task: true,
    },
    CatalogModel {
        repo: "ibm-granite/granite-4.1-3b-GGUF",
        file: GRANITE_4_1_3B,
        label: "IBM Granite 4.1 3B: compact candidate for application form answers",
        bytes: 2_099_501_664,
        layers: 40,
        fits_every_task: false,
    },
    CatalogModel {
        repo: "Qwen/Qwen3-4B-GGUF",
        file: QWEN3_4B,
        label: "Qwen3 4B: compact baseline for CV reading and matching",
        // 2.5 GB.
        bytes: 25 * GB / 10,
        layers: 36,
        fits_every_task: true,
    },
    CatalogModel {
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        file: GEMMA_4_E4B,
        label: "Gemma 4 E4B: candidate for reading Danish job posts",
        // 4.98 GB.
        bytes: 4_977_171_584,
        layers: 42,
        fits_every_task: false,
    },
    CatalogModel {
        repo: "Qwen/Qwen3-8B-GGUF",
        file: "Qwen3-8B-Q4_K_M.gguf",
        label: "Qwen3 8B: bigger and slower than Qwen3 4B, and no better in the bake-off",
        // 5.03 GB.
        bytes: 503 * GB / 100,
        layers: 36,
        fits_every_task: true,
    },
    CatalogModel {
        repo: "mradermacher/munin-qwen3.5-9B-GGUF",
        file: "munin-qwen3.5-9B.Q4_K_M.gguf",
        label: "Munin Qwen3.5 9B: Danish-tuned writing candidate; needs more memory",
        // 5.78 GB.
        bytes: 5_780_090_944,
        layers: 32,
        fits_every_task: false,
    },
    CatalogModel {
        repo: "Qwen/Qwen3-14B-GGUF",
        file: "Qwen3-14B-Q4_K_M.gguf",
        label: "Qwen3 14B: larger alternative; evaluate before switching",
        // 9.0 GB.
        bytes: 90 * GB / 10,
        layers: 40,
        fits_every_task: true,
    },
];

/// The smallest catalog model whose answers are good enough for the AI match and the writing.
/// A computer that can only run smaller ones on its graphics card is pointed to an OpenAI or
/// Claude key instead.
pub const SMALLEST_GOOD_MODEL: &str = QWEN3_4B;

/// Whether model is SMALLEST_GOOD_MODEL or bigger.
pub fn is_good_enough(model: &CatalogModel) -> bool {
    // An explicit quality allowlist: parameter count or file size is not evidence.
    matches!(
        model.file,
        QWEN3_4B | GEMMA_4_E4B | "Qwen3-8B-Q4_K_M.gguf" | "munin-qwen3.5-9B.Q4_K_M.gguf" | "Qwen3-14B-Q4_K_M.gguf"
    )
}

/// The catalog model with this file name.
pub fn find(file: &str) -> Option<&'static CatalogModel> {
    MODELS.iter().find(|model| model.file == file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_downloads_from_its_repository() {
        assert_eq!(
            find("Qwen3-8B-Q4_K_M.gguf").unwrap().url(),
            "https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf"
        );
        assert_eq!(find("nonsense.gguf"), None);
    }

    #[test]
    fn the_smallest_model_isnt_good_enough_and_the_rest_are() {
        let good: Vec<bool> = MODELS.iter().map(is_good_enough).collect();
        assert_eq!(good, [false, false, true, true, true, true, true]);
    }
}
