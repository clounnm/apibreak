# apibreak

**Break your API with one key: no certificates, no scripts.**

![apibreak demo](demo.gif)

apibreak is a local reverse proxy with a terminal UI, made for frontend
developers. Point your app at `localhost:4000` instead of the real API, watch
every request, and press a key to make an endpoint fail so you can see how your
UI copes with a 500, an expired token, a slow response or a dropped connection.

There is nothing to set up: no HTTPS interception, no CA certificates, no system
proxy.

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/clounnm/apibreak/releases/latest/download/apibreak-installer.sh | sh
```

On Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/clounnm/apibreak/releases/latest/download/apibreak-installer.ps1 | iex"
```

## Quick start

1. Start the proxy in front of your backend:

   ```sh
   apibreak --target https://api.example.com
   ```

2. Change your frontend's API base URL to `http://localhost:4000`.
3. Use your app. Requests appear as they happen.
4. Select a request and press `5`. That endpoint now returns 500.
5. Press `x` to bring it back.

No backend handy? This repository includes a small one:

```sh
cargo run --example demo-server                     # http://localhost:3000
cargo run -- --target http://localhost:3000         # in a second terminal
curl localhost:4000/api/users                       # in a third
```

## Keys

Scenario keys act on the selected request's endpoint. Ids in the path become a
pattern, so pressing `5` on `/api/users/7` breaks `GET /api/users/:id`. Pressing
a scenario's key again turns it off. Each endpoint has one response scenario at
a time, and `d` can be combined with any of them.

| Key | Scenario |
|---|---|
| `5` | Return 500 |
| `4` | Return 401 (token expired) |
| `n` | Return 404 |
| `e` | Empty response: `[]` if the endpoint returned an array, otherwise 204 |
| `d` | Delay the response, 3 s by default |
| `o` | Offline: drop the connection without answering |
| `m` | Serve the recorded mock instead of calling the backend |
| `x` | Remove all scenarios from this endpoint |

| Key | Action |
|---|---|
| `r` | Record the response as a mock in `.apibreak/mocks/` |
| `E` | Open the mock in `$EDITOR` |
| `j` / `k` | Move, or scroll the details panel |
| `g` / `G` | First / last |
| `/` | Filter by path |
| `Tab` | Switch panel |
| `c` | Clear the list |
| `p` | Pause recording |
| `?` | Help |
| `q` | Quit |

## Scenarios in your repository

Scenarios are saved to `.apibreak/scenarios.yaml`, so you can commit them and
share them with your team. Edits to the file apply while apibreak runs, and if
the file stops parsing, the last good scenarios stay active until you fix it.

```yaml
target: https://api.example.com
port: 4000
default_delay: 3s
scenarios:
  - match: "GET /api/users"
    action: status
    status: 500
  - match: "POST /api/login"
    action: delay
    delay: 3s
  - match: "GET /api/users/:id"
    action: mock
    file: mocks/GET_api_users_id.json
  - match: "GET /api/feed"
    action: empty
  - match: "GET /api/health"
    action: offline
```

`--target`, `--port` and `--delay` override the file. Changing a scenario in the
TUI rewrites the file, which removes any comments in it.

## Headless mode

`apibreak --headless` runs the proxy with the scenarios from the file and logs to
stdout instead of opening the TUI. Use it in CI or for demos.

```text
apibreak :4000 -> https://api.example.com
scenarios: GET /api/users → 500
20:29:13 GET /api/users 500 0ms [500]
```

## Options

| Flag | Default | Meaning |
|---|---|---|
| `--target <url>` | from file | Backend to proxy to |
| `--port <port>` | `4000` | Local port |
| `--cors[=false]` | on | CORS headers and preflight answers for localhost origins |
| `--delay <time>` | `3s` | Delay for the `d` key: `500ms`, `3s`, `2m` |
| `--history <n>` | `500` | Requests kept in memory |
| `--headless` | off | Run without the TUI |

apibreak listens on loopback only. CORS is answered for localhost origins only,
so other websites open in your browser cannot read your API through the proxy.
Scenario responses carry CORS headers too, so your app sees the 500 rather than
a CORS error.

## How it differs from mitmproxy and HTTP Toolkit

[mitmproxy](https://mitmproxy.org) and [HTTP Toolkit](https://httptoolkit.com)
are more capable. They intercept traffic from any app, including HTTPS, and they
can script or rewrite almost anything. If you need that, use them.

apibreak does one narrower job: breaking your own API while you build a frontend.
Because it is a reverse proxy you simply point your app at, it needs no
certificates or proxy settings, and it makes a failure one key press. The
trade-off is that it only sees traffic sent to it, and only over plain HTTP from
your app to the proxy. It also has no scripting, and no WebSocket support yet.

## Known limits

- Request bodies are buffered, up to 256 MiB. Responses are streamed.
- The details panel shows up to 256 KiB per body, and bodies over 2 MiB cannot be
  recorded as mocks.
- Binary responses cannot be recorded as mocks.
- gzip is the only compression apibreak can read, so it asks the backend for gzip
  when the client accepts any compression.

See [ROADMAP.md](ROADMAP.md) for what may come next, and
[CONTRIBUTING.md](CONTRIBUTING.md) to help.

## License

[MIT](LICENSE)
