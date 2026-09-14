"""Deterministic pictures and a screenshot oracle independent of icat output."""
import hashlib
import struct
import unicodedata
import zlib

PLACEHOLDER = "\U0010eeee"


def colours(number):
    return [tuple(48 + c % 176 for c in hashlib.sha256(f"{number}:{i}".encode()).digest()[:3])
            for i in range(9)]


def png(number, width, height, texture=False):
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    palette = colours(number)
    stripes = [b"\0" + b"".join(bytes(palette[y * 3 + x]) *
                (((x + 1) * width + 2) // 3 - (x * width + 2) // 3) for x in range(3))
               for y in range(3)]
    if texture:
        # Coarse, low-amplitude texture survives thumbnail resampling and
        # exercises multi-packet streams; nine flat tiles compress too easily.
        grid_w, grid_h = min(width, 180), min(height, 120)
        stripes = []
        for y in range(grid_h):
            row = bytearray(b"\0")
            for x in range(grid_w):
                noise = hashlib.sha256(f"{number}:{x}:{y}".encode()).digest()
                colour = bytes(c + noise[i] % 17 - 8 for i, c in enumerate(palette[(y * 3 // grid_h) * 3 + x * 3 // grid_w]))
                row.extend(colour * (((x + 1) * width + grid_w - 1) // grid_w - (x * width + grid_w - 1) // grid_w))
            stripes.append(bytes(row))
    pixels = b"".join(stripes[min(y * len(stripes) // height, len(stripes) - 1)] for y in range(height))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(pixels, 6)) + chunk(b"IEND", b""))


def rectangles(screen, top, bottom, cell):
    """Locate image cells in terminal text, ignoring their combining marks.

    The text supplies only the region to inspect; it cannot make the pixel
    assertion pass. It fails on missing images, literal placeholders, stale
    pictures and distorted aspect ratios.
    """
    boxes = {}
    for y, line in enumerate(screen.splitlines()[top:bottom], top):
        chars = [c for c in line if unicodedata.category(c) not in ("Mn", "Me", "Cf")]
        x = 0
        while x < len(chars):
            if chars[x] != PLACEHOLDER:
                x += 1
                continue
            start = x
            while x < len(chars) and chars[x] == PLACEHOLDER:
                x += 1
            key = next((k for k, b in boxes.items() if b[0] == start and b[3] == y), (start, y))
            prior = boxes.get(key, (start, y, x, y))
            boxes[key] = (start, prior[1], max(x, prior[2]), y + 1)
    return [(x * cell[0], y * cell[1], right * cell[0], bottom * cell[1])
            for x, y, right, bottom in boxes.values()]


def match(image, box, number, size):
    """Require a filled nine-colour spatial pattern at the source aspect ratio.

    Derive bounds from the black-backed screenshot, allow a few edge pixels
    for resizing/JPEG rounding, and check 900 positions across all nine tiles.
    Colour diversity alone cannot distinguish a picture from corrupted text.
    """
    width, height, channels, pixels = (image[k] for k in ("width", "rows", "channels", "pixels"))
    left, top, right, bottom = box
    if not (0 <= left < right <= width and 0 <= top < bottom <= height):
        return {"ok": False, "reason": "image region is outside the screen", "box": box}
    points = []
    for y in range(top, bottom):
        for x in range(left, right):
            offset = (y * width + x) * channels
            if max(pixels[offset:offset + 3]) > 32:
                points.append((x, y))
    if not points:
        return {"ok": False, "reason": "image region is blank", "box": box}
    x0, x1 = min(p[0] for p in points), max(p[0] for p in points) + 1
    y0, y1 = min(p[1] for p in points), max(p[1] for p in points) + 1
    drawn_w, drawn_h = x1 - x0, y1 - y0
    ratio_error = abs(drawn_w / drawn_h / (size[0] / size[1]) - 1)
    palette = colours(number)
    good = 0
    for row in range(30):
        y = y0 + min(drawn_h - 1, int((row + 0.5) * drawn_h / 30))
        for column in range(30):
            x = x0 + min(drawn_w - 1, int((column + 0.5) * drawn_w / 30))
            offset = (y * width + x) * channels
            expected = palette[(row // 10) * 3 + column // 10]
            good += max(abs(a - b) for a, b in zip(pixels[offset:offset + 3], expected)) <= 18
    # Literal glyphs leave holes; a tiny valid patch cannot stand in for the image.
    coverage = len(points) / (drawn_w * drawn_h)
    occupancy = drawn_w * drawn_h / ((right - left) * (bottom - top))
    return {"ok": good >= 870 and ratio_error <= 0.04 and coverage >= 0.95 and occupancy >= 0.65,
            "matched": good, "samples": 900, "aspect_error": round(ratio_error, 4),
            "coverage": round(coverage, 4), "occupancy": round(occupancy, 4),
            "bounds": [x0, y0, x1, y1], "fixture": number}


def swatches(image, row, left, colours, cell):
    """All sixteen displayed swatches must paint their expected RGB values."""
    if len(colours) != 16 or len(set(colours)) < 2:
        return False
    for i, expected in enumerate(colours):
        for dx, dy in ((1, 0.25), (2, 0.5), (3, 0.75)):
            x, y = int((left + i * 4 + dx) * cell[0]), int((row + dy) * cell[1])
            if not (0 <= x < image["width"] and 0 <= y < image["rows"]):
                return False
            offset = (y * image["width"] + x) * image["channels"]
            if max(abs(a - b) for a, b in zip(image["pixels"][offset:offset + 3], expected)) > 4:
                return False
    return True
