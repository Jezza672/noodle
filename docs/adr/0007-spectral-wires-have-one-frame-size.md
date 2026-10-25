# Spectral wires have one frame size

A spectral wire carries FFT frames of 2048 samples every 512 (a Hann window, four-fold overlap). Every spectral node and wire uses these, so any spectral node feeds any other without the compiler working out a frame size per wire, and a node that only moves bins (a gate, a shift, a filter) never needs a resampling step between frames of different lengths.

The cost is that one resolution has to suit everything: 2048 at 48 kHz resolves about 23 Hz per bin, which is coarse for the bass and smears fast transients over about 43 ms. A choice of frame size (a config on Analyze that spectral nodes adopt from their input, as audio shapes propagate) is the planned way out, and the wire format already carries the lane shape, so adding a size to it is a compiler change, not a node API change. Until then, a patch that needs sharper time resolution does not use the spectral path.

Frames are timestamped within the block like events, rather than being delivered as one block-sized spectrum, because the engine's block size is the audio device's and is not a multiple of the hop. A node writes one frame out for each frame in, so none of them buffers samples.
