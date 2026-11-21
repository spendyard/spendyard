# The API contract

`spendyard.api.v1`: what the CLI and Spendyard Cloud say to each other, as
Connect services in protobuf. The platform (`../platform`) generates its
TypeScript from the published module (`proto/buf.gen.yaml` there); the CLI's Rust comes
later, with `spendyard login cloud`, `push` and `pull` (C2 in the platform's
docs/cloud.md).

```bash
buf lint && buf build      # here
buf breaking --against '.git#subdir=proto'
```

Published as the public module
[buf.build/spendyard/spendyard](https://buf.build/spendyard/spendyard): after a
change here is committed and pushed, `buf push --git-metadata` from this
folder, and the platform pins the new module commit (its
`proto/buf.gen.yaml`).
