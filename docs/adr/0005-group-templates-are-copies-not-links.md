# Group templates are copies, not links

Using a group template from the library puts an unlinked copy of the group in the project. The copy remembers which template it came from, but editing or upgrading the template never changes it.

A project must stay self-contained, and a group must stay fully editable once created (the rule that synths are groups). A linked instance would change under a project whenever a system group improved, which would change how old projects sound. Presets are matched by the group's topology, not by its origin, so a copy that was edited into a new shape stops being offered the old presets and is simply a new group.

The cost is that fixes and improvements to a system template never reach groups already made from it, and nothing tells the user a newer version exists. The one way to get a newer version is an explicit "Update from template" on the group (right-click), which replaces what is inside, keeps the wires at its ports, and is one undo step.
