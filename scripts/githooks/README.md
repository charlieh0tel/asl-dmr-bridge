# githooks

Pre-commit / pre-push guards: scan staged additions for personal DMR
IDs, bare credentials, and JWT-shape tokens.  Wire up once per clone:

```
git config core.hooksPath scripts/githooks
```

Override on a known-safe hit (e.g. a documented public test JWT)
with `git commit --no-verify`.
