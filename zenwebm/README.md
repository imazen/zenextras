# zenwebm

Sans-I/O WebM/Matroska demuxing and live muxing with `zencodec-media`.
The muxer publishes headers at `begin`, then clusters at its ordinary packet,
duration and size boundaries. Drain `pending_output` and call `consume_output`,
or use `take_output`.

For immediate delivery at a complete media boundary, call
`WebmMuxer::flush_cluster(None)` after writing the packet, then drain the output.
This leaves the segment open and does not wait for another input packet.
It does not make a dependent video packet independently decodable. Callers
choose boundaries appropriate to their codec and track interleaving.

The explicit operation rejects calls before `begin`, checks optional cancellation
before changing output, and does nothing when no cluster is open. Default cluster
behavior is unchanged. `finish` still closes the final cluster.

`cargo test --manifest-path tools/webm-contract/Cargo.toml` runs the original
WebM suite and explicit-flush controls on a pinned shared contract without
resolving unrelated workspace codecs. This isolated graph does not qualify the
other workspace members.

The isolated suite passes 18/18 cases, including the unchanged default-cluster
assertions, per-packet prefix round trips, no-begin/empty/idempotent flushes,
and cancellation preserving the unpublished cluster. Strict Clippy also passes
(Ryzen 9 5900XT, 2026-11-09):

```text
run-heavy: done rc=0 1s | peak-RSS 0.17GiB | min-avail 48836MiB | peak-load 0.81
run-heavy: done rc=0 3s | peak-RSS 0.25GiB | min-avail 48716MiB | peak-load 0.53
```
