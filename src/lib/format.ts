// How the definition is put into words and numbers on screen.

import { fileUrl } from "./api";
import type { Asset } from "./bindings/Asset";
import type { Corner } from "./bindings/Corner";
import type { Direction } from "./bindings/Direction";
import type { Fit } from "./bindings/Fit";
import type { Motion } from "./bindings/Motion";
import type { ShowDefinition } from "./bindings/ShowDefinition";
import type { Slide } from "./bindings/Slide";
import type { TransitionChoice } from "./bindings/TransitionChoice";

export const PHOTO_EXTENSIONS = ["jpg", "jpeg", "png", "heic", "heif"];
export const VIDEO_EXTENSIONS = ["mp4", "m4v", "mov"];
export const MEDIA_FILTER = {
  name: "写真と動画",
  extensions: [...PHOTO_EXTENSIONS, ...VIDEO_EXTENSIONS],
};

export const MUSIC_FILTER = {
  name: "音楽",
  extensions: ["mp3", "m4a", "aac", "wav", "flac", "ogg", "oga", "opus", "aif", "aiff", "wma"],
};

export const CORNERS: Record<Corner, string> = {
  topLeft: "左上",
  topRight: "右上",
  bottomLeft: "左下",
  bottomRight: "右下",
};

/// The slow zooms offered, by name. "still" is a slide that stays still
/// even when the look zooms every photo.
export const ZOOMS: Record<"in" | "out" | "still", Motion> = {
  in: { fromScale: 1, toScale: 1.15, fromOffset: [0, 0], toOffset: [0, 0] },
  out: { fromScale: 1.15, toScale: 1, fromOffset: [0, 0], toOffset: [0, 0] },
  still: { fromScale: 1, toScale: 1, fromOffset: [0, 0], toOffset: [0, 0] },
};

export function zoomOf(motion: Motion | null): "in" | "out" | "still" | "none" {
  if (!motion) return "none";
  if (motion.toScale === motion.fromScale) return "still";
  return motion.toScale > motion.fromScale ? "in" : "out";
}

/// "10秒", "1.5秒", "1分20秒".
export function seconds(ms: number): string {
  const secs = Math.round(ms / 100) / 10;
  if (secs < 60) return `${secs}秒`;
  const whole = Math.round(secs);
  const minutes = Math.floor(whole / 60);
  const rest = whole - minutes * 60;
  return rest ? `${minutes}分${rest}秒` : `${minutes}分`;
}

/// "820 KB", "12 MB", "1.4 GB".
export function fileSize(bytes: number): string {
  if (bytes < 1e6) return `${Math.max(1, Math.round(bytes / 1e3))} KB`;
  if (bytes < 1e9) return `${Math.round(bytes / 1e6)} MB`;
  return `${(bytes / 1e9).toFixed(1)} GB`;
}

/// "1分未満", "約3分", "約1時間20分" for a rough length of time.
export function roughly(secs: number): string {
  const minutes = Math.round(secs / 60);
  if (minutes < 1) return "1分未満";
  if (minutes < 60) return `約${minutes}分`;
  const rest = minutes % 60;
  return `約${Math.floor(minutes / 60)}時間${rest ? `${rest}分` : ""}`;
}

/// "0:05", "12:34", "1:02:03" for a running time.
export function clock(totalSecs: number): string {
  const secs = Math.floor(totalSecs);
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = String(secs % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

export const DIRECTIONS: Record<Direction, string> = {
  left: "左へ",
  right: "右へ",
  up: "上へ",
  down: "下へ",
};

export function transitionName(choice: TransitionChoice, show: ShowDefinition): string {
  switch (choice.kind) {
    case "cut":
      return "カット";
    case "fade":
      return "フェード";
    case "slide":
      return `スライド（${DIRECTIONS[choice.direction]}）`;
    case "wipe":
      return `ワイプ（${DIRECTIONS[choice.direction]}）`;
    case "zoom":
      return "ズーム";
    case "paw":
      return "肉球";
    case "custom":
      return show.look.customTransitions.find((t) => t.id === choice.id)?.name ?? "カスタム";
  }
}

export const FITS: Record<Fit, string> = {
  blur: "ぼかし背景",
  contain: "黒帯",
  cover: "トリミング",
};

/// Length of a transition when neither its slide nor the look sets one
/// (the engine's `TransitionChoice::default_ms`).
export function ownLength(choice: TransitionChoice): number {
  switch (choice.kind) {
    case "cut":
      return 0;
    case "paw":
      return 5000;
    default:
      return 1000;
  }
}

/// What the slide actually uses: its own settings, or the look's.
export function effective(slide: Slide, show: ShowDefinition) {
  const transition = slide.transition ?? show.look.transition;
  return {
    durationMs: slide.durationMs ?? show.look.photoMs,
    transition,
    transitionMs:
      transition.kind === "cut" ? 0 : (slide.transitionMs ?? show.look.transitionMs ?? ownLength(transition)),
    fit: slide.framing.fit ?? show.look.fit,
  };
}

/// The thumbnail made when `asset` was imported.
export function thumbnailUrl(mediaDir: string, asset: Asset): string {
  const separator = mediaDir.includes("\\") ? "\\" : "/";
  return fileUrl([mediaDir, "thumbs", `${asset.file}.jpg`].join(separator));
}

/// How long a slide is on screen, as shown in the list.
export function slideLength(slide: Slide, asset: Asset | undefined, show: ShowDefinition): string {
  if (asset?.kind === "video") {
    if (asset.durationMs === null) return "動画";
    const end = Math.min(slide.video.endMs ?? asset.durationMs, asset.durationMs);
    return `動画 ${seconds(Math.max(0, end - slide.video.startMs))}`;
  }
  return seconds(effective(slide, show).durationMs);
}
