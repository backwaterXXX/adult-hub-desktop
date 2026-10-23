use adult_hub_player::parse_header;
use std::path::PathBuf;

#[derive(Default, Debug, Clone)]
pub struct Options {
    pub url: Option<String>,
    pub headers: Vec<String>,
    pub demo_shuffle: bool,
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
                "--header" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--header requires a value".to_owned())?;
                    options
                        .headers
                        .push(parse_header(&value).map_err(|error| error.to_string())?);
                }
                "--help" | "-h" => {
                    println!("Adult Hub Desktop {}", env!("CARGO_PKG_VERSION"));
                    println!("Usage: adult-hub-desktop [OPTIONS] [MEDIA_URL]");
                    println!();
                    println!("Options:");
                    println!("  -h, --help            Show this help message and exit");
                    println!("  -v, --version         Show application version");
                    println!("      --header <HEADER> HTTP header to pass to libmpv");
                    std::process::exit(0);
                }
                "--version" | "-v" => {
                    println!("adult-hub-desktop {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                "--" => continue,
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
