# The API contract

`spendyard.api.v1`: what the CLI and Spendyard Cloud say to each other, as
Connect services in protobuf. The platform (`../platform`) generates its
TypeScript from here (`proto/buf.gen.yaml` there); the CLI's Rust comes
later, with `spendyard login cloud`, `push` and `pull` (C2 in the platform's
docs/cloud.md).

```bash
buf lint && buf build      # here
buf breaking --against '.git#subdir=proto'
```

The module is `buf.build/spendyard/spendyard`; once it's published, the
platform pins a commit of it instead of reading this folder.
