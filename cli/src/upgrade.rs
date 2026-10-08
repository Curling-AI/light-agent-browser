use crate::color;
use std::path::Path;
use std::process::{exit, Command, Stdio};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// npm package this fork is published as (the CLI command stays `agent-browser`).
const PACKAGE_NAME: &str = "light-agent-browser";
const NPM_REGISTRY_URL: &str = "https://registry.npmjs.org/light-agent-browser/latest";
const CARGO_GIT_URL: &str = "https://github.com/Curling-AI/light-agent-browser";

enum InstallMethod {
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Homebrew,
    Cargo,
    Unknown,
}

async fn fetch_latest_version() -> Result<String, String> {
    let resp =
        crate::tls::apply_to_reqwest(reqwest::Client::builder(), &crate::tls::process_options())?
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?
            .get(NPM_REGISTRY_URL)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch version info: {}", e))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse version info: {}", e))?;

    body.get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "No version field in registry response".to_string())
}

/// Parse the `.install-method` marker written by postinstall.js.
fn read_install_method_marker(exe_dir: &Path) -> Option<InstallMethod> {
    let contents = std::fs::read_to_string(exe_dir.join(".install-method")).ok()?;
    match contents.trim() {
        "npm" => Some(InstallMethod::Npm),
        "pnpm" => Some(InstallMethod::Pnpm),
        "yarn" => Some(InstallMethod::Yarn),
        "bun" => Some(InstallMethod::Bun),
        _ => None,
    }
}

fn detect_install_method() -> InstallMethod {
    if let Ok(exe) = std::env::current_exe() {
        // Resolve symlinks to find the real binary location
        let real_path = exe.canonicalize().unwrap_or(exe);

        // Preferred: read the marker file written at install time
        if let Some(dir) = real_path.parent() {
            if let Some(method) = read_install_method_marker(dir) {
                return method;
            }
        }

        // Fallback: infer from executable path
        let path_str = real_path.to_string_lossy();

        if path_str.contains("/.cargo/bin/") || path_str.contains("\\.cargo\\bin\\") {
            return InstallMethod::Cargo;
        }

        if path_str.contains("/Cellar/agent-browser/")
            || path_str.contains("/homebrew/")
            || path_str.contains("/linuxbrew/")
        {
            return InstallMethod::Homebrew;
        }

        if path_str.contains("/pnpm/") || path_str.contains("/pnpm-global/") {
            return InstallMethod::Pnpm;
        }

        if path_str.contains("/.yarn/") || path_str.contains("/yarn/global/") {
            return InstallMethod::Yarn;
        }

        if path_str.contains("/.bun/") {
            return InstallMethod::Bun;
        }

        if path_str.contains("node_modules/light-agent-browser")
            || path_str.contains("node_modules\\light-agent-browser")
        {
            return InstallMethod::Npm;
        }
    }

    // Last resort: probe package managers via subprocess

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if command_succeeds("brew", &["list", "agent-browser"]) {
            return InstallMethod::Homebrew;
        }
    }

    if command_output_contains(
        "pnpm",
        &["list", "-g", PACKAGE_NAME, "--depth=0"],
        PACKAGE_NAME,
    ) {
        return InstallMethod::Pnpm;
    }

    if command_output_contains("yarn", &["global", "list", "--depth=0"], PACKAGE_NAME) {
        return InstallMethod::Yarn;
    }

    if command_output_contains("bun", &["pm", "ls", "-g"], PACKAGE_NAME) {
        return InstallMethod::Bun;
    }

    if command_succeeds("npm", &["list", "-g", PACKAGE_NAME, "--depth=0"]) {
        return InstallMethod::Npm;
    }

    InstallMethod::Unknown
}

fn command_succeeds(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn command_output_contains(cmd: &str, args: &[&str], needle: &str) -> bool {
    Command::new(cmd)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains(needle))
        .unwrap_or(false)
}

fn upgrade_command(method: &InstallMethod) -> Option<(&'static str, Vec<String>)> {
    let latest = format!("{}@latest", PACKAGE_NAME);
    let (cmd, args): (&str, Vec<String>) = match method {
        InstallMethod::Npm => ("npm", vec!["install".into(), "-g".into(), latest]),
        InstallMethod::Pnpm => ("pnpm", vec!["add".into(), "-g".into(), latest]),
        // NOTE: `yarn global` is Yarn Classic (v1) only; Yarn Berry (v2+) removed it.
        // Users on Yarn v2+ won't reach this path — detection falls through to Unknown.
        InstallMethod::Yarn => ("yarn", vec!["global".into(), "add".into(), latest]),
        InstallMethod::Bun => ("bun", vec!["install".into(), "-g".into(), latest]),
        InstallMethod::Cargo => (
            "cargo",
            vec![
                "install".into(),
                "--git".into(),
                CARGO_GIT_URL.into(),
                "agent-browser".into(),
                "--force".into(),
            ],
        ),
        // There is no Homebrew formula for this fork; a Homebrew install is
        // upstream agent-browser and upgrading it would not install the fork.
        InstallMethod::Homebrew | InstallMethod::Unknown => return None,
    };
    Some((cmd, args))
}

fn run_upgrade_command(method: &InstallMethod) -> bool {
    let Some((cmd, args)) = upgrade_command(method) else {
        return false;
    };
    println!("Running: {} {}", cmd, args.join(" "));
    Command::new(cmd)
        .args(&args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn print_manual_upgrade_help() {
    eprintln!("  To update manually, run one of:");
    eprintln!("    npm install -g {}@latest       # npm", PACKAGE_NAME);
    eprintln!("    pnpm add -g {}@latest          # pnpm", PACKAGE_NAME);
    eprintln!("    yarn global add {}@latest      # yarn", PACKAGE_NAME);
    eprintln!("    bun install -g {}@latest       # bun", PACKAGE_NAME);
    eprintln!(
        "    cargo install --git {} agent-browser --force   # Cargo",
        CARGO_GIT_URL
    );
}

pub fn run_upgrade() {
    let current = CURRENT_VERSION;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!(
                "{} Failed to create runtime: {}",
                color::error_indicator(),
                e
            );
            exit(1);
        });

    let latest = match rt.block_on(fetch_latest_version()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "{} Could not check latest version: {}",
                color::warning_indicator(),
                e
            );
            String::new()
        }
    };

    if !latest.is_empty() && current == latest.as_str() {
        println!(
            "{} {} is already at the latest version (v{})",
            color::success_indicator(),
            PACKAGE_NAME,
            current
        );
        return;
    }

    let method = detect_install_method();

    let method_name = match &method {
        InstallMethod::Npm => "npm",
        InstallMethod::Pnpm => "pnpm",
        InstallMethod::Yarn => "yarn",
        InstallMethod::Bun => "bun",
        InstallMethod::Homebrew => "Homebrew",
        InstallMethod::Cargo => "Cargo",
        InstallMethod::Unknown => "",
    };

    if matches!(method, InstallMethod::Unknown) {
        eprintln!(
            "{} Could not detect installation method.",
            color::error_indicator()
        );
        print_manual_upgrade_help();
        exit(1);
    }
    if matches!(method, InstallMethod::Homebrew) {
        eprintln!(
            "{} This binary was installed with Homebrew, which ships upstream agent-browser. Install {} instead:",
            color::error_indicator(),
            PACKAGE_NAME
        );
        print_manual_upgrade_help();
        exit(1);
    }

    println!("Detected installation via {}.", method_name);

    if !latest.is_empty() {
        println!(
            "{}",
            color::cyan(&format!(
                "Upgrading {}... v{} → v{}",
                PACKAGE_NAME, current, latest
            ))
        );
    } else {
        println!(
            "{}",
            color::cyan(&format!("Upgrading {} (v{})...", PACKAGE_NAME, current))
        );
    }

    let success = run_upgrade_command(&method);

    if success {
        if !latest.is_empty() {
            println!(
                "{} Done! v{} → v{}",
                color::success_indicator(),
                current,
                latest
            );
        } else {
            println!("{} Done!", color::success_indicator());
        }
    } else {
        eprintln!("{} Upgrade failed.", color::error_indicator());
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_commands_target_the_fork_package() {
        let (cmd, args) = upgrade_command(&InstallMethod::Npm).unwrap();
        assert_eq!(cmd, "npm");
        assert_eq!(args, ["install", "-g", "light-agent-browser@latest"]);
        let (_, args) = upgrade_command(&InstallMethod::Cargo).unwrap();
        assert!(args.contains(&CARGO_GIT_URL.to_string()));
        assert!(upgrade_command(&InstallMethod::Homebrew).is_none());
        assert!(NPM_REGISTRY_URL.contains(PACKAGE_NAME));
    }
}
