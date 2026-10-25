# Track model: mute, clip gain and crossfades

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** done. Closes #202 and #203. The design answers (Q1 to Q7) are in
`design/feedback-questions.md` in the project folder and on the issues.

1. **Mute acts at the track's outputs (#202).** A track's mute is the track
   input's `mute` parameter, still an output of the node for wiring. When
   nothing is wired to a track group output's mute, flatten drives that
   output's stage from it, so muting needs no wire in the track's graph and is
   smoothed like any stage mute. The track input stops its MIDI while muted and
   ends the notes it holds as the mute engages. Solo, the header, the mixer
   strip and `Automate Mute` all set the same parameter.
2. **No track gain.** The track input's `gain` parameter and output, the header
   slider, the `Automate Gain` entry and the track fader in the mixer's group
   strips are gone, as is the track template's gain wire. (A group output's own
   `gain` remains for nested groups and for the evaluation songs, which set
   each part's level there.)
3. **Clip gain.** `AudioClip` gets `gain_points`: (ticks from the clip's start,
   dB), straight lines in dB between them, held beyond the first and last, on
   top of the existing scalar `gain`. The track input applies it per frame
   (`ScheduledClip::level`); trimming a clip's start shifts the points. It is
   edited in the clip editor for unwarped clips: double-click the waveform to
   add a point, drag to move, double-click a point to remove.
4. **Crossfades (#203).** `AudioClip::crossfade` (`before`, `after` ticks) lives
   on the later of two clips butted within `BUTT_SECONDS`. The schedule extends
   the earlier clip past its end and the later one back before its start, as
   far as each file has audio and no more than half the clip; they fade
   equal-power across the whole stretch and the node plays both at once, each
   with its own stream and warp window. In the timeline a handle on the cut
   makes the crossfade (equal both sides, each stopping at its room), and its
   edges and middle resize and slide it.
5. **Not done.** The envelope has no editor for warped clips (it plays), a
   crossfade whose clips stop being butted is kept but ignored, and chained
   crossfades whose spans overlap on a short clip are not supported (the
   spans are held to half the clip so they cannot).
