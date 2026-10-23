# Fork notes: light-agent-browser

This fork of [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) makes [Lightpanda](https://github.com/lightpanda-io/browser) the default engine while staying backward compatible and easy to rebase on upstream.

## What differs from upstream

- **Default engine.** Without an explicit `--engine`, local launches use Lightpanda and fall back to Chrome (with a warning) on Windows, when Lightpanda is not installed, with a non-Lightpanda `--executable-path`, or with Chrome-only options. Explicit engines, `--cdp`, `--auto-connect`, and providers behave exactly as upstream.
- **Install.** `agent-browser install` downloads Lightpanda. `--with-chrome` and `--with-deps` add Chrome; `--engine chrome install` is the upstream behavior.
- **Screenshots and PDFs.** Lightpanda pages are rendered by Chrome from a serialized DOM, either in-process or through `agent-browser renderer serve` (`--screenshot-renderer`).
- **Local files.** Lightpanda only speaks http(s), so `file://` pages are served to it from disk through a reserved host (`http://file.agent-browser.invalid/...`), limited to the folders the session opened unless `--allow-file-access` is set. A local `.pdf` still relaunches the session with Chrome.

## Where the code lives

New behavior is isolated in fork-only files so upstream merges rarely conflict:

- `cli/src/native/engine.rs`: default engine resolution and fallback rules
- `cli/src/native/render/`: DOM serialization, local Chrome renderer, remote client, and the `renderer serve` HTTP service; `callers.rs` verifies per-caller HMAC tokens and admits renders under per-caller limits; `metrics.rs` serves `/metrics`; `live.rs` decides and runs live captures in a short-lived local Chrome; `guard.rs` holds the untrusted-input policy the service enforces (scripts off, public http(s) subresources only)
- `cli/src/native/file_url.rs`: `file://` pages on Lightpanda (URL mapping, allowed folders, serving files through `Fetch`)
- `cli/src/lightpanda_install.rs`: Lightpanda download for `install`
- `docker/renderer/`: renderer container image and Kubernetes example
- `FORK.md`: this file

Upstream files carry small, self-contained hooks:

- `cli/src/native/actions.rs`: engine resolution in the two local launch paths, `engine_is_default`/`force_chrome` state and the local `.pdf` relaunch in `handle_navigate`, `file_urls` state (recorded `--allow-file-access` at both launch paths, cleared on `close`, reserved URL in `handle_navigate` and `handle_tab_new`, reserved-host requests served first in the `Fetch` handler, its pattern in `build_fetch_patterns`, `session_setup_pending`, and `apply_session_setup`, and `file://` shown in every response through `execute_command`), `rendererReason` in screenshot and pdf responses, `pending_launch_warning` and `screenshot_renderer` state, screenshot and diff screenshot routed through `render::capture_screenshot`, `pdf` routed through `render::capture_pdf`, renderer shutdown on `close`, and `handle_launch_with_deadline` boxing the launch future (with the fork's hooks it overflows a 2 MB stack in debug builds)
- `cli/src/native/mod.rs`: module registration
- `cli/src/native/browser.rs`: `validate_lightpanda_options` accepts `--allow-file-access`
- `cli/src/native/cdp/client.rs`: `subscribe_session_with_buffer`, so the renderer's request gate gets a buffer that does not drop `Fetch.requestPaused` (`subscribe_session` keeps the upstream 16-event, drop-newest behavior). Upstream's `subscribe_methods` (#2110) does not replace it: its lossless taps only see broadcast events, and a private session's events never reach the broadcast
- `cli/src/native/cdp/lightpanda.rs`: `find_lightpanda` also checks the install directory
- `cli/src/native/cdp/chrome.rs`: the "Chrome cache directory" warning ignores Lightpanda entries
- `cli/src/install.rs`: `run_install` installs Lightpanda first; Chrome install moved to `install_chrome`
- `cli/src/native/actions.rs`: `handle_tab_new` and `click --new-tab` refuse a second page on Lightpanda up front (`engine::second_page_error`), since Lightpanda serves one page per CDP connection
- `cli/src/connection.rs`: `send_command` waits for the daemon to exit after a successful `close`, so the next command starts a fresh daemon (upstream #2134; drop this hook once upstream fixes it)
- `cli/src/main.rs`: `renderer` command, install flags, launch warning printed on stderr
- `cli/src/output.rs`: response warnings printed for every action, help text
- `cli/src/flags.rs`, `cli/src/commands.rs`, `cli/src/mcp.rs`, `agent-browser.schema.json`: `--screenshot-renderer` plumbing, per-command `rendererToken` forwarding, and MCP parity
- `cli/src/native/actions.rs` also strips `rendererToken` from dashboard command broadcasts
- `cli/tests/tls_cli.rs`: the Chrome manifest trust test runs `--engine chrome install`
- `scripts/postinstall.js`: install reminder mentions Lightpanda
- Documentation: `README.md`, `skill-data/core/`, `docs/content/docs/`

The upstream e2e suite assumes Chrome without naming an engine, so test builds resolve an unset engine to Chrome (see `resolve_launch_engine`). Fork behavior is covered by unit tests in `engine.rs`, `file_url.rs`, and `render/` and by `e2e_lightpanda_screenshot_renderers` and `e2e_lightpanda_opens_file_urls`.

## Syncing with upstream

```bash
git remote add upstream https://github.com/vercel-labs/agent-browser.git   # once
git fetch upstream
git merge upstream/main          # or: git rebase upstream/main
cd cli && cargo test && cargo clippy --all-targets
LIGHTPANDA_BIN=~/.agent-browser/browsers/lightpanda-1.0.0/lightpanda \
  cargo test e2e_lightpanda -- --ignored --test-threads=1
```

When resolving conflicts, keep upstream's version of a hunk and re-apply the fork hook listed above. Pay attention to upstream changes that:

- add a new launch path in `actions.rs` (it needs the same `engine::resolve_launch_engine` call)
- add a Chrome-only launch option (add it to `engine::chrome_only_option`)
- add a new screenshot or PDF entry point (route it through `render::capture_screenshot` or `render::capture_pdf`)
- change `install.rs` or the Lightpanda launcher

Upstream release commits conflict in the version files (`package.json`, `cli/Cargo.toml`, `cli/Cargo.lock`, `packages/*`) and in both changelogs. Never resolve them by taking upstream's whole file (`git checkout --theirs`): `package.json` and `cli/Cargo.toml` carry the fork's identity (npm name `light-agent-browser`, repository, homepage, description, keywords), and a release with upstream's name would try to publish `agent-browser`. Take only the version lines, then check that `git diff origin/main -- package.json cli/Cargo.toml` changes nothing but the version. In `CHANGELOG.md`, keep exactly one `release:start`/`release:end` pair, around the fork's entry for the release.

Bump `LIGHTPANDA_VERSION` in `cli/src/lightpanda_install.rs` when adopting a new Lightpanda release.
