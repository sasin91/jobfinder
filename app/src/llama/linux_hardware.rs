//! Linux GPU inventory, including WSL where NVIDIA is exposed without DRM cards.
//! Zero VRAM means unknown/shared memory, never all of the host's RAM.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::hardware::{Gpu, GpuVendor};

pub(super) fn detect(drm: &Path) -> Option<Gpu> {
    let mut cards = drm_cards(drm);
    if let Some(output) = nvidia_output() {
        cards.extend(parse_nvidia(&output));
    }
    cards.into_iter().max_by_key(|gpu| gpu.vram_bytes)
}

fn drm_cards(drm: &Path) -> Vec<Gpu> {
    let Ok(entries) = std::fs::read_dir(drm) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let number = name.to_str()?.strip_prefix("card")?;
            if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let device = entry.path().join("device");
            let hex = std::fs::read_to_string(device.join("vendor")).ok()?;
            let vendor = GpuVendor::from_pci_id(u32::from_str_radix(hex.trim().trim_start_matches("0x"), 16).ok()?);
            let vram_bytes = std::fs::read_to_string(device.join("mem_info_vram_total"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            let name = std::fs::read_to_string(device.join("product_name"))
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| format!("{vendor:?} graphics ({})", name.to_string_lossy()));
            Some(Gpu {
                name,
                vendor,
                vram_bytes,
            })
        })
        .collect()
}

fn nvidia_output() -> Option<String> {
    // WSL's utility is not always on PATH. Do not invoke a shell or wait indefinitely.
    for program in ["nvidia-smi", "/usr/lib/wsl/lib/nvidia-smi"] {
        let Ok(mut child) = Command::new(program)
            .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
        else {
            continue;
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    use std::io::Read;
                    let mut result = String::new();
                    child.stdout.take()?.take(16_384).read_to_string(&mut result).ok()?;
                    return Some(result);
                }
                Ok(Some(_)) => break,
                _ if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
    None
}

fn parse_nvidia(output: &str) -> Vec<Gpu> {
    output
        .lines()
        .filter_map(|line| {
            let (name, mib) = line.rsplit_once(',')?;
            Some(Gpu {
                name: name.trim().to_string(),
                vendor: GpuVendor::Nvidia,
                vram_bytes: mib.trim().parse::<u64>().ok()?.checked_mul(1024 * 1024)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wsl_and_multiple_cards_are_read_without_treating_unknown_as_memory() {
        let cards = parse_nvidia("RTX 4070, 12282\nRTX 3060, 12288\nunknown, [N/A]\n");
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[1].vram_bytes, 12 * 1024 * 1024 * 1024);
        assert!(parse_nvidia("broken").is_empty());
    }
    #[test]
    fn drm_ignores_connectors_and_preserves_unknown_memory() {
        let folder = std::env::temp_dir().join(format!("jobfinder-drm-{}", std::process::id()));
        let device = folder.join("card0/device");
        std::fs::create_dir_all(&device).unwrap();
        std::fs::create_dir_all(folder.join("card0-HDMI-A-1")).unwrap();
        std::fs::write(device.join("vendor"), "0x8086\n").unwrap();
        let cards = drm_cards(&folder);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].vendor, GpuVendor::Intel);
        assert_eq!(cards[0].vram_bytes, 0);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
