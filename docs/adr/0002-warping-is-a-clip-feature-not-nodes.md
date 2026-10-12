# Warping is a clip feature, not a set of nodes

Time warping belongs to audio clips and the track input that plays them. It is not a Warp node, and warp data is not a new kind of wire.

A warp reads its source at a position that is not the output position, so a node in the middle of an audio chain can't do it; it would need a "seekable source" port and a "warp data" port that nothing else in the graph can use. Those special ports would make the graph look flexible while offering nothing to rewire. We considered a clip-as-node design (clip node, Warp node, warp data wired between clips) and turned it down for that reason.

What is shared between clips is the groove, which is copied or linked as clip data, not wired. Algorithms are a mode of the clip's warp, not separate nodes. The existing offline Time Stretch node stays as the way to stretch anything in the graph after the fact.
