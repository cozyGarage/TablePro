# Optional upstream reference review

TablePro Linux is an independent Rust and GTK codebase. Other TablePro implementations may be reviewed as references for security fixes, product behavior, SQL semantics, and user expectations. This review is optional and is not a source synchronization process.

## Rules

- Never merge, rebase, or cherry-pick Apple source trees into this repository.
- Do not copy platform framework code or assume another implementation's architecture applies to Linux.
- Inspect the behavior and the reason for the change.
- Check whether the same risk or product need exists in the Rust and GTK application.
- Manually implement only the behavior that applies to Linux.
- Keep existing Linux policy, audit, storage, GTK ownership, and driver boundaries intact.
- Add Rust tests that prove the ported behavior here.
- Omit changes that depend on platform services with no Linux equivalent unless there is a clear Linux product requirement.

## Good review targets

Reference review is most useful for:

- SQL safety classification and approval rules
- Credential handling and redaction
- Connection cancellation and reconnect behavior
- TLS and SSH identity checks
- Data-grid correctness and destructive-action guards
- User-facing behavior that should be consistent across TablePro products

Packaging, desktop integration, process management, keyrings, and UI framework details should follow native Linux behavior instead.

## Recording a manual port

Add an entry only when a reference review causes a Linux code or product change. Routine Linux development does not need an entry.

```text
## YYYY-MM-DD: short behavior name

- Reference reviewed: repository, ref, and commit or release.
- Linux relevance: risk or product behavior that also applies here.
- Manual port: Rust or GTK behavior implemented in this repository.
- Not ported: platform-specific parts that do not apply.
- Verification: focused tests, real-driver fixture, or GTK flow run.
```

The entry should describe behavior, not file-by-file source movement. There should be no source-tree merge to record.
