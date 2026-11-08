//! Chrome launched without what only an automated Chrome declares: the headless
//! User-Agent, `navigator.webdriver` and the 800x600 screen.
//! `AGENT_BROWSER_COHERENT_CHROME` turns it on. Fork-only, see FORK.md.

use std::path::Path;

use super::chrome::LaunchOptions;

pub(super) const WINDOW_SIZE: (u32, u32) = (1920, 1080);

/// Arguments that go before the user's, so an explicit one still wins.
pub(super) fn launch_args(options: &LaunchOptions, user_args: &[String]) -> Vec<String> {
    if !options.coherent_fingerprint {
        return Vec::new();
    }
    let mut args = vec!["--disable-blink-features=AutomationControlled".to_string()];
    if options.effectively_headless() {
        // Headless reports an 800x600 screen under any window without this.
        let (width, height) = user_args
            .iter()
            .rev()
            .find_map(|arg| window_size(arg))
            .or(options.viewport_size)
            .unwrap_or(WINDOW_SIZE);
        args.push(format!("--screen-info={{{width}x{height}}}"));
    }
    args
}

fn window_size(arg: &str) -> Option<(u32, u32)> {
    let (width, height) = arg.strip_prefix("--window-size=")?.split_once(',')?;
    Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
}

/// A launch flag because a per-target CDP override misses a popup's first
/// request and service workers, both measured against real Chromium. Headed
/// Chrome already sends its real UA.
/// shortcut: Linux only, the pods' platform: elsewhere the UA token would
/// contradict `navigator.platform`. Add the other tokens before a desktop needs it.
pub(super) fn add_user_agent(options: &mut LaunchOptions, chrome_path: &Path) {
    if !cfg!(target_os = "linux")
        || !options.coherent_fingerprint
        || !options.effectively_headless()
    {
        return;
    }
    if let Some(user_agent) = user_agent(chrome_path) {
        options.args.insert(0, format!("--user-agent={user_agent}"));
    }
}

fn user_agent(chrome_path: &Path) -> Option<String> {
    user_agent_for(&crate::doctor::chrome::query_chrome_version(chrome_path)?)
}

/// The reduced UA of a headed Chrome, which the headless one differs from only
/// by its product name. No version means no flag: the headless UA beats a wrong one.
fn user_agent_for(version_output: &str) -> Option<String> {
    let major = version_output.split_whitespace().find_map(|word| {
        let (major, _) = word.split_once('.')?;
        major.parse::<u32>().ok()
    })?;
    Some(format!(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36"
    ))
}

#[cfg(test)]
mod tests {
    use super::super::chrome::build_chrome_args;
    use super::*;

    fn coherent(args: &[&str]) -> LaunchOptions {
        LaunchOptions {
            coherent_fingerprint: true,
            args: args.iter().map(|arg| arg.to_string()).collect(),
            ..Default::default()
        }
    }

    fn chrome_args(options: &LaunchOptions) -> Vec<String> {
        build_chrome_args(options).unwrap().args
    }

    #[test]
    fn user_agent_reads_the_major_version() {
        for output in [
            "Chromium 156.0.8078.79 Alpine Linux",
            "Google Chrome for Testing 156.0.8078.79",
        ] {
            let user_agent = user_agent_for(output).unwrap();
            assert!(user_agent.contains(" Chrome/156.0.0.0 "), "{user_agent}");
            assert!(!user_agent.contains("Headless"), "{user_agent}");
        }
    }

    #[test]
    fn user_agent_needs_a_version() {
        for output in ["", "Chromium", "Chromium v1.x"] {
            assert_eq!(user_agent_for(output), None, "{output}");
        }
    }

    #[test]
    fn off_by_default() {
        let args = chrome_args(&LaunchOptions::default());
        assert!(!args.iter().any(|a| a.contains("AutomationControlled")));
        assert!(!args.iter().any(|a| a.starts_with("--screen-info")));
        assert!(args.iter().any(|a| a == "--window-size=1280,720"));
    }

    #[test]
    fn default_window_and_screen() {
        let args = chrome_args(&coherent(&[]));
        assert!(args
            .iter()
            .any(|a| a == "--disable-blink-features=AutomationControlled"));
        assert!(args.iter().any(|a| a == "--window-size=1920,1080"));
        assert!(args.iter().any(|a| a == "--screen-info={1920x1080}"));
    }

    #[test]
    fn screen_follows_the_last_explicit_window() {
        let args = chrome_args(&coherent(&[
            "--window-size=800,600",
            "--window-size=2560,1440",
        ]));
        assert!(!args.iter().any(|a| a == "--window-size=1920,1080"));
        assert!(args.iter().any(|a| a == "--screen-info={2560x1440}"));
    }

    #[test]
    fn maximized_window_gets_the_default_screen() {
        let args = chrome_args(&coherent(&["--start-maximized"]));
        assert!(args.iter().any(|a| a == "--screen-info={1920x1080}"));
    }

    #[test]
    fn explicit_screen_wins() {
        let args = chrome_args(&coherent(&["--screen-info={1024x768}"]));
        let position = |flag: &str| args.iter().position(|a| a == flag).unwrap();
        assert!(position("--screen-info={1920x1080}") < position("--screen-info={1024x768}"));
    }

    #[test]
    fn headed_keeps_its_real_screen() {
        let options = LaunchOptions {
            headless: false,
            ..coherent(&[])
        };
        let args = launch_args(&options, &[]);
        assert!(!args.iter().any(|a| a.starts_with("--screen-info")));
    }

    #[cfg(unix)]
    fn fake_chrome(script: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("coherent-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_chrome_that_hangs_on_version_gives_no_user_agent() {
        let chrome = fake_chrome("exec sleep 60");
        let started = std::time::Instant::now();
        assert_eq!(user_agent(&chrome), None);
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        let _ = std::fs::remove_file(chrome);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn user_agent_goes_first_only_when_coherent_and_headless() {
        let chrome = fake_chrome("echo Chromium 156.0.8078.79");
        let mut options = coherent(&["--user-agent=mine"]);
        add_user_agent(&mut options, &chrome);
        assert!(options.args[0].starts_with("--user-agent=Mozilla"));
        assert_eq!(options.args[1], "--user-agent=mine");

        for mut other in [
            LaunchOptions::default(),
            LaunchOptions {
                headless: false,
                ..coherent(&[])
            },
        ] {
            let before = other.args.clone();
            add_user_agent(&mut other, &chrome);
            assert_eq!(other.args, before);
        }
        let _ = std::fs::remove_file(chrome);
    }
}
