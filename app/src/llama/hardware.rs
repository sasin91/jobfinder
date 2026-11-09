//! What this computer has to run a model on: its memory, and its biggest graphics card with that
//! card's own memory (VRAM). Ported from penny's hardware.Inventory, reading VRAM through DXGI:
//! Win32_VideoController.AdapterRAM, which penny reads, is 32 bits and so says 4 GB on any bigger
//! card.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Bytes in a gibibyte, which sizes are shown in.
pub const GIB: u64 = 1024 * 1024 * 1024;

/// Who made a graphics card, which decides the compute libraries llama.cpp needs for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

impl GpuVendor {
    /// The vendor of a PCI vendor id.
    pub fn from_pci_id(vendor_id: u32) -> Self {
        /// NVIDIA's PCI vendor id.
        const NVIDIA: u32 = 0x10DE;
        /// AMD's (ATI's) PCI vendor id.
        const AMD: u32 = 0x1002;
        /// Intel's PCI vendor id.
        const INTEL: u32 = 0x8086;
        match vendor_id {
            NVIDIA => GpuVendor::Nvidia,
            AMD => GpuVendor::Amd,
            INTEL => GpuVendor::Intel,
            _ => GpuVendor::Other,
        }
    }
}

/// A graphics card and its own memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    pub vendor: GpuVendor,
    pub vram_bytes: u64,
}

/// The computer's memory and its biggest graphics card, if it has one with memory of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    pub memory_bytes: u64,
    pub gpu: Option<Gpu>,
}

impl Inventory {
    /// This computer's.
    pub fn detect() -> Self {
        Inventory {
            memory_bytes: platform::memory_bytes(),
            gpu: platform::biggest_gpu(),
        }
    }

    /// The card's memory, 0 without a card.
    pub fn vram_bytes(&self) -> u64 {
        self.gpu.as_ref().map_or(0, |gpu| gpu.vram_bytes)
    }
}

impl fmt::Display for Inventory {
    /// "32 GB memory, NVIDIA GeForce RTX 4070 with 12 GB".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} memory", gigabytes(self.memory_bytes))?;
        match &self.gpu {
            Some(gpu) if gpu.vram_bytes == 0 => write!(f, ", {} (dedicated memory unknown)", gpu.name),
            Some(gpu) => write!(f, ", {} with {}", gpu.name, gigabytes(gpu.vram_bytes)),
            None => f.write_str(", no graphics card with memory of its own"),
        }
    }
}

/// bytes as "4.7 GB" (gibibytes, as Windows shows them), to one decimal.
pub fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GIB as f64)
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    use super::{Gpu, GpuVendor};

    pub fn memory_bytes() -> u64 {
        let mut status = MEMORYSTATUSEX {
            dwLength: size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // SAFETY: status is a MEMORYSTATUSEX with its dwLength set, as the call requires.
        match unsafe { GlobalMemoryStatusEx(&mut status) } {
            Ok(()) => status.ullTotalPhys,
            Err(e) => {
                crate::log_line!("llama: this computer's memory couldn't be read: {e}");
                0
            }
        }
    }

    /// The hardware card with the most memory of its own, skipping Microsoft's software renderer.
    pub fn biggest_gpu() -> Option<Gpu> {
        // SAFETY: CreateDXGIFactory1 takes no pointers of ours; the factory is released on drop.
        let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
            Ok(factory) => factory,
            Err(e) => {
                crate::log_line!("llama: the graphics cards couldn't be listed: {e}");
                return None;
            }
        };
        let mut biggest: Option<Gpu> = None;
        // SAFETY: EnumAdapters1 is called with increasing indexes until it says there are no more.
        for adapter in (0..).map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok()) {
            // SAFETY: the adapter came from the factory above.
            let Ok(description) = (unsafe { adapter.GetDesc1() }) else {
                continue;
            };
            let is_software = description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
            if is_software || description.DedicatedVideoMemory == 0 {
                continue;
            }
            let gpu = Gpu {
                name: name_of(&description.Description),
                vendor: GpuVendor::from_pci_id(description.VendorId),
                vram_bytes: description.DedicatedVideoMemory as u64,
            };
            if biggest.as_ref().is_none_or(|found| gpu.vram_bytes > found.vram_bytes) {
                biggest = Some(gpu);
            }
        }
        biggest
    }

    /// DXGI's zero-padded UTF-16 name.
    fn name_of(wide: &[u16]) -> String {
        let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..end]).trim().to_string()
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Gpu;

    /// The kibibytes /proc/meminfo counts in.
    const KIB: u64 = 1024;

    /// MemTotal from /proc/meminfo, 0 where there is none.
    pub fn memory_bytes() -> u64 {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|meminfo| {
                meminfo
                    .lines()
                    .find_map(|line| line.strip_prefix("MemTotal:"))
                    .and_then(|kib| kib.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            })
            .map_or(0, |kib| kib * KIB)
    }

    /// DRM covers native Linux; nvidia-smi also covers WSL's GPU exposure.
    pub fn biggest_gpu() -> Option<Gpu> {
        crate::llama::linux_hardware::detect(std::path::Path::new("/sys/class/drm"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inventory_says_what_it_found() {
        let inventory = Inventory {
            memory_bytes: 32 * GIB,
            gpu: Some(Gpu {
                name: "NVIDIA GeForce RTX 4070".to_string(),
                vendor: GpuVendor::Nvidia,
                vram_bytes: 12 * GIB,
            }),
        };
        assert_eq!(
            inventory.to_string(),
            "32.0 GB memory, NVIDIA GeForce RTX 4070 with 12.0 GB"
        );
        let no_card = Inventory {
            memory_bytes: 16 * GIB,
            gpu: None,
        };
        assert_eq!(
            no_card.to_string(),
            "16.0 GB memory, no graphics card with memory of its own"
        );
    }

    #[test]
    fn vendors_are_read_from_pci_ids() {
        assert_eq!(GpuVendor::from_pci_id(0x10DE), GpuVendor::Nvidia);
        assert_eq!(GpuVendor::from_pci_id(0x1002), GpuVendor::Amd);
        assert_eq!(GpuVendor::from_pci_id(0x8086), GpuVendor::Intel);
        assert_eq!(GpuVendor::from_pci_id(0x1414), GpuVendor::Other);
    }
}
