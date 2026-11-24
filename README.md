# QREAM

QREAM turns selected requirements into a campaign that people can reopen and inspect. A requirement carries its purpose, source, and definition of done into preparation. That gives the person reviewing the work a way to trace it back to the request.

## Prepare one campaign

The [worked example](examples/prepare.rs) starts with a small document review. Its requirement is concrete: a reviewer must be able to reopen the campaign and find the selected requirement. From the repository root on Linux, use Rust 1.94.0, a C compiler, and Cargo.

```sh
campaign="$(mktemp -d)/document-review"
cargo run --locked --example prepare -- "$campaign"
cat "$campaign/OUTCOME.md"
```

The program prepares the campaign, reopens it, and checks that its outcome document exists. It then attempts a second creation in the same location. Success prints **A second creation was refused without replacing the campaign.** The folder stays on disk for you to inspect. This example starts no agents or daemon.

## Explore the runtime

```sh
cargo build --locked --bin qream --bin qreamd
./target/debug/qream --help
```

Add `--offline` to the Cargo commands when the locked crates are cached. The locked dependencies were fetched from the registry. Local verification covered campaign preparation, persistence, conflict refusal, and isolated daemon start, stop, and restart. Fresh-machine UI installation and the full graphical worker journey still need verification.

| To select… | Set |
| --- | --- |
| Runtime home, socket, or store | `QREAM_HOME`, `QREAM_SOCKET`, or `QREAM_STORE` |
| Standalone tracker database | `TRACKER_DB` |
| Namespace used by safety checks | `QREAM_NAMESPACE_ROOT` |

Defaults use the account home and XDG locations. The Linux account needs a local entry in `/etc/passwd`. Installers accept explicit tree, build-environment, target, and lock paths. The public build updates the bundled SQLite, async runtime, and spreadsheet writer from the original release. The [interface source](ui/) accompanies the retained 0.4.0 engine at revision `2a93d6735590`. See [dependency notices](NOTICES.md).

---

**[Explore OVRIN Labs](https://ovrinlabs.ca/)** · [Citium research](https://citium.ovrinlabs.ca/) · [Zavent data tools](https://zaventdata.com/)

[Apache-2.0 license](LICENSE) · [Contributing](CONTRIBUTING.md) · [Report a vulnerability privately](SECURITY.md)
