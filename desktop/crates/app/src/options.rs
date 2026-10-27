use adult_hub_player::parse_header;
use std::path::PathBuf;

#[derive(Default, Debug, Clone)]
pub struct Options {
    pub url: Option<String>,
    pub headers: Vec<String>,
    pub demo_shuffle: bool,
    pub debug: bool,
    pub renderer: Option<String>,
}

impl Options {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--demo-shuffle" => {
                    options.demo_shuffle = true;
                }
                "--debug" => {
                    options.debug = true;
                }
                "--renderer" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--renderer requires gl or software".to_owned())?;
                    options.renderer = Some(Self::normalize_renderer(&value)?);
                }
                "--header" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--header requires a value".to_owned())?;
                    options
                        .headers
                        .push(parse_header(&value).map_err(|error| error.to_string())?);
                }
                "--help" | "-h" => {
                    println!(
                        "Adult Hub Desktop v{} (build {})",
                        env!("CARGO_PKG_VERSION"),
                        env!("ADULT_HUB_BUILD_ID")
                    );
                    println!("Usage: adult-hub-desktop [OPTIONS] [MEDIA_URL]");
                    println!();
                    println!("Options:");
                    println!("  -h, --help                Show this help message and exit");
                    println!("  -v, --version             Show application version and exit");
                    println!(
                        "      --debug               Write detailed diagnostics to the log file"
                    );
                    println!(
                        "      --renderer <MODE>     Force the UI renderer for this run: gl | software"
                    );
                    println!("      --header <HEADER>     HTTP header to pass to libmpv");
                    println!("      --demo-shuffle        Run the built-in shuffle demo and exit");
                    println!();
                    println!(
                        "Renderer priority: --renderer > AH_RENDERER > saved preference > automatic."
                    );
                    println!(
                        "A one-off --renderer or AH_RENDERER choice never changes the saved preference."
                    );
                    std::process::exit(0);
                }
                "--version" | "-v" => {
                    let pkg_ver = env!("CARGO_PKG_VERSION");
                    let commit = env!("ADULT_HUB_COMMIT_HASH");
                    let dirty = if env!("ADULT_HUB_DIRTY") == "1" {
                        "-dirty"
                    } else {
                        ""
                    };
                    let build_id = env!("ADULT_HUB_BUILD_ID");
                    let build_date = env!("ADULT_HUB_BUILD_DATE");
                    println!(
                        "adult-hub-desktop v{pkg_ver} ({commit}{dirty}) build={build_id} built={build_date}"
                    );
                    std::process::exit(0);
                }
                "--" => continue,
                _ if argument.starts_with("--renderer=") => {
                    let value = argument.trim_start_matches("--renderer=");
                    options.renderer = Some(Self::normalize_renderer(value)?);
                }
                _ if argument.starts_with("--header=") => {
                    let value = argument.trim_start_matches("--header=");
                    options
                        .headers
                        .push(parse_header(value).map_err(|error| error.to_string())?);
                }
                _ if argument.starts_with('-') => {
                    return Err(format!("unknown argument: {argument}"));
                }
                _ => {
                    if options.url.is_some() {
                        return Err("only one media URL can be passed".to_owned());
                    }
                    options.url = Some(argument);
                }
            }
        }
        Ok(options)
    }

    fn normalize_renderer(value: &str) -> Result<String, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gl" | "opengl" => Ok("gl".to_owned()),
            "software" | "sw" => Ok("software".to_owned()),
            other => Err(format!("--renderer must be gl or software, got '{other}'")),
        }
    }
}

pub const PUBLIC_SAMPLE_MP4_URL: &str =
    "https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4";

pub fn resolve_sample_media() -> String {
    resolve_named_sample_media("sample.mp4")
}

pub fn resolve_named_sample_media(filename: &str) -> String {
    // 1. Executable directory
    if let Some(path) = media_beside_executable(filename) {
        return path;
    }
    // 2. Current working directory
    let local = PathBuf::from(filename);
    if local.is_file() {
        return local.to_string_lossy().into_owned();
    }
    let repo_sample = PathBuf::from("desktop").join(filename);
    if repo_sample.is_file() {
        return repo_sample.to_string_lossy().into_owned();
    }
    // 3. Fallback to hardcoded remote public MP4 stream URL
    PUBLIC_SAMPLE_MP4_URL.to_string()
}

pub fn initial_media(options: &Options) -> Option<String> {
    // A bundled sample is an explicit demo action in the UI, not a startup
    // default. Opening the installed app without arguments should land on Home.
    options.url.clone()
}

pub fn media_beside_executable(name: &str) -> Option<String> {
    let executable = std::env::current_exe().ok()?;
    let candidate = executable.parent()?.join(name);
    candidate
        .is_file()
        .then(|| candidate.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::Options;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn debug_flag_and_renderer_combinations() {
        let options = parse(&["--debug"]).unwrap();
        assert!(options.debug);
        assert_eq!(options.renderer, None);

        let options = parse(&["--debug", "--renderer", "software"]).unwrap();
        assert!(options.debug);
        assert_eq!(options.renderer.as_deref(), Some("software"));

        let options = parse(&["--debug", "--renderer=gl"]).unwrap();
        assert!(options.debug);
        assert_eq!(options.renderer.as_deref(), Some("gl"));
    }

    #[test]
    fn renderer_values_are_normalized() {
        assert_eq!(
            parse(&["--renderer", "OpenGL"])
                .unwrap()
                .renderer
                .as_deref(),
            Some("gl")
        );
        assert_eq!(
            parse(&["--renderer=SW"]).unwrap().renderer.as_deref(),
            Some("software")
        );
    }

    #[test]
    fn invalid_renderer_arguments_error() {
        assert!(parse(&["--renderer", "vulkan"]).is_err());
        assert!(parse(&["--renderer=vk"]).is_err());
        assert!(parse(&["--renderer"]).is_err());
    }

    #[test]
    fn debug_does_not_disturb_other_arguments() {
        let options = parse(&["--debug", "--header", "X-Test: 1", "file.mp4"]).unwrap();
        assert!(options.debug);
        assert_eq!(options.url.as_deref(), Some("file.mp4"));
        assert_eq!(options.headers.len(), 1);
    }

    #[test]
    fn version_and_build_id_metadata() {
        let pkg_ver = env!("CARGO_PKG_VERSION");
        assert!(!pkg_ver.is_empty());
        let parts: Vec<&str> = pkg_ver.split('.').collect();
        assert_eq!(parts.len(), 3, "version must be major.minor.patch semver");
        for part in &parts {
            assert!(part.chars().all(|c| c.is_ascii_digit()), "version parts must be numeric");
        }
        let build_id = env!("ADULT_HUB_BUILD_ID");
        let expected_prefix = format!("{pkg_ver}-");
        assert!(build_id.starts_with(&expected_prefix), "build_id was: {build_id}");
        assert!(!build_id.is_empty());
        let build_date = env!("ADULT_HUB_BUILD_DATE");
        assert!(!build_date.is_empty());
    }
}
