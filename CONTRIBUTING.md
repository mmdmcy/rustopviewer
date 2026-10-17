# Contributing to RustOp Viewer

Thanks for helping. The rules are simple:

- Go standard library only: no third-party modules.
- Nothing may listen on a network interface by default.
- Before a pull request:

```bash
gofmt -l .      # prints nothing
go vet ./...
go test ./...
```

Security reports go through a private advisory (see SECURITY.md), not an
issue.
