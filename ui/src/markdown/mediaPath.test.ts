import test from "node:test";
import assert from "node:assert/strict";
import { resolveMediaPath, mediaContentUrl } from "./mediaPath.ts";

test("stored media paths resolve from the containing document", () => {
  const doc = "/notes/session/readme.md";
  for (const [source, expected] of [
    ["photo.jpg", "/notes/session/photo.jpg"],
    ["./photo.jpg", "/notes/session/photo.jpg"],
    ["../photo.jpg", "/notes/photo.jpg"],
    ["/media/a.mp4", "/media/a.mp4"],
    ["../../../../root.wav", "/root.wav"],
    ["./my%20photo%23a.jpg", "/notes/session/my photo#a.jpg"],
  ]) assert.equal(resolveMediaPath(source, doc), expected);
  assert.equal(resolveMediaPath("./a.png", "/readme.md"), "/a.png");
});
test("unsafe schemes, hosts, controls and malformed encoding are not document paths", () => {
  for (const source of ["", "//evil.test/a", "https://evil.test/a", "javascript:alert(1)", "data:text/html,x", "#anchor", "%ZZ", "%2F%2Fevil.test/a", "a%00b", "a\\b"])
    assert.equal(resolveMediaPath(source, "/notes/a.md"), null);
});
test("content URL encodes each path segment without turning filenames into queries", () => {
  assert.equal(mediaContentUrl("/media/my photo#?.png"), "/api/content/media/my%20photo%23%3F.png");
});
