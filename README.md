# light-agent-browser

Lightweight browser automation for AI agents, powered by [Lightpanda](https://github.com/lightpanda-io/browser): open pages, read them as an accessibility tree, and act on elements from the shell.

<p>
  <a href="https://www.npmjs.com/package/light-agent-browser"><img alt="npm version: light-agent-browser" src="https://img.shields.io/npm/v/light-agent-browser.svg?style=for-the-badge&amp;labelColor=000000" height="28"></a>
  <a href="https://github.com/Curling-AI/light-agent-browser/blob/main/LICENSE"><img alt="License: Apache-2.0" src="https://img.shields.io/github/license/Curling-AI/light-agent-browser.svg?style=for-the-badge&amp;labelColor=000000" height="28"></a>
</p>

light-agent-browser is a fork of [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) that uses Lightpanda as the default engine, keeps Chrome as an automatic fallback, and renders screenshots of Lightpanda pages through a local or shared Chrome renderer. It installs the same `agent-browser` command and accepts the same commands, flags, environment variables, and config files, so existing scripts, skills, and MCP setups keep working. See [Lightpanda Engine and Screenshot Rendering](#lightpanda-engine-and-screenshot-rendering) and [FORK.md](FORK.md).

Install either light-agent-browser or upstream agent-browser globally, not both: they provide the same `agent-browser` command.

agent-browser is a native Rust CLI that gives an AI agent a browser it can drive one command at a time. `snapshot` returns the page as compact text with a ref such as `@e1` for each element. The agent acts on a ref, reads the page again to check the result, and repeats. The browser stays open between commands, so each step is one short shell call.

The documentation links below point to [agent-browser.dev](https://agent-browser.dev), the upstream site. It describes Chrome as the default engine; this README lists what the fork changes.

## Install

```bash
npm install -g light-agent-browser
agent-browser install           # download Lightpanda, the default engine (first time only)
```

- **Chrome:** `agent-browser install --with-chrome` also downloads Chrome for Testing, which renders screenshots and PDFs of Lightpanda pages, serves Chrome-only options, and is the engine on Windows. `agent-browser --engine chrome install` installs only Chrome.
- **Linux:** `agent-browser install --with-deps` also installs Chrome and its system libraries.
- **Cargo:** `cargo install --git https://github.com/Curling-AI/light-agent-browser agent-browser`, then `agent-browser install`.
- There is no Homebrew formula for light-agent-browser; `brew install agent-browser` installs upstream agent-browser.

`agent-browser upgrade` detects the installation method (npm, pnpm, yarn, bun, or Cargo) and upgrades the `light-agent-browser` package. Project installs, `upgrade`, and `doctor` are covered in [Installation](https://agent-browser.dev/installation).

## Quick start

```bash
agent-browser open example.com
agent-browser snapshot -i       # interactive elements, each with a ref
agent-browser click @e1         # act on a ref from that snapshot
agent-browser get url           # check the result
agent-browser close
```

The snapshot for `example.com`:

```
- link "Learn more" [ref=e1]
```

- Refs describe the page at the time of the snapshot. After navigation or a DOM change, take a new snapshot before using refs again.
- CSS selectors, text, XPath, and semantic locators also work, for example `agent-browser find role button click --name "Submit"`. See [Selectors](https://agent-browser.dev/selectors).
- A click fails, instead of reporting success, when the target is hidden, disabled, outside the viewport, or covered by another element such as a consent banner. Dismiss the covering element, take a new snapshot, and retry.

The full walkthrough is in the [quick start](https://agent-browser.dev/quick-start).

## How it works

The CLI sends each command to a background daemon that starts on the first command and controls the browser over the Chrome DevTools Protocol. Each `--session` gets its own daemon and browser. By default, the daemon closes a headless browser and exits after one hour without commands, saving state first when [session persistence](https://agent-browser.dev/sessions) is configured.

Lightpanda is the default engine, with Chrome for Testing as the fallback (see [Engine selection](#engine-selection)). You can also attach to a running Chrome with `--cdp` or `--auto-connect` ([CDP mode](https://agent-browser.dev/cdp-mode)), select the Chrome or [Obscura](https://agent-browser.dev/engines/obscura) engine with `--engine`, drive Mobile Safari in the [iOS Simulator](https://agent-browser.dev/ios), or use a cloud browser provider such as [Browserbase](https://agent-browser.dev/providers/browserbase), [Browserless](https://agent-browser.dev/providers/browserless), [Browser Use](https://agent-browser.dev/providers/browser-use), [Kernel](https://agent-browser.dev/providers/kernel), or [AgentCore](https://agent-browser.dev/providers/agentcore).

## For AI agents

The CLI ships skills that match its version:

```bash
agent-browser skills get core                 # the core loop, refs, waiting, and common workflows
agent-browser skills get core --ref commands  # one reference, such as the full command list
agent-browser skills get core --full          # adds the full command reference and templates
agent-browser skills list                     # specialized skills: electron, slack, dogfood, and more
```

Install the discovery skill into an agent harness:

```bash
npx skills add Curling-AI/light-agent-browser
```

The installed skill is a thin stub that tells the agent to load `agent-browser skills get core`, so its instructions always match the installed CLI. Or add this to your `AGENTS.md`:

```markdown
Use `agent-browser` for browser automation. Before the first use, run
`agent-browser skills get core` and follow it.
```

For MCP clients, `agent-browser mcp` starts an MCP server over stdio. See [MCP server](https://agent-browser.dev/commands#mcp-server).

## Commands

Run `agent-browser --help` for the full list, or `agent-browser <command> --help` for one command.

| Group | Commands | Docs |
|---|---|---|
| Navigate and read | `open`, `back`, `forward`, `reload`, `read`, `snapshot`, `get`, `is` | [Core](https://agent-browser.dev/commands#core), [Snapshots](https://agent-browser.dev/snapshots) |
| Interact | `click`, `dblclick`, `fill`, `type`, `press`, `keyboard`, `hover`, `focus`, `select`, `check`, `uncheck`, `scroll`, `drag`, `upload`, `mouse`, `find` | [Core](https://agent-browser.dev/commands#core), [Find elements](https://agent-browser.dev/commands#find-elements) |
| Wait | `wait` | [Wait](https://agent-browser.dev/commands#wait) |
| Capture | `screenshot`, `pdf`, `record`, `download`, `diff` | [Recording](https://agent-browser.dev/recording), [Diffing](https://agent-browser.dev/diffing), [Files](https://agent-browser.dev/files) |
| Page and network state | `cookies`, `storage`, `network`, `set`, `clipboard`, `eval`, `pushstate` | [Network](https://agent-browser.dev/network), [Settings](https://agent-browser.dev/commands#settings) |
| Tabs, frames, and dialogs | `tab`, `frame`, `dialog` | [Tabs and frames](https://agent-browser.dev/commands#tabs-frames) |
| Debug and performance | `console`, `errors`, `highlight`, `inspect`, `trace`, `profiler`, `vitals`, `react`, `a11y` | [Debugging](https://agent-browser.dev/debugging), [Profiler](https://agent-browser.dev/profiler), [React](https://agent-browser.dev/react) |
| Sessions and auth | `session`, `state`, `auth`, `profiles`, `confirm`, `deny` | [Sessions](https://agent-browser.dev/sessions), [Security](https://agent-browser.dev/security) |
| Agents and integrations | `skills`, `mcp`, `webmcp`, `flow`, `chat`, `batch`, `plugin` | [Skills](https://agent-browser.dev/skills), [WebMCP](https://agent-browser.dev/webmcp), [Plugins](https://agent-browser.dev/plugins) |
| Observe | `stream`, `dashboard` | [Streaming](https://agent-browser.dev/streaming), [Dashboard](https://agent-browser.dev/dashboard) |
| Setup | `install`, `upgrade`, `doctor`, `connect`, `close` | [Installation](https://agent-browser.dev/installation) |
| Screenshot renderer | `renderer serve`, `renderer token` | [Remote renderer](#remote-renderer) |

Flags, environment variables, and `agent-browser.json` are documented in [Configuration](https://agent-browser.dev/configuration).

## Lightpanda Engine and Screenshot Rendering

[Lightpanda](https://github.com/lightpanda-io/browser) is the default engine. It starts in milliseconds and uses a fraction of Chrome's memory, which suits agents that mostly navigate, read, and fill forms.

### Engine selection

When you pass `--engine` (or set `AGENT_BROWSER_ENGINE` or `engine` in config), that engine is always used. Without an explicit engine, agent-browser launches Lightpanda unless it cannot serve the launch, in which case it uses Chrome and prints a warning naming the reason:

- the platform has no Lightpanda build (Windows)
- Lightpanda is not installed (`agent-browser install` downloads it)
- `--executable-path` points at a binary that is not Lightpanda
- a Chrome-only option is set: `--headed`, `--profile`, `--extension`, `--state`, `--webgpu`, `--ca-cert`, or `--args`

`--cdp`, `--auto-connect`, and `--provider` attach to an existing browser and are unaffected. Pass `--engine chrome` to keep the previous Chrome default and silence the warning.

### Opening file:// pages

Lightpanda only speaks http(s), so agent-browser serves `file://` pages to it. `open file:///path/page.html` navigates Lightpanda to `http://file.agent-browser.invalid/path/page.html` and the daemon answers that host from disk through CDP `Fetch`, with the page's scripts, stylesheets, images, and links. Folders open as a listing. The `.invalid` host never resolves, so nothing leaves the machine. `open`, `get url`, `back`, and `tab list` show the `file://` URL; scripts in the page see the http one (`location.protocol` is `http:`).

Every served file shares that one origin, so a page could `fetch()` any file the host serves. Requests are limited to the folders of the files the session opened (and their subfolders, after resolving `..` and symlinks); anything else gets a 403. `--allow-file-access` lifts the limit. The folders are forgotten on `close`. File pages cannot be opened while `--allowed-domains` is set.

Lightpanda has no PDF viewer. Opening a local `.pdf` relaunches the session with Chrome and prints a warning when the engine came from the default; with an explicit `--engine lightpanda` it fails with a hint.

### Screenshots

Lightpanda has no layout engine, so its own `Page.captureScreenshot` only draws the page text. To get a real screenshot, agent-browser serializes the current Lightpanda DOM (scripts and inline event handlers removed, form values and CSSOM-inserted styles kept, password values masked), collects the page cookies, viewport, scroll position, and color scheme, and renders that in Chrome at the original URL. Selector screenshots, `--full`, `--annotate`, and JPEG all work. `--json` output reports the renderer in `data.renderer`.

Choose the renderer with `--screenshot-renderer` (or `AGENT_BROWSER_SCREENSHOT_RENDERER`, or `screenshotRenderer` in config):

| Value | Behavior |
| --- | --- |
| `auto` (default) | Local Chrome when one is installed, otherwise `native` |
| `chrome` | Local headless Chrome, launched on the first screenshot and kept until `close`. Fails when Chrome is not installed |
| `native` | Lightpanda's text-only PNG render. No extra dependencies |
| `http(s)://host:port` | Remote renderer service (see below) |

```bash
agent-browser open https://example.com          # Lightpanda
agent-browser screenshot page.png               # Rendered by local Chrome when available
agent-browser --screenshot-renderer native screenshot text.png
```

PDFs use the same path: with Lightpanda, `agent-browser pdf` sends the serialized page to the renderer, which prints it with Chrome's `Page.printToPDF`, so the PDF keeps the page's layout and styles. `--screenshot-renderer` applies to both, `native` produces Lightpanda's own text-only PDF (selectable text and links, no CSS), and `--json` reports `data.renderer`. A renderer service from before PDF support returns an error instead of a PDF; update it.

Pages a serialized render cannot reproduce are captured live instead: when the page has a visible `<canvas>` (Chart.js, ECharts) or a chart that measures its own SVG (ApexCharts, Google Charts), and, with a remote renderer only, when it is served from `file://`, loopback, or a private address (which a shared renderer cannot reach and refuses), a short-lived local Chrome opens the page with the session cookies, lets its scripts run, captures, and exits. `--json` then reports `"renderer": "chrome-live"` and `rendererReason` (`canvas`, `layout-chart`, `local-file`, or `local-server`). The live capture reloads the URL, so state that only exists after interactions is lost, and it is skipped with `--annotate`, with `--screenshot-renderer native`, and while `--allowed-domains` is active. A local renderer reaches local pages itself and paints the session's DOM, keeping typed values; for a `file://` page it reads the page's stylesheets and images from the same allowed folders. When a live capture is needed but no Chrome is installed, the configured renderer captures what it can and the command warns (`rendererWarning` in `--json`). A shared renderer never captures live: the flag that requests it never crosses the wire.

Limitations of the serialized render: video frames, closed shadow roots, and cross-origin iframe state are not reproduced, nor are canvas pixels when no local Chrome is available for a live capture. Iframes reload from the network. Element screenshots inside iframes are not supported. The renderer fetches subresources directly, not through `--proxy`. While `--allowed-domains` is active, Chrome rendering is disabled because it would load resources outside the filter: `auto` uses the native render and `chrome` or a renderer URL fails.

### Remote renderer

Run the renderer as a shared service so agent machines only need Lightpanda:

```bash
AGENT_BROWSER_RENDERER_TOKEN=change-me agent-browser renderer serve --host 0.0.0.0 --port 9300
```

Agents point at it:

```bash
export AGENT_BROWSER_SCREENSHOT_RENDERER=http://renderer.internal:9300
export AGENT_BROWSER_RENDERER_TOKEN=change-me
agent-browser screenshot
```

Each `screenshot` call forwards the caller's current `AGENT_BROWSER_RENDERER_TOKEN` to the session daemon (falling back to the daemon's own environment), so changing it needs no restart. The token is stripped from the command events shown in the dashboard. The service exposes `POST /v1/render` and `GET /healthz`, renders each request in an isolated Chrome browser context, and accepts `--concurrency` (default 4), `--max-body-mb` (default 32), and `--recycle-after` (default 200 renders before Chrome is relaunched). It treats every request as untrusted: page scripts are disabled, subresources load only over http(s) from public addresses (loopback, private, link-local, and CGNAT ranges are refused, which covers cloud metadata endpoints), each render has an 85 second deadline, and viewports are capped at 4096 pixels per side with a device scale factor of 2. Requests contain the page HTML and its cookies, so the service refuses to listen on a non-loopback address without `AGENT_BROWSER_RENDERER_TOKEN` (pass `--allow-unauthenticated` to override). Keep it on a private network and use TLS (for example, through your ingress) when traffic leaves the host. The address check resolves each host before Chrome fetches it, so a network policy around the service is still the hard boundary against DNS rebinding.

### Configuring everything through environment variables

Every setting has an environment variable, so an orchestrator can make Lightpanda plus a shared renderer the default for all agents without passing flags:

```bash
export AGENT_BROWSER_ENGINE=lightpanda                              # optional: lightpanda is already the default
export AGENT_BROWSER_SCREENSHOT_RENDERER=http://agent-browser-renderer:9300
export AGENT_BROWSER_RENDERER_TOKEN=change-me
agent-browser open https://example.com
agent-browser screenshot page.png                                   # rendered by the shared service
```

The same variables apply to `agent-browser mcp`, whose tools run the CLI with the server's environment. MCP callers can pick `auto`, `chrome`, or `native` with a tool's `renderer` argument, but not a renderer URL, which only the server's environment or config can set. Precedence is flag, then environment variable, then `screenshotRenderer` in `agent-browser.json`. The renderer service itself reads `AGENT_BROWSER_RENDERER_HOST`, `AGENT_BROWSER_RENDERER_PORT`, `AGENT_BROWSER_RENDERER_CONCURRENCY`, `AGENT_BROWSER_RENDERER_RECYCLE_AFTER`, `AGENT_BROWSER_RENDERER_TOKEN`, `AGENT_BROWSER_RENDERER_HMAC_KEY`, `AGENT_BROWSER_RENDERER_PER_CALLER_CONCURRENCY`, `AGENT_BROWSER_RENDERER_PER_CALLER_PER_MINUTE`, and `AGENT_BROWSER_EXECUTABLE_PATH`.

For a fleet, give each agent its own token instead of sharing one. With `AGENT_BROWSER_RENDERER_HMAC_KEY` set, the service accepts `<caller>.<hex HMAC-SHA256(key, caller)>` (caller ids use letters, digits, `-`, `_` and `:`), so an orchestrator can issue tokens without calling the service, and `agent-browser renderer token <caller>` prints one. Each caller is limited on its own (`--per-caller-concurrency`, default 2; `--per-caller-per-minute`, default 30) and gets `429` with `Retry-After` past either; requests on the shared token are not limited per caller. `GET /metrics` serves Prometheus counters (requests by outcome, in-flight renders, a render duration histogram, Chrome launches) without caller labels, and every request logs one JSON line with its caller, outcome, status, duration and page host (never the full URL).

A container image and a Kubernetes example live in [`docker/renderer/`](docker/renderer/):

```bash
docker build -f docker/renderer/Dockerfile -t agent-browser-renderer .
docker run --rm -p 9300:9300 -e AGENT_BROWSER_RENDERER_TOKEN=change-me agent-browser-renderer
```

## Coherent Chrome

`AGENT_BROWSER_COHERENT_CHROME=1` launches Chrome without the headless markers: `navigator.webdriver`, the 800x600 screen and, on headless Linux only, the `HeadlessChrome` User-Agent. The daemon reads it from its environment, so it never relaunches a running browser. An explicit `--user-agent` or `--screen-info` still wins, and the screen follows an explicit window size.

It only changes how Chrome launches and does not select Chrome: a session on Lightpanda stays there until it moves to Chrome.

## Fork options

| Option | Description |
| --- | --- |
| `--engine <name>` | Browser engine: `lightpanda` (default; falls back to Chrome when unavailable, see [Engine selection](#engine-selection)), `chrome`, `obscura` (experimental; rejects proxy bypass rules) (or `AGENT_BROWSER_ENGINE` env) |
| `--screenshot-renderer <mode>` | Renderer for Lightpanda screenshots and PDFs: `auto` (default), `chrome`, `native`, or an `http(s)://` renderer URL (or `AGENT_BROWSER_SCREENSHOT_RENDERER` env) |
| `--allow-file-access` | Allow file:// pages to read any local file (Chromium flags; on Lightpanda, lifts the opened-folder limit) |
| `install --with-chrome` | Also install Chrome for Testing |
| `AGENT_BROWSER_RENDERER_TOKEN` env | Bearer token sent to a remote renderer, and required by `renderer serve` when set |
| `AGENT_BROWSER_RENDERER_HMAC_KEY` env | Key `renderer serve` uses to accept per-caller tokens |
| `AGENT_BROWSER_LIGHTPANDA_VERSION` env | Lightpanda release downloaded by `install` (default `1.0.0`; `nightly` always re-downloads) |
| `AGENT_BROWSER_COHERENT_CHROME` env | Launch Chrome without the headless markers (see [Coherent Chrome](#coherent-chrome)) |

## Platforms

Native binaries ship for macOS (ARM64, x64), Linux (ARM64, x64, glibc and musl), and Windows (x64). The `light-agent-browser` npm package bundles them and runs the one for the host. Lightpanda builds exist for macOS and Linux; Windows uses Chrome.

## Development

Building from source requires Node.js 24 or later, pnpm 11 or later, and Rust:

```bash
git clone https://github.com/Curling-AI/light-agent-browser
cd light-agent-browser
pnpm install
pnpm build:source               # builds the dashboard and the native CLI
pnpm link --global
agent-browser install
```

Run the Rust checks from `cli/`:

```bash
cargo fmt -- --check
cargo clippy
cargo test
cargo test e2e -- --ignored --test-threads=1   # launches real headless Chrome
LIGHTPANDA_BIN=~/.agent-browser/browsers/lightpanda-1.0.0/lightpanda \
  cargo test e2e_lightpanda -- --ignored --test-threads=1
```

[AGENTS.md](AGENTS.md) lists the files to update with each change, the docs checks, and the release process. [FORK.md](FORK.md) lists where the fork differs from upstream and how to sync it.

## Documentation

- [agent-browser.dev](https://agent-browser.dev): upstream's introduction, guides, and reference
- [Commands](https://agent-browser.dev/commands) and [Configuration](https://agent-browser.dev/configuration)
- [Security](https://agent-browser.dev/security): the auth vault, domain allowlists, action policies, and content boundaries
- [Next.js and Vercel](https://agent-browser.dev/next) and [eve](https://agent-browser.dev/eve): running agent-browser in Vercel Sandbox and eve agents
- [Changelog](CHANGELOG.md)

## License

Apache-2.0
