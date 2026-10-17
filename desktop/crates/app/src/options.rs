use adult_hub_player::parse_header;
use std::path::PathBuf;

#[derive(Default, Debug, Clone)]
pub struct Options {
    pub url: Option<String>,
    pub headers: Vec<String>,
}

impl Options {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
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

pub fn initial_media(options: &Options) -> Option<String> {
    if let Some(url) = &options.url {
        return Some(url.clone());
    }
    media_beside_executable("sample.mp4").or_else(|| {
        let local = PathBuf::from("sample.mp4");
        local.exists().then(|| local.to_string_lossy().into_owned())
    })
}

pub fn media_beside_executable(name: &str) -> Option<String> {
    let executable = std::env::current_exe().ok()?;
    let candidate = executable.parent()?.join(name);
    candidate
        .is_file()
        .then(|| candidate.to_string_lossy().into_owned())
}
