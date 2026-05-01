# USRP call metadata

The bridge emits a one-shot USRP TEXT frame at the start of each
inbound DMR call (and on stream changes) so an ASL3 dialplan can
display, log, or route based on talker info without parsing DMR
itself.  An empty `{}` payload follows on call end.

## Wire format

USRP frame `frame_type = 2` (Text), header otherwise zero except
`seq` and the `USRP` magic.  Payload is a UTF-8 JSON string,
NUL-terminated (matches the chan_usrp / DVSwitch convention so
consumers that strncpy the buffer don't read past the end).

The JSON shape (no subscriber lookup configured):

```json
{"dmr_id":3107702,"tg":91,"slot":1,"cc":1}
```

With `[repeater].subscriber_file` pointing at a RadioID-style
`user.csv`, hits add `call` and `name` (operator's first name):

```json
{"dmr_id":3107702,"tg":91,"slot":1,"cc":1,"call":"AI6KG","name":"Christopher"}
```

The bridge does not fetch the CSV itself.  Download it once and
refresh on whatever cadence you like (RadioID updates daily but
hourly refresh is overkill for most operators):

```
sudo mkdir -p /var/lib/asl-dmr-bridge
sudo wget -O /var/lib/asl-dmr-bridge/user.csv https://radioid.net/static/user.csv
```

Then point the config at it:

```toml
[repeater]
subscriber_file = "/var/lib/asl-dmr-bridge/user.csv"
```

A `systemd` timer or cron entry hitting that same `wget` weekly is
plenty.  The bridge re-reads the file only on (re)start.

Field meanings:
- `dmr_id` -- talker's on-air subscriber ID (the DMRD `src_id` field).
- `tg` -- talkgroup for group calls; addressee's subscriber ID for
  unit-to-unit (private) calls.
- `slot` -- 1 or 2.
- `cc` -- color code from the bridge config (0..15).
- `call` -- optional, only when the lookup hits.
- `name` -- optional, operator's first name when known.

Empty object `{}` (2 bytes plus terminator) on call end / RX
timeout / shutdown -- consumers that latch should clear their state.

## When frames are emitted

| Event | Payload |
|---|---|
| RX header (start of call) | populated JSON |
| Inbound stream-id change mid-call (different talker) | populated JSON |
| Implicit RX start (voice burst with no preceding header) | populated JSON |
| RX terminator (clean call end) | `{}` |
| RX stream timeout (lost network) | `{}` |
| Bridge shutdown while in RX | `{}` |

All emissions are best-effort `try_send` -- if the metadata channel
backs up, frames are dropped silently rather than stalling voice.
The metadata path never gates voice.

## Consuming from ASL3

Stock `chan_usrp.c` delivers the TEXT payload as `AST_FRAME_TEXT` to
the channel.  In `extensions.conf`, you can see the text via the
`TEXT()` event in your channel hook or by capturing it from the
USRP channel context:

```asterisk
exten => h,1,NoOp(call ended)
; A more useful pattern is to log inbound text frames into a
; rolling file via a channel hook or AGI; see ASL3's app_rpt
; documentation for the current best practice in your version.
```

This is intentionally minimal documentation -- the dialplan side is
your problem, and what counts as best practice changes with ASL3
versions.  The contract on the wire side is fixed: one TEXT frame
on every call boundary, JSON shape locked by
`dmr_events::tests::call_metadata_json_shape_no_lookup` /
`call_metadata_json_shape_with_lookup`.
