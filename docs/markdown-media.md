# Embedding media in Markdown

Tabularium stores binary documents alongside text. Upload the media first using `PUT /api/content/<path>` with the correct Content-Type, the CLI, or MCP `put_binary`.

Use the normal Markdown image syntax for all three supported media kinds:

```markdown
![Photo caption](/media/photo.jpg)
![](./clip.mp4)
![Voice recording](../audio/voice.ogg)
```

The caption is optional: `![](photo.jpg)` works and displays no caption row. For images the text also serves as alt text. Preview and document chat choose image, video or audio by the stored MIME type, not the extension. A video named `clip.bin` with MIME `video/mp4` is still a video. Missing or incorrect MIME types do not trigger extension guessing.

## Paths

For a Markdown document `/notes/session/readme.md`:

| Source | Stored document |
| --- | --- |
| `/media/photo.jpg` | `/media/photo.jpg` |
| `photo.jpg` | `/notes/session/photo.jpg` |
| `./photo.jpg` | `/notes/session/photo.jpg` |
| `../photo.jpg` | `/notes/photo.jpg` |

Dot segments are normalized at the Tabularium root. Spaces can be percent encoded or written with angle brackets, e.g. `![](<./my photo.jpg>)`. Paths are document paths, not operating-system file paths. To avoid ambiguity, percent-encode literal `?` and `#` in filenames. Ordinary `[Download](./clip.mp4)` remains a link.

## Display and access

- Images are bounded to the document width and open full-size in a new tab on click.
- Video and audio have native controls, metadata preload and no autoplay.
- Media reads retain normal authentication and document ACL checks. No public share link is minted and no credentials are placed in URLs.
- Missing/denied/unplayable media use a generic unavailable placeholder. Unsupported MIME types use an unsupported-type placeholder.
- PDFs and other attachments are not embedded; use an ordinary link instead.
- Supported raster images: JPEG, PNG, GIF, WebP, AVIF. Video: MP4, WebM, Ogg. Audio: Ogg/Opus, MP3, MP4 audio, WAV, WebM audio, FLAC. Actual playback depends on browser codecs.
- Existing HTTP(S) image references remain external images; this feature does not fetch remote media through the server.
