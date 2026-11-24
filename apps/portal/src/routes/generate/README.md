# Generate form

`#/generate?workflow=<id>` opens a manifest-driven form. With no workflow
selected, the page offers a picker. A preset applies only the keys it supplies;
it does not replace other edits. Review validates with the server but does not submit a run.

The form separates editable values from the reviewed parameter snapshot.
Fields distinguish absent values from empty text, zero and false, retain raw
numeric drafts, and record whether a value came from a default, preset or user.
Locked seeds use the entered integer; randomised seeds are chosen on review
and live only in that snapshot. Edits invalidate the snapshot. Integers beyond
JavaScript's safe range are kept as `bigint` and require exact JSON support in
the browser. Whole-valued floats retain their JSON representation for enum
matching. Browsers that cannot read the original JSON number token must be
updated before reviewing or submitting; Workflow Detail still checks backend
node availability.
Steps and suggested options are control hints, not binding rules. Imported
multiline text remains visible in a multiline editor even when the manifest
uses a single-line text widget.

Reference images are previewed from the selected local file after upload. The
server returns metadata but does not serve stored image bytes, so a reference
supplied by a manifest or preset can show its ID but not its preview. See the [portal share link contract](../../../../../docs/architecture.md#generate-share-links-and-remix) for restoration and Remix.

Requiredness and expected output types use the server’s effective values. The
workflow detail API identifies these as declared, defaulted or unknown. An
optional parameter without a manifest default stays unset.
