# Human session results

Scores from sessions run with the protocol in [../rubric.md](../rubric.md). None yet.

`results.csv`, one row per participant and story (create it with the first session):

```
date,build,participant,story,late_change,success,seconds,actions,wrong_turns,ease_1_to_7,tlx_0_to_100,notes
```

`sus.csv`, one row per participant and build:

```
date,build,participant,sus_0_to_100
```

`build` is the commit of `main` that was tested. Dead ends and lost moments go in `notes` and, if they are worth a fix, in an issue named after the story ID.
