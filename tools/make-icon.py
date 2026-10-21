"""Draws icon.png: a small KVM box with a cable to a monitor, on a
transparent background. Own artwork; run from the repository root."""
from PIL import Image, ImageDraw

S = 4  # drawn at 4x and scaled down for smooth edges
img = Image.new("RGBA", (256 * S, 256 * S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
teal, dark, light = (20, 150, 140, 255), (16, 64, 72, 255), (210, 245, 240, 255)


def box(x0, y0, x1, y1):
    return [v * S for v in (x0, y0, x1, y1)]


d.rounded_rectangle(box(24, 20, 232, 150), radius=16 * S, fill=dark)       # monitor
d.rounded_rectangle(box(38, 34, 218, 136), radius=6 * S, fill=teal)        # screen
d.rectangle(box(118, 150, 138, 168), fill=dark)                            # stand
d.rounded_rectangle(box(88, 166, 168, 176), radius=4 * S, fill=dark)
# The KVM box in front, with its own little display and a status light.
d.rounded_rectangle(box(56, 190, 200, 240), radius=12 * S, fill=dark)
d.rounded_rectangle(box(70, 200, 150, 230), radius=4 * S, fill=teal)
d.ellipse(box(170, 208, 184, 222), fill=light)
# A cable from the box up to the monitor.
d.line([(v[0] * S, v[1] * S) for v in [(200, 215), (226, 215), (226, 160), (210, 150)]], fill=dark, width=8 * S, joint="curve")

img.resize((256, 256), Image.LANCZOS).save("icon.png")
