# Isolated browser environment

This optional example supplies an MCP environment for the runtime's native
Responses computer tool. It uses a new temporary Chromium profile for each MCP
connection. It does not connect to a person's browser or desktop.

Install the dependencies here with `npm install` and `npx playwright install chromium`.
The environment needs Node and a working Chromium installation; it is separate
from the Rust runtime. Launch it as a session-local stdio MCP server:

```json
{
  "name": "browser",
  "command": "node",
  "args": ["/absolute/path/to/browser.mjs", "--url", "https://example.com"],
  "env": []
}
```

Local stdio MCP is disabled by default. An owner must explicitly set
`allow_client_stdio_mcp: true` for trusted ACP clients to launch this example,
or host the environment behind an authenticated HTTP MCP endpoint. Granting
stdio MCP trusts that client with host command authority; the browser action
approval does not grant it.

Set `provider.native.computer` in the runtime configuration:

```json
{"mcp_server":"browser","mcp_tool":"computer","display_width":1280,"display_height":720}
```

Use an OpenAI/Azure Responses model with the native `computer` tool. Runtime
approval covers each ordered action batch, including screenshots. Read-only
sessions cannot control the environment; workspace mode does not bypass approval.
A client without interactive permission support cannot use it unattended.

Only the starting origin is reachable. Repeat `--allow-origin https://...` for
additional origins you intend to expose. Redirects and subresources are checked;
service workers, downloads, dialogs, popups and durable profiles are disabled.
An origin allowlist is a useful restriction, not a substitute for network
isolation. Deploy the driver in an isolated container/VM when browsing untrusted
sites. Do not point it at sensitive management pages without understanding the
access that gives the model. Browser page content is untrusted input.

`--width` and `--height` must match the runtime display dimensions. The runtime
validates action bounds before invoking the environment. The example implements
click, double click, typing, keys, scrolling, dragging, moving, wait and screenshot;
mouse back/forward/wheel click are explicitly refused. Failed batches can have
partial effects and are never automatically replayed.
