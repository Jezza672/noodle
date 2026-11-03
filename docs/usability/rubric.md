# Cognitive Dimensions rubric and human sessions

The scorecard (see [usability.md](../architecture/usability.md)) measures what a machine can count. This page covers the rest: six Cognitive Dimensions of Notations that need a person's judgment, and a short protocol for sessions with people. Results sit next to the scorecard: ratings below, session scores in [sessions/](sessions/README.md).

## The rubric

Rate each story's best `default` route on each dimension: **Good**, **Fair** or **Poor**, with one line saying why. Rate the route as the user lives it, not the code behind it. Two raters should agree within one step; if not, discuss and write down the reason.

| Dimension | Ask | Poor looks like |
|---|---|---|
| Hidden dependencies | Is it visible what depends on what? Can I see what a change will touch? | Changing a node alters something far away that nothing on screen links. |
| Hard mental operations | Must I hold a lot in my head, or compute something the app could? | Work out a key range or a sample rate by hand and type it. |
| Progressive evaluation | Can I hear or see the work while it is half built? | Nothing sounds until every wire is right. |
| Role expressiveness | Does each part say what it is for? | Three identical-looking nodes whose jobs differ. |
| Abstraction gradient | Can I group, template and reuse, and are the first steps simple? | Either everything is a primitive, or the only way out of a preset is to start again. |
| Consistency | Having learnt one thing, can I guess the next? | The same gesture does different things in two views. |

Rate once per route, and again when the route changes by more than a step or two. Keep ratings in the table below; an out-of-date rating is worse than none, so note the PR that last touched the route.

### Ratings for the MVP stories

First pass by the route author, from the routes as of #340. A person should check these in the first human session.

| Story | Hidden dependencies | Hard mental operations | Progressive evaluation | Role expressiveness | Abstraction gradient | Consistency |
|---|---|---|---|---|---|---|
| S01 comp (`partial`) | Fair: the comp lives in the clip's menu, effects on the track do not show which take feeds them | Good: swiping is direct | Good: audition while swiping | Good: lanes show takes | Fair: flatten is the only way to use a comp as plain clips | Good: same gestures as clip editing |
| S07 restructure (`blocked`) | Not rated: no sections yet (#329). To rate once built: do automation and tempo move with a section? | | | | | |
| S09 drum pattern (`partial`) | Good: the Drum Rack shows its pads | Fair: pitch numbers stand for pads until named | Good: plays while editing | Good: pads are labelled | Fair: no swing or choke at the rack, only through nodes | Fair: piano roll and rack use different note names |
| S12 multisample (`blocked`) | Not rated: no folder import (#331). By hand it would be a Note Split and one Sampler per file with ranges typed by hand: Poor on hard mental operations | | | | | |
| S19 drum bus (`partial`) | Fair: sidechain wiring is on the Dynamics node but the source is a wire far away | Fair: the user places pre or post fader by choosing where to tap | Good: meters while playing | Fair: a parallel bus is an ordinary Mix node | Good: the bus is a group | Good |
| S20 automation (`partial`) | Fair: points land on a lane in the timeline, not on the node | Good: ride a slider | Good: Touch and Latch while playing | Good | Fair: no relative trim over a range | Good |

## Human sessions

Run when a milestone lands or a route changes a lot, with three to five people who use a DAW but not Noodle. Fifteen minutes each is enough; a story list of three to six keeps the day short.

1. **Setup.** Fresh project, fresh app, screen and audio recorded with consent. Say that the app is being tested, not the person. No help, except for a hard stop (a crash, or ten minutes on one step).
2. **Per story.** Read the story text aloud (not the late changes). Ask them to think aloud. Note success (done check passes), time, actions, wrong turns and dead ends.
3. **Single Ease Question** straight after each story: "Overall, how easy or difficult was this task?" 1 (very difficult) to 7 (very easy).
4. **Late change.** Give one of the story's late changes. Same notes, same question.
5. **Optional, NASA-TLX.** After a story that felt heavy, the six TLX scales (mental, physical, temporal, performance, effort, frustration), 0 to 100 each, unweighted.
6. **At the end, SUS.** The ten System Usability Scale statements on a 1 to 5 scale, scored 0 to 100. SUS is for Noodle as a whole, not per story.
7. **Debrief.** Ask where they got lost and what they expected to find; write each as an issue with the story ID.

Store each session as one row per story in `sessions/results.csv` (columns in [sessions/README.md](sessions/README.md)), with a short note file next to it for the dead ends. Never store names; a participant is `P1`, `P2`.
