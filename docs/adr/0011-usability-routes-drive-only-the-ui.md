# Usability routes drive only the UI

A usability route is a scripted run of one user story through the app. It reaches the app only through real pointer and key events, the same input a person produces. It never calls into `Session` or the graph to make a change, so a route cannot game its score by taking a shortcut no user has. The action log that the scorer reads is the list of those events.

Routes may read state, for example to check that a story's goal is met (the "done check"). Reading does not change the app and is not counted as an action.

Rejected: letting routes call `Session` commands directly. It is simpler to write, but it measures the command API and not the interface, and any missing UI would go unnoticed.
