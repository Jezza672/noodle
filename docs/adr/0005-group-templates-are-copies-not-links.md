# Group templates are copies, not links

Using a group template from the library puts an unlinked copy of the group in the project. The copy remembers which template it came from, but editing or upgrading the template never changes it.

A project must stay self-contained, and a group must stay fully editable once created (the rule that synths are groups). A linked instance would change under a project whenever a system group improved, which would change how old projects sound. Presets are matched by the group's topology, not by its origin, so a copy that was edited into a new shape stops being offered the old presets and is simply a new group.
