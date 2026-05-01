# Brandmeister Halligan API integration

The `brandmeister-api` crate is a typed Rust client for the
Brandmeister Halligan REST API (v2).  It powers two things in this
repo:

- `bmcli` -- a standalone CLI for ad-hoc peer / talkgroup queries and
  static-TG management.
- The bridge's `[brandmeister_api]` config section -- startup-time
  peer-profile log and optional pure-set static-TG reconciliation.

API base URL: `https://api.brandmeister.network/v2/`.  Upstream
OpenAPI: `https://api.brandmeister.network/api-docs`.

## Getting an API key

1. Log in to <https://brandmeister.network>.
2. Click your callsign (top right) -> "Profile Settings" -> "API
   Keys".
3. Generate a new key.  The value is a long opaque JWT (~1 KB).

The key authenticates *you*.  Mutations on a device require the
device to belong to your account.

## bmcli

```
cargo run -p bmcli -- <command> ...
```

Token sources (mutually exclusive, exactly one for write commands):

- `--api-key-file <path>` -- read a single-line JWT from a file
- `BRANDMEISTER_API_KEY=...` -- env var

Read commands (no token needed):

```
bmcli device <id>                  # info: callsign, freqs, last master
bmcli device <id> profile          # statics + dynamics + timed + blocks
bmcli device <id> statics          # current static TGs
bmcli talkgroup <id>               # talkgroup metadata
bmcli talkgroup <id> devices       # who has it static-subscribed
```

Write commands (token required):

```
bmcli device <id> static add --slot <1|2> --tg <n>
bmcli device <id> static remove --slot <1|2> --tg <n>
bmcli device <id> get-repeater     # live state from the master
bmcli device <id> drop-dynamic --slot <1|2>
```

## Bridge integration

Add a `[brandmeister_api]` section to the bridge config:

```toml
[brandmeister_api]
# Bearer JWT.  Pick exactly one source:
api_key = "..."                                  # inline
# api_key_file = "/etc/asl-dmr-bridge/bm.key"    # single-line file
# or set $BRANDMEISTER_API_KEY in the bridge's environment

# Pure-set reconciliation: declared list = final state at startup.
# Missing TGs are POSTed, extras are DELETEd.  Omit a slot to leave
# it untouched; `[]` reduces it to empty.  Requires an api_key.
static_talkgroups_ts1 = [91, 3100]
static_talkgroups_ts2 = []
```

What happens at bridge startup, before the BM master connect:

1. Anonymous `GET /device/{dmr_id}/profile` -- one INFO log line
   summarising statics / dynamics / timed presence.  Always runs;
   needs no token; failure logs a warning and the bridge continues.
2. If a token *and* at least one `static_talkgroups_tsN` list are
   configured: `GET /device/{dmr_id}/talkgroup`, compute
   `(adds, removes)` per slot, run `DELETE` then `POST` calls.  Per-
   call failures log an ERROR and the bridge continues.

The bridge does not gate startup on API success: the voice path
works regardless.

## Failure mode notes

- **HTTP 455 "The group field is required"** on add: the live BM API
  expects the body field name `group`, not `talkgroup` as the
  OpenAPI doc claims.  `brandmeister-api` already handles this; if
  you ever see this error, you're hitting it directly via curl.
- **Stringly-typed IDs.** `GET /device/{id}/talkgroup` returns
  `{"talkgroup":"91","slot":"1","repeaterid":"310770201"}` (ints as
  strings).  `brandmeister-api`'s deserializers accept either form.
- **Reconciliation cadence.** Runs once at startup.  Set
  `[brandmeister_api].reconcile_interval` (e.g. `"1h"`) to also re-
  reconcile periodically while the bridge is up; default `"0s"` is
  startup-only.  Either way, per-call failures log and the bridge
  continues.

## Examples

Confirm a peer's state from a fresh shell:

```
cargo run -p bmcli -- device 310770201 profile
```

Subscribe a peer to TG 91 on TS1, ad hoc:

```
BRANDMEISTER_API_KEY="$(cat ~/.config/asl-dmr-bridge/bm.key)" \
  cargo run -p bmcli -- device 310770201 static add --slot 1 --tg 91
```

Make the bridge ensure that subscription on every start: put the
key in `[brandmeister_api].api_key_file` and add `91` to
`static_talkgroups_ts1`.
