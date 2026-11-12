import { useEffect, useState } from "react";
import { getDocument } from "../api/client";
import { mediaContentUrl, resolveMediaPath } from "./mediaPath";

const images = new Set(["image/jpeg", "image/png", "image/gif", "image/webp", "image/avif"]);
const videos = new Set(["video/mp4", "video/webm", "video/ogg"]);
const audios = new Set(["audio/ogg", "audio/opus", "audio/mpeg", "audio/mp4", "audio/wav", "audio/x-wav", "audio/webm", "audio/flac"]);

/** Embedded documents use the same metadata ACL and cookie-authenticated content route as attachments. */
export function MarkdownMedia({ source, caption, documentPath }: {
  source: string; caption: string; documentPath: string;
}) {
  const path = resolveMediaPath(source, documentPath);
  // A key on the caller remounts media when source/base changes, stopping old playback.
  const [mime, setMime] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let active = true;
    if (path) {
      void getDocument(path).then(doc => {
        if (active) setMime((doc.mime_type ?? "").split(";", 1)[0].trim().toLowerCase());
      }).catch(() => { if (active) setFailed(true); });
    }
    return () => { active = false; };
  }, [path]);
  const external = /^https?:\/\//i.test(source);
  if (!path && external) return <img src={source} alt={caption} loading="lazy" style={{maxWidth:"100%"}} />;
  if (!path || failed) return <span role="status" data-testid="media-unavailable">Media unavailable{caption ? `: ${caption}` : ""}</span>;
  const url = mediaContentUrl(path);
  const bounds = { maxWidth: "100%", maxHeight: "70vh", objectFit: "contain" as const };
  let media;
  if (mime === null) media = <span role="status">Loading media…</span>;
  else if (images.has(mime)) media = <a href={url} target="_blank" rel="noopener noreferrer" aria-label={`Open image: ${caption || path}`}><img src={url} alt={caption} loading="lazy" style={bounds} onError={() => setFailed(true)} /></a>;
  else if (videos.has(mime)) media = <video src={url} aria-label={caption || path} controls preload="metadata" style={bounds} onError={() => setFailed(true)} />;
  else if (audios.has(mime)) media = <audio src={url} aria-label={caption || path} controls preload="metadata" style={{width:"min(100%, 480px)"}} onError={() => setFailed(true)} />;
  else media = <span>Unsupported media type{caption ? `: ${caption}` : ""}</span>;
  // Span wrappers stay valid even when Markdown places an embed inside a paragraph.
  return <span data-testid="markdown-media" data-path={path} style={{display:"inline-flex",flexDirection:"column",maxWidth:"100%",gap:"0.35rem",marginBlock:"0.5rem"}}>{media}{caption && mime !== null ? <span>{caption}</span> : null}</span>;
}
