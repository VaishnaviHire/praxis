# Build features

Praxis is composed at build time with Cargo *features*. Several subsystems
are optional so you can build a proxy that carries only what a deployment
needs: a smaller binary, a leaner dependency tree, or no external
observability and admin surface at all.

Most features are on by default. Build the standard binary with:

```console
cargo build -p praxis-proxy --release
```

Build the leanest possible binary (every optional subsystem off) with:

```console
cargo build -p praxis-proxy --release --no-default-features
```

and add back individual features as needed:

```console
cargo build -p praxis-proxy --release --no-default-features --features admin-api
```

## Feature summary

| Feature | Default | Enables | Turn it off / on when |
| ------- | ------- | ------- | --------------------- |
| `config-reload` | on | Config-file and TLS-certificate hot-reload (filesystem watching). | Off for a static-config deployment: drops both watchers and the `notify`, `arc-swap`, and `tokio` dependencies they pull into the TLS crate. |
| `admin-api` | on | The admin HTTP service: management API (`/api/*`), Prometheus `/metrics`, and `/healthy` + `/ready`. | Off when the proxy exposes no monitoring or management surface. The data path and background health checks are unaffected; only the HTTP endpoints go away. |
| `otel` | off | OpenTelemetry / OTLP span export for traces. | On for distributed tracing. Pulls in a heavy `opentelemetry` + `tonic` dependency graph. |
| `policy-engine` | off | The `policy` filter (Praxis Policy Engine: OPA-style route policy, JWT identity, token exchange). | On for policy-based authorization. Heaviest optional dependency. |
| `basic-auth-filter` | off | The experimental `basic_auth` filter. | Dev and testing only. Slated for removal in favor of the policy engine ([praxis-proxy/policy]); prefer that for authentication. |
| `dev` | off | Developer convenience bundle (currently enables `basic-auth-filter`). | Local development builds. |
| `experimental` | off | Marker feature set transitively by experimental features; drives a startup warning. | Not selected directly; it lights up when an experimental feature is enabled. |

## Notes

- **Runtime still gates behavior.** Building with `admin-api` does not start
  the admin endpoints; they bind only when `admin.address` is configured. A
  listener's `hot_reload: true` key takes effect only when the binary was
  built with `config-reload`; otherwise the certificate is served statically
  and a startup warning is logged.
- **The memory allocator is not a feature.** Praxis targets Linux and always
  uses `tikv-jemallocator`; there is no build toggle for it.
- **Where the savings are.** Dropping `config-reload`, `admin-api`, `otel`,
  `policy-engine`, and `basic-auth-filter` is what trims the dependency tree
  and binary size. Most filters are always compiled in and share dependencies
  with the core proxy, so gating them individually would not remove a crate.

## See also

- [Filter Reference][filter-reference]: the per-filter `Feature` column shows
  which filters require a cargo feature.
- [Observability][observability]: metrics and tracing, gated by `admin-api`
  and `otel`.
- [TLS][tls]: the runtime `hot_reload` listener key, gated by `config-reload`.
- [Getting Started][getting-started]: the build and test workflow.

[praxis-proxy/policy]: https://github.com/praxis-proxy/policy
[filter-reference]: ../filters/reference.md
[observability]: observability.md
[tls]: tls.md
[getting-started]: ../developing/getting-started.md
