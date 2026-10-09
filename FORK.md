# Fork notes: light-agent-browser

This fork of [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) makes [Lightpanda](https://github.com/lightpanda-io/browser) the default engine while staying backward compatible and easy to rebase on upstream.

## What differs from upstream

- **Default engine.** Without an explicit `--engine`, local launches use Lightpanda and fall back to Chrome (with a warning) on Windows, when Lightpanda is not installed, with a non-Lightpanda `--executable-path`, or with Chrome-only options. Explicit engines, `--cdp`, `--auto-connect`, and providers behave exactly as upstream.
- **Install.** `agent-browser install` downloads Lightpanda. `--with-chrome` and `--with-deps` add Chrome; `--engine chrome install` is the upstream behavior.
- **Screenshots and PDFs.** Lightpanda pages are rendered by Chrome from a serialized DOM, either in-process or through `agent-browser renderer serve` (`--screenshot-renderer`).

## Where the code lives

New behavior is isolated in fork-only files so upstream merges rarely conflict:

- `cli/src/native/engine.rs`: default engine resolution and fallback rules
- `cli/src/native/render/`: DOM serialization, local Chrome renderer, remote client, and the `renderer serve` HTTP service; `guard.rs` holds the untrusted-input policy the service enforces (scripts off, public http(s) subresources only)
- `cli/src/lightpanda_install.rs`: Lightpanda download for `install`
- `docker/renderer/`: renderer container image and Kubernetes example
- `FORK.md`: this file

Upstream files carry small, self-contained hooks:

- `cli/src/native/actions.rs`: engine resolution in the two local launch paths, `pending_launch_warning` and `screenshot_renderer` state, screenshot and diff screenshot routed through `render::capture_screenshot`, `pdf` routed through `render::capture_pdf`, renderer shutdown on `close`
- `cli/src/native/mod.rs`: module registration
- `cli/src/native/cdp/client.rs`: `subscribe_session_with_buffer`, so the renderer's request gate gets a buffer that does not drop `Fetch.requestPaused` (`subscribe_session` keeps the upstream 16-event, drop-newest behavior)
- `cli/src/native/cdp/lightpanda.rs`: `find_lightpanda` also checks the install directory
- `cli/src/native/cdp/chrome.rs`: the "Chrome cache directory" warning ignores Lightpanda entries
- `cli/src/install.rs`: `run_install` installs Lightpanda first; Chrome install moved to `install_chrome`
- `cli/src/main.rs`: `renderer` command, install flags, launch warning printed on stderr
- `cli/src/output.rs`: response warnings printed for every action, help text
- `cli/src/flags.rs`, `cli/src/commands.rs`, `cli/src/mcp.rs`, `agent-browser.schema.json`: `--screenshot-renderer` plumbing, per-command `rendererToken` forwarding, and MCP parity
- `cli/src/native/actions.rs` also strips `rendererToken` from dashboard command broadcasts
- `cli/tests/tls_cli.rs`: the Chrome manifest trust test runs `--engine chrome install`
- `scripts/postinstall.js`: install reminder mentions Lightpanda
- Documentation: `README.md`, `skill-data/core/`, `docs/content/docs/`

The upstream e2e suite assumes Chrome without naming an engine, so test builds resolve an unset engine to Chrome (see `resolve_launch_engine`). Fork behavior is covered by unit tests in `engine.rs` and `render/` and by `e2e_lightpanda_screenshot_renderers`.

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

Bump `LIGHTPANDA_VERSION` in `cli/src/lightpanda_install.rs` when adopting a new Lightpanda release.
