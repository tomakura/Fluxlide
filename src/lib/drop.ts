// Where a file drop reported by the webview lands, in CSS pixels.

/// The webview reports drop positions relative to itself, typed as physical
/// pixels. Only Windows (WebView2) gives physical pixels; macOS (WKWebView)
/// and Linux (WebKitGTK) give points, which are already CSS pixels.
export function dropPoint(
  position: { x: number; y: number },
  windows: boolean,
  pixelRatio: number,
): { x: number; y: number } {
  const scale = windows ? pixelRatio : 1;
  return { x: position.x / scale, y: position.y / scale };
}

export const isWindows = () => navigator.userAgent.includes("Windows");
