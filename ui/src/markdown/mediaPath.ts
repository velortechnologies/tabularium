/** Resolve a stored media path relative to the containing Markdown document. */
export function resolveMediaPath(source: string, documentPath: string): string | null {
  const raw = source.trim();
  if (!raw || raw.startsWith("//") || raw.startsWith("#") || /^[a-z][a-z0-9+.-]*:/i.test(raw)) return null;
  let decoded: string;
  try { decoded = decodeURIComponent(raw); } catch { return null; }
  if (decoded.startsWith("//") || decoded.includes("\\") || Array.from(decoded).some(char => char.charCodeAt(0) < 32)) return null;
  const base = documentPath.slice(0, documentPath.lastIndexOf("/"));
  const parts = (decoded.startsWith("/") ? decoded : `${base}/${decoded}`).split("/");
  const result: string[] = [];
  for (const part of parts) {
    if (!part || part === ".") continue;
    if (part === "..") { result.pop(); continue; }
    result.push(part);
  }
  return result.length ? `/${result.join("/")}` : null;
}

export function mediaContentUrl(path: string): string {
  return `/api/content${path.split("/").map(encodeURIComponent).join("/")}`;
}
