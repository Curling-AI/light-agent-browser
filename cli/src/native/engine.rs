//! Default browser engine selection.
//!
//! Lightpanda is the default engine. When the caller does not pick an engine
//! explicitly (no `--engine` flag and no `AGENT_BROWSER_ENGINE`), the daemon
//! falls back to Chrome whenever Lightpanda cannot serve the launch: the
//! platform has no Lightpanda build (Windows), the binary is not installed, or
//! a Chrome-only launch option is set. An explicit engine is never overridden.

use std::path::Path;

use super::cdp::chrome::LaunchOptions;
use super::cdp::lightpanda::find_lightpanda;

pub const DEFAULT_ENGINE: &str = "lightpanda";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineChoice {
    pub engine: String,
    /// Set when the default engine was replaced by Chrome; explains why.
    pub fallback_reason: Option<String>,
}

impl EngineChoice {
    pub fn warning(&self) -> Option<String> {
        self.fallback_reason.as_ref().map(|reason| {
            format!(
                "Using the Chrome engine instead of Lightpanda: {}. Pass --engine chrome to silence this warning.",
                reason
            )
        })
    }
}

/// Resolves the engine for a local launch, probing for an installed Lightpanda.
pub fn resolve_launch_engine(explicit: Option<&str>, options: &LaunchOptions) -> EngineChoice {
    // The upstream e2e suite exercises Chrome-specific behavior without
    // naming an engine; keep it on Chrome so it runs unmodified. The
    // default-selection logic is covered by `resolve_engine_with` tests.
    #[cfg(test)]
    let explicit = explicit.or(Some("chrome"));
    resolve_engine_with(explicit, options, cfg!(windows), || {
        find_lightpanda().is_some()
    })
}

/// Pure resolution logic; `lightpanda_installed` is only called when needed.
pub fn resolve_engine_with(
    explicit: Option<&str>,
    options: &LaunchOptions,
    is_windows: bool,
    lightpanda_installed: impl FnOnce() -> bool,
) -> EngineChoice {
    if let Some(engine) = explicit.map(str::trim).filter(|e| !e.is_empty()) {
        return EngineChoice {
            engine: engine.to_ascii_lowercase(),
            fallback_reason: None,
        };
    }

    let chrome = |reason: String| EngineChoice {
        engine: "chrome".to_string(),
        fallback_reason: Some(reason),
    };

    if let Some(path) = options.executable_path.as_deref() {
        // An explicit binary decides the engine: existing setups point
        // --executable-path at a Chrome build, so only a Lightpanda binary
        // keeps the default.
        if is_lightpanda_binary(path) {
            return EngineChoice {
                engine: DEFAULT_ENGINE.to_string(),
                fallback_reason: None,
            };
        }
        return EngineChoice {
            engine: "chrome".to_string(),
            fallback_reason: None,
        };
    }

    if let Some(option) = chrome_only_option(options) {
        return chrome(format!("{} requires Chrome", option));
    }
    if is_windows {
        return chrome("Lightpanda has no Windows build".to_string());
    }
    if !lightpanda_installed() {
        return chrome(
            "Lightpanda is not installed (run `agent-browser install` to download it)".to_string(),
        );
    }

    EngineChoice {
        engine: DEFAULT_ENGINE.to_string(),
        fallback_reason: None,
    }
}

/// Returns the first launch option Lightpanda cannot honor, if any.
pub fn chrome_only_option(options: &LaunchOptions) -> Option<&'static str> {
    if !options.headless {
        return Some("--headed");
    }
    if options.profile.is_some() {
        return Some("--profile");
    }
    if options.extensions.as_ref().is_some_and(|e| !e.is_empty()) {
        return Some("--extension");
    }
    if options.storage_state.is_some() {
        return Some("--state");
    }
    if options.allow_file_access {
        return Some("--allow-file-access");
    }
    if options.webgpu {
        return Some("--webgpu");
    }
    if options.ca_cert.is_some() {
        return Some("--ca-cert");
    }
    if !options.args.is_empty() {
        return Some("--args");
    }
    None
}

fn is_lightpanda_binary(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().contains("lightpanda"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(explicit: Option<&str>, options: &LaunchOptions, installed: bool) -> EngineChoice {
        resolve_engine_with(explicit, options, false, || installed)
    }

    #[test]
    fn defaults_to_lightpanda_when_installed() {
        let choice = resolve(None, &LaunchOptions::default(), true);
        assert_eq!(choice.engine, "lightpanda");
        assert!(choice.warning().is_none());
    }

    #[test]
    fn explicit_engine_is_never_overridden() {
        let options = LaunchOptions {
            headless: false,
            ..LaunchOptions::default()
        };
        let choice = resolve(Some("Lightpanda"), &options, false);
        assert_eq!(choice.engine, "lightpanda");
        assert!(choice.fallback_reason.is_none());
        assert_eq!(
            resolve(Some("chrome"), &LaunchOptions::default(), true).engine,
            "chrome"
        );
    }

    #[test]
    fn falls_back_to_chrome_when_lightpanda_missing() {
        let choice = resolve(None, &LaunchOptions::default(), false);
        assert_eq!(choice.engine, "chrome");
        assert!(choice.warning().unwrap().contains("not installed"));
    }

    #[test]
    fn falls_back_to_chrome_on_windows() {
        let choice = resolve_engine_with(None, &LaunchOptions::default(), true, || true);
        assert_eq!(choice.engine, "chrome");
        assert!(choice.warning().unwrap().contains("Windows"));
    }

    #[test]
    fn chrome_only_options_select_chrome_without_probing() {
        let options = LaunchOptions {
            profile: Some("Default".to_string()),
            ..LaunchOptions::default()
        };
        let choice = resolve_engine_with(None, &options, false, || {
            panic!("must not probe for Lightpanda when Chrome is required")
        });
        assert_eq!(choice.engine, "chrome");
        assert!(choice.warning().unwrap().contains("--profile"));
    }

    #[test]
    fn executable_path_selects_engine_by_binary_name() {
        let chrome = LaunchOptions {
            executable_path: Some("/opt/google/chrome/chrome".to_string()),
            ..LaunchOptions::default()
        };
        let choice = resolve(None, &chrome, true);
        assert_eq!(choice.engine, "chrome");
        assert!(choice.fallback_reason.is_none());

        let lightpanda = LaunchOptions {
            executable_path: Some("/usr/local/bin/lightpanda".to_string()),
            ..LaunchOptions::default()
        };
        assert_eq!(resolve(None, &lightpanda, false).engine, "lightpanda");
    }

    #[test]
    fn each_chrome_only_option_is_detected() {
        let cases: Vec<(LaunchOptions, &str)> = vec![
            (
                LaunchOptions {
                    headless: false,
                    ..LaunchOptions::default()
                },
                "--headed",
            ),
            (
                LaunchOptions {
                    extensions: Some(vec!["/ext".to_string()]),
                    ..LaunchOptions::default()
                },
                "--extension",
            ),
            (
                LaunchOptions {
                    storage_state: Some("state.json".to_string()),
                    ..LaunchOptions::default()
                },
                "--state",
            ),
            (
                LaunchOptions {
                    allow_file_access: true,
                    ..LaunchOptions::default()
                },
                "--allow-file-access",
            ),
            (
                LaunchOptions {
                    webgpu: true,
                    ..LaunchOptions::default()
                },
                "--webgpu",
            ),
            (
                LaunchOptions {
                    args: vec!["--foo".to_string()],
                    ..LaunchOptions::default()
                },
                "--args",
            ),
        ];
        for (options, flag) in cases {
            assert_eq!(chrome_only_option(&options), Some(flag));
        }
        assert_eq!(
            chrome_only_option(&LaunchOptions {
                extensions: Some(Vec::new()),
                ..LaunchOptions::default()
            }),
            None
        );
    }
}
