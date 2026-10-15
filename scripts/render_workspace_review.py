#!/usr/bin/env python3
"""Rasterize the actual egui meshes exported by the ignored workspace review test.

Requires Pillow. Solid triangles and atlas text use a simple CPU renderer;
this reviews layout and clipping, not native GPU antialiasing or capture.
"""
import argparse
import json
import math
from pathlib import Path
from PIL import Image, ImageDraw

def render(source):
    data = json.loads(source.read_text())
    width, height = data['size']
    aw, ah = data['atlas_size']
    atlas = data['atlas']
    image = Image.new('RGB', (width, height), (0, 0, 0))
    for mesh in data['meshes']:
        vertices = mesh['vertices']
        clip = mesh['clip']
        indices = mesh['indices']
        for i in range(0, len(indices), 3):
            a, b, c = [vertices[index] for index in indices[i:i + 3]]
            x0 = max(0, math.floor(clip[0]), math.floor(min(a[0], b[0], c[0])))
            y0 = max(0, math.floor(clip[1]), math.floor(min(a[1], b[1], c[1])))
            x1 = min(width, math.ceil(clip[2]), math.ceil(max(a[0], b[0], c[0])))
            y1 = min(height, math.ceil(clip[3]), math.ceil(max(a[1], b[1], c[1])))
            if x1 <= x0 or y1 <= y0:
                continue
            if a[2:] == b[2:] == c[2:]:
                color = a[4]
                alpha = color[3]
                if alpha == 0:
                    continue
                color = tuple((min(255, round(v * 255 / alpha)) for v in color[:3])) + (alpha,)
                patch = image.crop((x0, y0, x1, y1))
                ImageDraw.Draw(patch, 'RGBA').polygon([(v[0] - x0, v[1] - y0) for v in [a, b, c]], fill=color)
                image.paste(patch, (x0, y0))
                continue
            denominator = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1])
            if abs(denominator) < 1e-09:
                continue
            pixels = image.load()
            for y in range(y0, y1):
                for x in range(x0, x1):
                    px, py = (x + 0.5, y + 0.5)
                    wa = ((b[1] - c[1]) * (px - c[0]) + (c[0] - b[0]) * (py - c[1])) / denominator
                    wb = ((c[1] - a[1]) * (px - c[0]) + (a[0] - c[0]) * (py - c[1])) / denominator
                    wc = 1 - wa - wb
                    if min(wa, wb, wc) < -1e-06:
                        continue
                    u = wa * a[2] + wb * b[2] + wc * c[2]
                    v = wa * a[3] + wb * b[3] + wc * c[3]
                    tx = min(aw - 1, max(0, int(u * aw)))
                    ty = min(ah - 1, max(0, int(v * ah)))
                    texture = atlas[ty * aw + tx]
                    color = [(wa * a[4][k] + wb * b[4][k] + wc * c[4][k]) * texture[k] / 255 for k in range(4)]
                    alpha = color[3] / 255
                    if alpha == 0:
                        continue
                    previous = pixels[x, y]
                    pixels[x, y] = tuple((min(255, max(0, round(color[k] + previous[k] * (1 - alpha)))) for k in range(3)))
    target = source.with_suffix('.png')
    image.save(target)
    return target

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', nargs='?', default='/tmp/dartmoor-ui-review')
    root = Path(parser.parse_args().directory)
    for source in sorted(root.glob('*.json')):
        print(render(source).name, flush=True)
if __name__ == '__main__':
    main()
