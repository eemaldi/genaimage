# Generate form

`#/generate?workflow=<id>` opens a manifest-driven form. With no workflow
selected, the page offers a picker. A preset applies only the keys it supplies;
it does not replace other edits. The review is local and does not submit a run.

The form separates editable values from the reviewed parameter snapshot.
Fields distinguish absent values from empty text, zero and false, retain raw
numeric drafts, and record whether a value came from a default, preset or user.
Locked seeds use the entered integer; randomised seeds are chosen on review
and live only in that snapshot. Edits invalidate the snapshot. Integers beyond
JavaScript's safe range are kept as `bigint` and require exact JSON support in
the browser. Whole-valued floats retain their JSON representation for enum
matching. Steps and suggested options are control hints, not binding rules.

Reference images are previewed from the selected local file after upload. The
server returns metadata but does not serve stored image bytes, so a reference
supplied by a manifest or preset can show its ID but not its preview. Server
binding validation, run submission and shareable form state are not wired yet.
