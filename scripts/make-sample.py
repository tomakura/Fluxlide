"""Draws the pictures of the sample slideshow (assets/sample/*.jpg).

    python scripts/make-sample.py

They are abstract: soft gradients with the three cards of the Fluxlide mark,
so the captions (in assets/sample/sample.json) can say what to do. Needs
Pillow. The pictures are part of Fluxlide and under its MIT license.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

SIZE = (1920, 1080)
OUT = Path(__file__).resolve().parent.parent / "assets" / "sample"

# Top-left and bottom-right colour of each picture's sky, and where the
# cards sit (as a fraction of the picture).
PICTURES = [
    ((19, 33, 59), (232, 85, 58), (0.60, 0.40)),
    ((24, 52, 92), (120, 190, 220), (0.32, 0.40)),
    ((46, 26, 70), (240, 150, 110), (0.64, 0.38)),
    ((14, 60, 66), (200, 220, 150), (0.36, 0.40)),
    ((32, 30, 48), (255, 190, 120), (0.56, 0.38)),
    ((20, 40, 80), (220, 120, 160), (0.42, 0.38)),
]


def gradient(top_left, bottom_right):
    w, h = SIZE
    small = Image.new("RGB", (64, 36))
    for y in range(36):
        for x in range(64):
            t = (x / 63 + y / 35) / 2
            small.putpixel(
                (x, y),
                tuple(round(a + (b - a) * t) for a, b in zip(top_left, bottom_right)),
            )
    return small.resize((w, h), Image.BICUBIC)


def cards(base, at):
    """The mark's three cards, large and soft, from the back to the front."""
    w, h = SIZE
    layer = Image.new("RGBA", SIZE, (0, 0, 0, 0))
    draw = ImageDraw.Draw(layer)
    card_w, card_h = int(w * 0.26), int(w * 0.26 * 24 / 34)
    step = int(w * 0.06)
    x0, y0 = int(w * at[0]) - card_w // 2 - step, int(h * at[1]) - card_h // 2 - step
    for index, (colour, alpha) in enumerate(
        [((255, 255, 255), 50), ((255, 255, 255), 90), ((232, 85, 58), 220)]
    ):
        x, y = x0 + index * step, y0 + index * step
        draw.rounded_rectangle(
            (x, y, x + card_w, y + card_h), radius=int(card_w * 4 / 34), fill=(*colour, alpha)
        )
    shadow = layer.filter(ImageFilter.GaussianBlur(24))
    out = base.convert("RGBA")
    out.alpha_composite(Image.new("RGBA", SIZE, (0, 0, 0, 0)))
    out.alpha_composite(shadow.point(lambda v: v // 3))
    out.alpha_composite(layer)
    return out.convert("RGB")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for index, (top_left, bottom_right, at) in enumerate(PICTURES, start=1):
        picture = cards(gradient(top_left, bottom_right), at)
        picture.save(OUT / f"{index:02}.jpg", quality=86, optimize=True, progressive=False)
        print(OUT / f"{index:02}.jpg")


if __name__ == "__main__":
    main()
