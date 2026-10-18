use adult_hub_player::Player;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

#[derive(Debug, Clone)]
pub struct SystemSnapshot {
    pub os_name: String,
    pub os_version: String,
    pub cpu_brand: String,
    pub logical_processors: usize,
    pub total_memory_mb: u64,
    pub available_memory_mb: u64,
    pub vulkan_loader: String,
}

fn vulkan_loader_status() -> String {
    #[cfg(any(windows, target_os = "linux"))]
    {
        let library_name = if cfg!(windows) {
            "vulkan-1.dll"
        } else {
            "libvulkan.so.1"
        };
        // The libmpv build used by this app links the Vulkan loader directly.
        // Probe it here so a missing driver runtime is visible in Settings.
        return if unsafe { libloading::Library::new(library_name) }.is_ok() {
            "Available".to_owned()
        } else {
            "Missing".to_owned()
        };
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    {
        "Not checked on this platform".to_owned()
    }
}

pub fn snapshot() -> SystemSnapshot {
    let mut system =
        System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()));
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());

    let os_name = System::name().unwrap_or_else(|| std::env::consts::OS.to_owned());
    let os_version = System::long_os_version().unwrap_or_else(|| "Unknown".into());
    let cpu_brand = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_owned())
        .filter(|brand| !brand.is_empty())
        .unwrap_or_else(|| "Unknown processor".into());

    SystemSnapshot {
        os_name,
        os_version,
        cpu_brand,
        logical_processors: system.cpus().len(),
        total_memory_mb: system.total_memory() / (1024 * 1024),
        available_memory_mb: system.available_memory() / (1024 * 1024),
        vulkan_loader: vulkan_loader_status(),
    }
}

pub fn update_ui(ui: &crate::MainWindow, player: Option<&Player>) {
    let info = snapshot();
    ui.set_system_os_name(info.os_name.into());
    ui.set_system_os_version(info.os_version.into());
    ui.set_system_cpu_brand(info.cpu_brand.into());
    ui.set_system_logical_processors(info.logical_processors.to_string().into());
    ui.set_system_total_memory(info.total_memory_mb.to_string().into());
    ui.set_system_available_memory(info.available_memory_mb.to_string().into());
    ui.set_system_vulkan_loader(info.vulkan_loader.clone().into());

    let codec_support = player.and_then(Player::common_video_decoder_support);
    if let Some(support) = codec_support {
        crate::diagnostics::GLOBAL_LOG.i(
            "SystemInfo",
            &format!(
                "libmpv common video decoders: H264={}, HEVC={}, VP9={}, AV1={}",
                support[0], support[1], support[2], support[3]
            ),
        );
    } else {
        crate::diagnostics::GLOBAL_LOG.w("SystemInfo", "libmpv decoder-list unavailable");
    }
    let status = |index| match codec_support {
        Some(support) if support[index] => "Available",
        Some(_) => "Not listed",
        None => "Unavailable",
    };
    ui.set_system_codec_h264(status(0).into());
    ui.set_system_codec_hevc(status(1).into());
    ui.set_system_codec_vp9(status(2).into());
    ui.set_system_codec_av1(status(3).into());
}
