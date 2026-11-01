# Roadmap

Ideas deliberately left out of the first version. None of them is promised.

- WebSocket pass-through
- GraphQL awareness: scenarios per operation name
- Binary mocks
- Rewriting `Domain` and `Secure` on cookies from HTTPS backends
- `--insecure` for backends with self-signed certificates
- Custom status codes and bodies from the TUI
- Random failure rates, such as failing 20% of requests
- Keeping comments when saving `scenarios.yaml`
- A choice between `:id` patterns and exact paths when pressing a scenario key
- Export of recorded traffic to HAR

Out of scope by design: HTTPS interception, CA certificates and system proxy
settings.
