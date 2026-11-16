# A cluster is played from its entry while the render is still writing it

ADR 0010 says a pass without entries plays live, and that the render swaps in when it finishes. For a project that is heavy enough to need the cache, that leaves the whole device callback computing every cluster from the pass start until the last frame of the (at least 30 s) render is written. The callback is then at its busiest exactly when a small device buffer (Monitoring latency, #548) needs it lightest.

The render is not slow: it runs flat out on a worker thread, many times faster than real time. What was missing was a way to use it before it is done. So an entry may now be **played while it fills**. A cluster plays from its entry once the render has written two seconds past the playhead (`AHEAD_JOIN_SECONDS`), and goes back to playing live if the lead falls below 0.4 s (`AHEAD_KEEP_SECONDS`), which happens only when the render is slower than the playhead. A cluster that has been given up stays live until its entry is whole, so a slow render does not flap between the two.

How it sits with ADR 0010:

- The key, the tiers, the origins and what an entry holds are unchanged. A growing entry is the same file under the same key, read before its last frame is on disk. The device buffer and the engine block do not change what is rendered (the block is in the key), so the device buffer moving never invalidates an entry.
- "A pass without entries plays live" becomes "a pass without whole entries plays live until its render is ahead of the playhead". Pause then play is still not cached, and a seek is still a new pass whose entry is rendered from its start.
- The hand-over is a recompile, as the swap at the end of a render always was: live to entry when the lead is made, entry to live when it is lost, and nothing at all when the render finishes, because the player's config names the entry and not whether it was whole.
- An edit changes the key, so the old entry comes off at once and the cluster plays live, which is how a tweak is heard immediately. The render for the new key starts when the editing settles, and the cluster goes back to the entry when that render is ahead.
- Only a single entry is played this way. A loop's laps wait for their renders to finish.
- A render that is cancelled or fails leaves nothing: the entry is `abandoned`, the player is given up and the plan is made again.
- Monitoring latency Off keeps the old rule (live until the render finishes). Auto and Always low play ahead.

Rejected: rendering ahead *in step with* the playhead (a render that waits for it). The render is much faster than real time, so waiting wastes the head start; only the lead matters. Also rejected: having the callback fall back to live inside the player itself, which would need the whole live subgraph kept in the plan next to the entry and so no saving at all.
