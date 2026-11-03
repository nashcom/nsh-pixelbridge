//! Configuration from environment variables (PIXELBRIDGE_*).
//!
//! There is no config file and no CLI option for settings: containers are
//! configured through the environment. Every value has a default and a valid
//! range; an invalid value stops the service at startup with a clear message.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use crate::convert::Limits;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub font_dir: PathBuf,
    pub max_request_bytes: usize,
    pub max_dimension: u32,
    pub max_pixels: u64,
    pub max_concurrent: usize,
    pub timeout_seconds: u64,
    pub public_url: Option<String>,
    /// Serve /openapi.json from this file (re-read on every request) instead
    /// of the copy built into the binary. For testing the API description.
    pub openapi_file: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        Config::from_lookup(|name| std::env::var(name).ok())
    }

    /// Same as `from_env`, but reads values through a closure so tests can
    /// supply their own without touching the real process environment.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let listen_text = get("PIXELBRIDGE_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".to_string());
        let listen = SocketAddr::from_str(&listen_text).map_err(|_| {
            format!("PIXELBRIDGE_LISTEN: '{listen_text}' is not a valid ip:port (example 0.0.0.0:8080)")
        })?;

        let public_url = get("PIXELBRIDGE_PUBLIC_URL")
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());

        let openapi_file = get("PIXELBRIDGE_OPENAPI_FILE")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        Ok(Config {
            listen,
            font_dir: PathBuf::from(get("PIXELBRIDGE_FONT_DIR").unwrap_or_else(|| "/fonts".to_string())),
            max_request_bytes: number(&get, "PIXELBRIDGE_MAX_REQUEST_BYTES", 5 * 1024 * 1024, 1024, 256 * 1024 * 1024)?,
            max_dimension: number(&get, "PIXELBRIDGE_MAX_DIMENSION", 4096, 1, 16384)?,
            max_pixels: number(&get, "PIXELBRIDGE_MAX_PIXELS", 16_000_000, 1, 268_435_456)?,
            max_concurrent: number(&get, "PIXELBRIDGE_MAX_CONCURRENT", 4, 1, 256)?,
            timeout_seconds: number(&get, "PIXELBRIDGE_TIMEOUT_SECONDS", 15, 1, 600)?,
            public_url,
            openapi_file,
        })
    }

    pub fn limits(&self) -> Limits {
        Limits { max_dimension: self.max_dimension, max_pixels: self.max_pixels }
    }
}

/// Reads one numeric setting: default if unset, error if unparsable or outside min..=max.
fn number<T>(get: &impl Fn(&str) -> Option<String>, name: &str, default: T, min: T, max: T) -> Result<T, String>
where
    T: FromStr + PartialOrd + Copy + std::fmt::Display,
{
    let Some(text) = get(name) else {
        return Ok(default);
    };
    let value = text
        .trim()
        .parse::<T>()
        .map_err(|_| format!("{name}: '{text}' is not a valid number"))?;
    if value < min || value > max {
        return Err(format!("{name}: {value} is outside the allowed range {min}..{max}"));
    }
    Ok(value)
}

pub const HELP: &str = "\
nsh-pixelbridge - SVG/PNG/JPEG converter REST service

Usage:
  nsh-pixelbridge                 run the service (configured by environment)
  nsh-pixelbridge -health-check   probe GET /health on the configured port (exit 0 = healthy)
  nsh-pixelbridge -version
  nsh-pixelbridge -help

Environment (default, range):
  PIXELBRIDGE_LISTEN              0.0.0.0:8080   ip:port to listen on (plain HTTP)
  PIXELBRIDGE_FONT_DIR            /fonts         directory with .ttf/.otf fonts for SVG text
  PIXELBRIDGE_MAX_REQUEST_BYTES   5242880        1024..268435456  maximum request body
  PIXELBRIDGE_MAX_DIMENSION       4096           1..16384         maximum output width/height
  PIXELBRIDGE_MAX_PIXELS          16000000       1..268435456     maximum output (and input) pixels
  PIXELBRIDGE_MAX_CONCURRENT      4              1..256           parallel conversions
  PIXELBRIDGE_TIMEOUT_SECONDS     15             1..600           time a request may wait for its result
  PIXELBRIDGE_PUBLIC_URL          (none)         server URL advertised in /openapi.json
  PIXELBRIDGE_OPENAPI_FILE        (none)         serve /openapi.json from this file, re-read on every request (testing)
  RUST_LOG                        info           log level (error, warn, info, debug, trace)
";

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let owned: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_lookup(move |name| owned.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    }

    #[test]
    fn defaults_apply() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.listen.port(), 8080);
        assert_eq!(c.max_request_bytes, 5 * 1024 * 1024);
        assert_eq!(c.max_dimension, 4096);
        assert_eq!(c.max_concurrent, 4);
        assert!(c.public_url.is_none());
        assert!(c.openapi_file.is_none());
    }

    #[test]
    fn openapi_file_is_optional_and_blank_means_off() {
        let c = cfg(&[("PIXELBRIDGE_OPENAPI_FILE", "/openapi.json")]).unwrap();
        assert_eq!(c.openapi_file, Some(PathBuf::from("/openapi.json")));
        assert!(cfg(&[("PIXELBRIDGE_OPENAPI_FILE", "  ")]).unwrap().openapi_file.is_none());
    }

    #[test]
    fn values_are_read_and_url_is_trimmed() {
        let c = cfg(&[
            ("PIXELBRIDGE_LISTEN", "127.0.0.1:9000"),
            ("PIXELBRIDGE_MAX_PIXELS", "1000"),
            ("PIXELBRIDGE_PUBLIC_URL", "https://img.example.com/"),
        ])
        .unwrap();
        assert_eq!(c.listen.port(), 9000);
        assert_eq!(c.max_pixels, 1000);
        assert_eq!(c.public_url.as_deref(), Some("https://img.example.com"));
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(cfg(&[("PIXELBRIDGE_LISTEN", "nonsense")]).is_err());
        assert!(cfg(&[("PIXELBRIDGE_MAX_CONCURRENT", "0")]).is_err());
        assert!(cfg(&[("PIXELBRIDGE_MAX_DIMENSION", "99999")]).is_err());
        assert!(cfg(&[("PIXELBRIDGE_TIMEOUT_SECONDS", "abc")]).is_err());
    }
}
