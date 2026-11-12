# Document view links

The Web UI stores display hints in the query string alongside `open=/path/to/document.md`.
Buttons update the URL without a page reload; opening the URL or refreshing restores the view.

- `full=1`: full panel; absent: docked.
- `meta=1`: document metadata instead of file content.
- `raw=1`: raw document text instead of rendered Markdown.
- `chat=1`: document chat surface.
- `edit=1`: open the editor with the current saved document body (text documents only).

For example: `/entries?open=%2Fnotes%2Freadme.md&meta=1&full=1`
or `/entries?open=%2Fnotes%2Freadme.md&edit=1&full=1`.
Full/dock is independent of the content mode. Metadata overlays the selected file surface;
editing takes priority for editable text. Binary files cannot enter text edit/chat/raw modes.
Unrecognized values are ignored. Existing unrelated query parameters are preserved.
Hints apply to the current URL; ordinary document links without hints use the default preview. Saving or cancelling removes the edit hint.
Only display state is stored: unsaved draft contents are never included in the URL or restored
by refresh. Refreshing an editor reloads the saved server document.
